//! Instant replay session: WGC capture -> pacing -> packet encoder -> hybrid
//! replay buffer, all driven from the recorder worker thread.
//!
//! The worker never performs file I/O in steady state. The only exception is
//! `ReplayBuffer::new`, which creates the SSD session directory once when the
//! first packet defines the layout. Spilling to the SSD runs on the buffer's
//! own spill thread; clip folders, names and files are handled by the exporter
//! thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use clipper_capture::windows::{CaptureSession, CapturedFrame};
use clipper_core::pipeline::{FramePacer, RateMeter};
use clipper_core::recording::{CaptureSource, RecorderEndpoint, ReplayOptions, ReplayStatus};
use clipper_encoder::backends::media_foundation::{EncoderConfig, MediaFoundation, SubmitOutcome};
use clipper_encoder::backends::packet_encoder::PacketEncoder;
use clipper_platform::windows::D3DContext;
use clipper_replay::EncodedPacket;
use clipper_replay::disk::DiskConfig;
use clipper_replay::error::ReplayError;
use clipper_replay::hybrid::{ReplayBuffer, ReplayConfig, ReplaySnapshot};

use crate::logic::{
    Counters, SlotTimeline, fit_output_size, repeat_lag_ticks, replay_failure_text, replay_status,
    replay_warning, valid_replay_duration,
};
use crate::recording_audio::{AudioPlan, AudioReporter, RecordingAudio};
use crate::windows_worker::{encoder_message, round_up_even, signal_handler};

/// Longest wait for a capture/audio wake. Short on purpose: on a static scene
/// WGC is silent and the pump itself must keep the 60 fps timeline moving.
pub const WAKE_WAIT: Duration = Duration::from_millis(30);
/// Real frames moved per service iteration (keeps audio and commands live).
const MAX_FRAMES_PER_PUMP: usize = 4;
/// Repeated frames submitted per service iteration.
const MAX_REPEATS_PER_PUMP: usize = 16;
/// Longest wait for the first captured frame before the session gives up.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);
/// Audio is mixed this far behind real time (same as manual recording).
const AUDIO_LAG_TICKS: i64 = 800_000;
/// Audio/video clock gap that cannot be recovered.
const MAX_CLOCK_GAP_TICKS: i64 = 10_000_000;

/// A real frame waiting for the repeats that fill the slots before it.
struct PendingFrame {
    frame: CapturedFrame,
    slot: i64,
}

/// Internal health counters of a session (logged when it stops).
#[derive(Clone, Copy, Debug, Default)]
pub struct ReplayDiagnostics {
    /// Frames the pacer skipped (source faster than the target rate).
    pub pacer_skipped: u64,
    /// Frames discarded because their slot was already filled.
    pub late_discarded: u64,
    /// Audio frames lost to ring overflow or the mixing window.
    pub audio_glitch_frames: u64,
    /// Packets the buffer rejected (timestamps not increasing).
    pub invalid_packets: u64,
    /// Video slots filled with a repeat of the previous frame (static scene,
    /// dropped or late capture frames).
    pub repeated_frames: u64,
    /// Audio packets re-anchored to their QPC time by the mixer (drift or
    /// discontinuity) and the largest such correction in 48 kHz frames.
    pub audio_resyncs: u64,
    pub audio_max_resync_frames: u64,
}

pub struct ReplaySession {
    // Field order is drop order: release the held frame and stop capture
    // first, then the encoder, audio and the buffer, and the device last.
    pending: Option<PendingFrame>,
    capture: CaptureSession,
    encoder: PacketEncoder,
    audio: Option<RecordingAudio>,
    audio_reporter: AudioReporter,
    buffer: Option<ReplayBuffer>,
    /// Keeps the device alive until everything above is released.
    _d3d: D3DContext,
    config: ReplayConfig,
    origin: i64,
    pacer: FramePacer,
    meter: RateMeter,
    counters: Counters,
    started: Instant,
    /// Constant frame rate grid of the video.
    timeline: SlotTimeline,
    /// Session time of the first submitted frame.
    video_start: Option<i64>,
    diagnostics: ReplayDiagnostics,
    packets: Vec<EncodedPacket>,
    pub wake: Receiver<()>,
    source_closed: Arc<AtomicBool>,
    encoder_name: String,
}

