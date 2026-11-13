//! Instant replay session: WGC capture -> pacing -> packet encoder -> hybrid
//! replay buffer, all driven from the recorder worker thread.
//!
//! The worker never performs file I/O in steady state. The only exceptions
//! are `ReplayBuffer::new`, which creates the SSD session directory once when
//! the first packet defines the layout, and the tiny `exists()` checks used
//! to pick a clip name when a save is requested. Spilling to the SSD runs on
//! the buffer's own spill thread and clip files are written by the exporter
//! thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use clipper_capture::windows::CaptureSession;
use clipper_core::pipeline::{FramePacer, RateMeter};
use clipper_core::recording::{CaptureSource, ReplayOptions, ReplayStatus};
use clipper_encoder::backends::media_foundation::{EncoderConfig, MediaFoundation, SubmitOutcome};
use clipper_encoder::backends::packet_encoder::{PacketEncoder, default_label};
use clipper_platform::windows::D3DContext;
use clipper_replay::EncodedPacket;
use clipper_replay::disk::DiskConfig;
use clipper_replay::error::ReplayError;
use clipper_replay::hybrid::{ReplayBuffer, ReplayConfig, ReplaySnapshot};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    ID3D11Texture2D,
};

use crate::logic::{
    Counters, fit_output_size, replay_failure_text, replay_status, valid_replay_duration,
};
use crate::recording_audio::RecordingAudio;
use crate::windows_worker::{encoder_message, round_up_even, signal_handler};

/// Longest wait for a capture/audio wake. Short on purpose: on a static scene
/// WGC is silent and the pump itself must keep the 60 fps timeline moving.
pub const WAKE_WAIT: Duration = Duration::from_millis(30);
/// Real frames moved per service iteration (keeps audio and commands live).
const MAX_FRAMES_PER_PUMP: usize = 4;
/// Repeated frames submitted per service iteration.
const MAX_REPEATS_PER_PUMP: usize = 16;
/// Frames of silence before a static scene is filled with repeated frames,
/// so a merely late real frame is never overtaken by a repeat.
const REPEAT_LAG_FRAMES: i64 = 3;
/// Audio is mixed this far behind real time (same as manual recording).
const AUDIO_LAG_TICKS: i64 = 800_000;
/// Audio/video clock gap that cannot be recovered.
const MAX_CLOCK_GAP_TICKS: i64 = 10_000_000;

/// GPU-side copy of the newest captured frame. Its content is re-submitted
/// while WGC delivers nothing (static scene) so the stream keeps a constant
/// frame rate and regular keyframes. Never read back to the CPU.
struct LastFrame {
    texture: ID3D11Texture2D,
    dims: (u32, u32),
    content: (u32, u32),
}

pub struct ReplaySession {
    // Field order is drop order: stop capture first, then the encoder, audio
    // and the buffer, and the device last.
    capture: CaptureSession,
    encoder: PacketEncoder,
    audio: Option<RecordingAudio>,
    buffer: Option<ReplayBuffer>,
    last_frame: Option<LastFrame>,
    d3d: D3DContext,
    config: ReplayConfig,
    origin: i64,
    pacer: FramePacer,
    meter: RateMeter,
    counters: Counters,
    started: Instant,
    /// End of the newest submitted frame on the session timeline.
    video_end: i64,
    /// Start time of the newest submitted frame.
    last_time: Option<i64>,
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
        let encoder =
            PacketEncoder::create_with_labels(mf, &d3d, &config, &audio_labels(recording))
                .map_err(|error| replay_failure_text(&encoder_message(&error)))?;
        let encoder_name = encoder.name().to_owned();

        // Initialize expensive GPU/codec resources before audio starts,
        // otherwise hardware encoder discovery can overflow the audio rings.
        let audio = RecordingAudio::start(&recording.audio, wake_tx)
            .map_err(|message| replay_failure_text(&message))?;
        let origin = audio.as_ref().map_or_else(
            clipper_audio::windows::qpc_now_ticks,
            RecordingAudio::origin,
        );