impl ReplaySession {
    pub fn start(
        mf: &MediaFoundation,
        source: &CaptureSource,
        options: &ReplayOptions,
    ) -> Result<Self, String> {
        let recording = &options.recording;
        let d3d = D3DContext::new()
            .map_err(|error| format!("could not create the GPU device: {error}"))?;

        let (wake_tx, wake) = sync_channel::<()>(1);
        let source_closed = Arc::new(AtomicBool::new(false));
        let capture = CaptureSession::start(
            &d3d,
            source,
            signal_handler(wake_tx.clone(), Arc::clone(&source_closed)),
        )
        .map_err(|error| replay_failure_text(&error.to_string()))?;
        // Optional (Windows 11 24H2+); older builds keep the WGC default.
        let _ = capture.set_target_fps(recording.fps);

        let (width, height) = capture.size();
        let (output_width, output_height) =
            fit_output_size((width, height), (recording.width, recording.height));
        let config = EncoderConfig {
            input_width: round_up_even(width),
            input_height: round_up_even(height),
            output_width,
            output_height,
            fps: recording.fps,
            bitrate_mbps: recording.bitrate_mbps,
            profile: recording.profile,
        };
        // The plan fixes the packet topology (mix + isolated tracks) for the
        // whole session: sources that start late or vanish render silence in
        // their track instead of changing the layout of buffered footage.
        let plan = AudioPlan::for_session(&recording.audio, Some(source));
        let audio_reporter = AudioReporter::new(plan.idle_states());
        let encoder = PacketEncoder::create_with_labels(mf, &d3d, &config, &plan.labels())
            .map_err(|error| replay_failure_text(&encoder_message(&error)))?;
        let encoder_name = encoder.name().to_owned();

        // Initialize expensive GPU/codec resources before audio starts,
        // otherwise hardware encoder discovery can overflow the audio rings.
        let mut audio = RecordingAudio::start(plan, wake_tx)
            .map_err(|message| replay_failure_text(&message))?;
        if let Some(audio) = &mut audio {
            audio.set_tolerant(true);
        }
        let origin = audio.as_ref().map_or_else(
            clipper_audio::windows::qpc_now_ticks,
            RecordingAudio::origin,
        );

        let interval = clipper_core::pipeline::FramePacer::new(recording.fps).interval();
        Ok(Self {
            pending: None,
            capture,
            encoder,
            audio,
            audio_reporter,
            buffer: None,
            _d3d: d3d,
            config: ReplayConfig {
                duration: options.duration,
                memory_limit_bytes: usize::try_from(options.memory_limit_bytes)
                    .unwrap_or(usize::MAX),
                disk: options.cache_directory.clone().map(|root| DiskConfig {
                    root,
                    max_bytes: options.disk_limit_bytes,
                }),
            },
            origin,
            pacer: FramePacer::with_origin(recording.fps, origin),
            meter: RateMeter::new(),
            counters: Counters::default(),
            started: Instant::now(),
            timeline: SlotTimeline::new(interval),
            video_start: None,
            diagnostics: ReplayDiagnostics::default(),
            packets: Vec::with_capacity(64),
            wake,
            source_closed,
            encoder_name,
        })
    }

    pub fn encoder_name(&self) -> &str {
        &self.encoder_name
    }

    /// Applies a new replay length live. Returns `false` for an invalid one.
    pub fn set_duration(&mut self, duration: Duration) -> bool {
        if !valid_replay_duration(duration) {
            return false;
        }
        self.config.duration = duration;
        if let Some(buffer) = &mut self.buffer {
            buffer.set_duration(duration);
        }
        true
    }

    /// Live gain of one source; ignored when the session does not capture it.
    pub fn set_audio_gain(&mut self, source: u32, gain_percent: u16) {
        if let Some(audio) = &mut self.audio {
            audio.set_gain(source, gain_percent);
        }
    }

    pub fn set_audio_noise_gate(
        &mut self,
        source: u32,
        settings: clipper_core::recording::NoiseGateSettings,
    ) {
        if let Some(audio) = &mut self.audio {
            audio.set_noise_gate(source, settings);
        }
    }

    /// Source statuses and levels for the UI.
    pub fn report_audio(&mut self, endpoint: &RecorderEndpoint) {
        self.audio_reporter.report(endpoint, self.audio.as_mut());
    }

    /// Clears the UI's audio meters and statuses once the session is over.
    pub fn finish_audio_report(&self, endpoint: &RecorderEndpoint) {
        AudioReporter::finish(endpoint);
    }

    pub fn source_closed(&self) -> bool {
        self.source_closed.load(Ordering::Acquire)
    }

    /// `true` about once per second.
    pub fn status_due(&mut self) -> bool {
        self.meter
            .update(self.started.elapsed(), Duration::from_secs(1))
    }

    pub fn status(&self, exports_in_flight: usize) -> ReplayStatus {
        let stats = self
            .counters
            .snapshot(self.started.elapsed(), self.meter.rate());
        let buffer = self.buffer.as_ref().map(ReplayBuffer::stats);
        let mut status = replay_status(
            buffer.as_ref(),
            self.config.duration,
            exports_in_flight,
            stats,
        );
        status.warning = replay_warning(
            status.warning.as_deref(),
            self.video_start.is_none(),
            self.diagnostics().audio_glitch_frames,
            self.diagnostics.invalid_packets,
        );
        status
    }

    pub fn diagnostics(&self) -> ReplayDiagnostics {
        let resync = self
            .audio
            .as_ref()
            .map(RecordingAudio::resync_stats)
            .unwrap_or_default();
        ReplayDiagnostics {
            audio_glitch_frames: self
                .audio
                .as_ref()
                .map_or(0, RecordingAudio::glitched_frames),
            audio_resyncs: resync.count,
            audio_max_resync_frames: resync.max_frames,
            ..self.diagnostics
        }
    }

    /// Frames WGC delivered so far, including superseded ones: the source
    /// frame rate as the capture sees it, before pacing.
    pub fn capture_delivered(&self) -> u64 {
        self.capture.frames_delivered()
    }

    /// Cheap Arc-clone snapshot of the configured duration; no file I/O.
    pub fn snapshot(&mut self) -> Result<ReplaySnapshot, ReplayError> {
        let duration = self.config.duration;
        match &mut self.buffer {
            Some(buffer) => buffer.snapshot(duration),
            None => Err(ReplayError::NotReady),
        }
    }

    /// One bounded service step: frames, repeated frames, audio, packets.
    pub fn pump(&mut self) -> Result<(), String> {
        let interval = self.pacer.interval();
        if self.video_start.is_none()
            && !self.encoder.has_frame()
            && self.pending.is_none()
            && self.started.elapsed() > FIRST_FRAME_TIMEOUT
        {
            return Err("the capture source delivered no frames".into());
        }
        let mut repeats_left = MAX_REPEATS_PER_PUMP;
        let mut pulled = 0;
        loop {
            if self.pending.is_none() {
                if pulled >= MAX_FRAMES_PER_PUMP {
                    break;
                }
                let Some(frame) = self
                    .capture
                    .try_next_frame()
                    .map_err(|error| replay_failure_text(&error.to_string()))?
                else {
                    break;
                };
                pulled += 1;
                self.counters.captured += 1;
                let Some(sample_time) = self.pacer.accept(frame.timestamp()) else {
                    self.diagnostics.pacer_skipped += 1;
                    // Before the shared origin (initial WGC frame delivered
                    // while audio was starting): keep it as the repeat source
                    // so a static scene still produces video.
                    if !self.encoder.has_frame() {
                        self.encoder
                            .stage(frame.texture(), frame.content_size())
                            .map_err(|error| replay_failure_text(&encoder_message(&error)))?;
                    }
                    continue;
                };
                let slot = self.timeline.slot_of(sample_time);
                if self.timeline.is_late(slot) {
                    // Its slot is taken (a repeat or an earlier frame).
                    self.diagnostics.late_discarded += 1;
                    continue;
                }
                self.pending = Some(PendingFrame { frame, slot });
            }
            if !self.flush_pending(interval, &mut repeats_left)? {
                break;
            }
        }

        self.fill_static_scene(interval, &mut repeats_left)?;

        if let Some(audio) = &mut self.audio {
            let result = if self.video_start.is_none() {
                // No video to align with yet: keep the capture rings empty.
                audio.discard()
            } else {
                audio.pump(&mut self.encoder, false, self.timeline.end_ticks())
            };
            if let Err(message) = result {
                return Err(replay_failure_text(&message));
            }
        }
        self.drain_packets()
    }

    /// Starts the video (and audio) timeline at `slot`.
    fn begin_video(&mut self, slot: i64) {
        if self.timeline.started() {
            return;
        }
        self.timeline.begin(slot);
        let start = self.timeline.time_of(slot);
        self.video_start = Some(start);
        if let Some(audio) = &mut self.audio {
            audio.start_at(self.origin + start);
        }
    }