        Ok(Self {
            capture,
            encoder,
            audio,
            buffer: None,
            last_frame: None,
            d3d,
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
            video_end: 0,
            last_time: None,
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
        replay_status(
            buffer.as_ref(),
            self.config.duration,
            exports_in_flight,
            stats,
        )
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
        for _ in 0..MAX_FRAMES_PER_PUMP {
            let Some(frame) = self
                .capture
                .try_next_frame()
                .map_err(|error| replay_failure_text(&error.to_string()))?
            else {
                break;
            };
            self.counters.captured += 1;
            let accepted = self.pacer.accept(frame.timestamp());
            let Some(sample_time) = accepted else {
                // Before the shared origin (initial WGC frame delivered while
                // audio was starting): keep it as the repeat source so a
                // static scene still produces video.
                if self.last_frame.is_none() {
                    self.remember(frame.texture(), frame.content_size())?;
                }
                continue;
            };
            // A frame delivered late after a declared gap must not move the
            // timeline backwards or collide with a repeated frame.
            if sample_time < self.video_end - interval
                || self
                    .last_time
                    .is_some_and(|last| sample_time < last + interval / 2)
            {
                continue;
            }
            let outcome = self
                .encoder
                .submit(frame.texture(), frame.content_size(), sample_time, interval)
                .map_err(|error| replay_failure_text(&encoder_message(&error)))?;
            match outcome {
                SubmitOutcome::Submitted => {
                    self.counters.encoded += 1;
                    self.meter.record();
                    self.video_end = sample_time + interval;
                    self.last_time = Some(sample_time);
                    self.remember(frame.texture(), frame.content_size())?;
                }
                SubmitOutcome::Dropped => self.counters.dropped += 1,
            }
            // `frame` drops here, returning its slot to the WGC pool.
        }

        self.fill_static_scene(interval)?;

        if let Some(audio) = &mut self.audio {
            if let Err(message) = audio.pump(&mut self.encoder, false, self.video_end) {
                return Err(replay_failure_text(&message));
            }
        }
        self.drain_packets()
    }

    /// Re-submits the last frame while WGC is silent, up to real time minus a
    /// small lag, so the timeline keeps moving at the configured frame rate.
    fn fill_static_scene(&mut self, interval: i64) -> Result<(), String> {
        let now = clipper_audio::windows::qpc_now_ticks() - self.origin;
        if self.audio.is_some() && now - AUDIO_LAG_TICKS - self.video_end > MAX_CLOCK_GAP_TICKS {
            return Err("the audio and video clocks drifted apart".into());
        }
        let due = now - REPEAT_LAG_FRAMES * interval;
        for _ in 0..MAX_REPEATS_PER_PUMP {
            if self.video_end + interval > due {
                break;
            }
            let Some(last) = &self.last_frame else {
                break;
            };
            let outcome = self
                .encoder
                .submit(&last.texture, last.content, self.video_end, interval)
                .map_err(|error| replay_failure_text(&encoder_message(&error)))?;
            match outcome {
                SubmitOutcome::Submitted => {
                    self.counters.encoded += 1;
                    self.meter.record();
                    self.last_time = Some(self.video_end);
                    self.video_end += interval;
                }
                // Encoder busy: try again on the next iteration.
                SubmitOutcome::Dropped => break,
            }
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
            buffer
                .push(packet)
                .map_err(|error| replay_failure_text(&error.to_string()))?;
        }
        Ok(())
    }

    /// GPU-copies the content area of `source` into the repeat texture.
    #[allow(unsafe_code)]
    fn remember(&mut self, source: &ID3D11Texture2D, content: (u32, u32)) -> Result<(), String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `source` is a live texture and `desc` a valid out-pointer.
        unsafe { source.GetDesc(&mut desc) };
        let dims = (desc.Width, desc.Height);
        if self
            .last_frame
            .as_ref()
            .is_none_or(|last| last.dims != dims)
        {
            desc.Usage = D3D11_USAGE_DEFAULT;
            desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
            desc.CPUAccessFlags = 0;
            desc.MiscFlags = 0;
            let mut texture: Option<ID3D11Texture2D> = None;
            // SAFETY: valid descriptor and out-pointer; no initial data.
            unsafe {
                self.d3d
                    .device()
                    .CreateTexture2D(&desc, None, Some(&mut texture))
            }
            .map_err(|error| format!("could not allocate the replay frame copy: {error}"))?;
            let texture = texture.ok_or("could not allocate the replay frame copy")?;
            self.last_frame = Some(LastFrame {
                texture,
                dims,
                content: (0, 0),
            });
        }
        let Some(last) = self.last_frame.as_mut() else {
            return Ok(());
        };
        let content = (content.0.clamp(1, dims.0), content.1.clamp(1, dims.1));
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: content.0,
            bottom: content.1,
            back: 1,
        };
        // SAFETY: both textures belong to this device and have the same
        // format; the region lies inside both. The immediate context is
        // multithread protected and the copy stays on the GPU.
        unsafe {
            self.d3d.context().CopySubresourceRegion(
                &last.texture,
                0,
                0,
                0,
                0,
                source,
                0,
                Some(&region),
            );
        }
        last.content = content;
        Ok(())
    }
}

/// Audio track labels: track 0 is the mix, then desktop and microphone when
/// they are recorded separately (same track count as manual recordings).
pub fn audio_labels(options: &clipper_core::recording::RecordingOptions) -> Vec<String> {
    let audio = &options.audio;
    if !audio.desktop && !audio.microphone {
        return Vec::new();
    }
    let separate = if audio.separate_tracks {
        usize::from(audio.desktop) + usize::from(audio.microphone)
    } else {
        0
    };
    let mut labels = vec![default_label(0)];
    if audio.separate_tracks {
        if audio.desktop {
            labels.push(default_label(1));
        }
        if audio.microphone {
            labels.push(default_label(2));
        }
    }
    debug_assert_eq!(labels.len(), 1 + separate);
    labels
}

#[cfg(test)]
mod tests {
    use clipper_core::recording::{QualityProfile, RecordingAudioOptions, RecordingOptions};

    use super::*;

    fn options(desktop: bool, microphone: bool, separate: bool) -> RecordingOptions {
        RecordingOptions {
            audio: RecordingAudioOptions {
                desktop,
                microphone,
                separate_tracks: separate,
                ..Default::default()
            },
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 16,
            profile: QualityProfile::Balanced,
            output_directory: "out".into(),
        }
    }

    #[test]
    fn labels_follow_the_manual_recording_track_count() {
        assert!(audio_labels(&options(false, false, true)).is_empty());
        assert_eq!(audio_labels(&options(true, false, false)), ["Mix"]);
        assert_eq!(audio_labels(&options(true, true, false)), ["Mix"]);
        assert_eq!(
            audio_labels(&options(true, true, true)),
            ["Mix", "Desktop", "Microphone"]
        );
        assert_eq!(
            audio_labels(&options(false, true, true)),
            ["Mix", "Microphone"]
        );
        assert_eq!(
            audio_labels(&options(true, false, true)),
            ["Mix", "Desktop"]
        );
    }
}