    /// Submits the held real frame once the slots before it are filled with
    /// repeats. `Ok(false)`: out of budget or the encoder is busy, the frame
    /// stays pending for the next iteration.
    fn flush_pending(&mut self, interval: i64, repeats_left: &mut usize) -> Result<bool, String> {
        let Some(pending) = self.pending.take() else {
            return Ok(true);
        };
        let slot = pending.slot;
        if self.timeline.is_late(slot) {
            self.diagnostics.late_discarded += 1;
            return Ok(true);
        }
        if !self.timeline.started() {
            // A remembered pre-origin frame lets the video start at 0.
            self.begin_video(if self.encoder.has_frame() { 0 } else { slot });
        }
        while self.timeline.next() < slot {
            if *repeats_left == 0 || !self.submit_repeat(interval)? {
                self.pending = Some(pending);
                return Ok(false);
            }
            *repeats_left -= 1;
        }
        let size = pending.frame.content_size();
        let outcome = self
            .encoder
            .submit(
                pending.frame.texture(),
                size,
                self.timeline.time_of(slot),
                interval,
            )
            .map_err(|error| replay_failure_text(&encoder_message(&error)))?;
        match outcome {
            SubmitOutcome::Submitted => {
                self.counters.encoded += 1;
                self.meter.record();
                // The encoder's staging texture now holds this frame and
                // serves as the repeat source: no extra GPU copy.
                self.timeline.advance();
            }
            // The slot stays empty; the static-scene fill repeats the last
            // frame there.
            SubmitOutcome::Dropped => self.counters.dropped += 1,
        }
        // `pending` drops here, returning its slot to the WGC pool.
        Ok(true)
    }

    /// Fills the next slot with the last frame. `Ok(false)` when the encoder
    /// is busy.
    fn submit_repeat(&mut self, interval: i64) -> Result<bool, String> {
        if !self.encoder.has_frame() {
            // Nothing to repeat (cannot happen once video started).
            self.timeline.advance();
            return Ok(true);
        }
        let time = self.timeline.time_of(self.timeline.next());
        let outcome = self
            .encoder
            .submit_repeat(time, interval)
            .map_err(|error| replay_failure_text(&encoder_message(&error)))?;
        match outcome {
            SubmitOutcome::Submitted => {
                self.counters.encoded += 1;
                self.diagnostics.repeated_frames += 1;
                self.meter.record();
                self.timeline.advance();
                Ok(true)
            }
            SubmitOutcome::Dropped => Ok(false),
        }
    }

    /// Re-submits the last frame while WGC is silent, up to real time minus a
    /// small lag, so the timeline keeps moving at the configured frame rate.
    fn fill_static_scene(&mut self, interval: i64, repeats_left: &mut usize) -> Result<(), String> {
        let now = clipper_audio::windows::qpc_now_ticks() - self.origin;
        // Only meaningful once video exists: without a first frame the video
        // end stays 0 and the clocks cannot be compared.
        if self.video_start.is_some()
            && self.audio.is_some()
            && now - AUDIO_LAG_TICKS - self.timeline.end_ticks() > MAX_CLOCK_GAP_TICKS
        {
            return Err("the audio and video clocks drifted apart".into());
        }
        if self.pending.is_some() || !self.encoder.has_frame() {
            return Ok(());
        }
        let due = now - repeat_lag_ticks(interval);
        while *repeats_left > 0 {
            if self.timeline.end_ticks() + interval > due {
                break;
            }
            self.begin_video(0);
            if !self.submit_repeat(interval)? {
                break;
            }
            *repeats_left -= 1;
        }
        Ok(())
    }

    fn drain_packets(&mut self) -> Result<(), String> {
        self.encoder
            .poll(&mut self.packets)
            .map_err(|error| replay_failure_text(&encoder_message(&error)))?;
        if self.packets.is_empty() {
            return Ok(());
        }
        if self.buffer.is_none() {
            // The encoder guarantees the layout before its first packet.
            let layout = self
                .encoder
                .layout()
                .ok_or("the encoder produced packets without a stream layout")?;
            let buffer = ReplayBuffer::new(self.config.clone(), layout)
                .map_err(|error| replay_failure_text(&error.to_string()))?;
            self.buffer = Some(buffer);
        }
        let Some(buffer) = self.buffer.as_mut() else {
            return Ok(());
        };
        for packet in self.packets.drain(..) {
            match buffer.push(packet) {
                Ok(()) => {}
                // Timestamps not increasing: lose this packet, keep recording.
                Err(ReplayError::Invalid(_)) => self.diagnostics.invalid_packets += 1,
                Err(error) => return Err(replay_failure_text(&error.to_string())),
            }
        }
        Ok(())
    }
}
