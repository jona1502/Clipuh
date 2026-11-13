//! Windows recorder worker: WGC capture -> pacing -> Media Foundation encoder.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use clipper_capture::windows::{CaptureSession, CaptureSignal, list_sources};
use clipper_core::pipeline::{FramePacer, RateMeter, recording_file_name};
use clipper_core::recording::{
    CaptureSource, REPLAY_NOT_RUNNING, RecorderCommand, RecorderEndpoint, RecorderEvent,
    RecordingOptions, RecordingStats, StopReason,
};
use clipper_encoder::backends::media_foundation::{
    EncoderConfig, EncoderError, HardwareEncoder, MediaFoundation, SubmitOutcome,
};
use clipper_platform::windows::{ComApartment, D3DContext};
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::emit;
use crate::logic::{
    Counters, Lifecycle, StopOutcome, classify_stop, fit_output_size, partial_path,
    unique_file_name,
};
use crate::recording_audio::{AudioTest, RecordingAudio};

const MAX_WAIT: Duration = Duration::from_millis(250);
const STATS_WINDOW: Duration = Duration::from_secs(1);

struct Recording {
    // Field order matters for implicit drops: capture first, then encoder,
    // then the device.
    session: Option<CaptureSession>,
    encoder: HardwareEncoder,
    audio: Option<RecordingAudio>,
    audio_failed: bool,
    video_end: i64,
    last_audio_meter: Instant,
    _d3d: D3DContext,
    pacer: FramePacer,
    meter: RateMeter,
    counters: Counters,
    started: Instant,
    final_path: PathBuf,
    partial: PathBuf,
    wake: Receiver<()>,
    source_closed: Arc<AtomicBool>,
}

impl Recording {
    fn stats(&self) -> RecordingStats {
        self.counters
            .snapshot(self.started.elapsed(), self.meter.rate())
    }

    /// Moves every pending capture frame through pacer and encoder.
    fn pump(&mut self) -> Result<(), String> {
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        // Bound each video batch: continuous WGC delivery must not starve
        // the independent WASAPI rings or command processing.
        for _ in 0..4 {
            let Some(frame) = session
                .try_next_frame()
                .map_err(|error| error.to_string())?
            else {
                break;
            };
            self.counters.captured += 1;
            let Some(sample_time) = self.pacer.accept(frame.timestamp()) else {
                continue;
            };
            // A frame delivered late after a declared WGC gap must not move
            // the muxer timeline backwards.
            if sample_time < self.video_end - self.pacer.interval() {
                continue;
            }
            let outcome = self
                .encoder
                .submit(
                    frame.texture(),
                    frame.content_size(),
                    sample_time,
                    self.pacer.interval(),
                )
                .map_err(|error| encoder_message(&error))?;
            match outcome {
                SubmitOutcome::Submitted => {
                    self.counters.encoded += 1;
                    self.meter.record();
                    self.video_end = sample_time + self.pacer.interval();
                }
                SubmitOutcome::Dropped => self.counters.dropped += 1,
            }
            // `frame` drops here, returning its slot to the WGC pool.
        }
        if let Some(audio) = &self.audio {
            let due = clipper_audio::windows::qpc_now_ticks() - audio.origin() - 800_000;
            if due - self.video_end > 10_000_000 {
                self.audio_failed = true;
                return Err("Audio/video clock gap exceeded the bounded recovery window.".into());
            }
            // WGC is change-driven. Mark missing frames instead of letting
            // the synchronous sink writer stall audio while a scene is static.
            while self.video_end + self.pacer.interval() < due {
                self.encoder
                    .video_gap(self.video_end)
                    .map_err(|error| encoder_message(&error))?;
                self.video_end += self.pacer.interval();
            }
        }
        if let Some(audio) = &mut self.audio
            && let Err(message) = audio.pump(&mut self.encoder, false, self.video_end)
        {
            self.audio_failed = true;
            return Err(message);
        }
        Ok(())
    }
}

struct Worker<'a> {
    endpoint: &'a RecorderEndpoint,
    lifecycle: Lifecycle,
    init_error: Option<String>,
    // Field order is drop order: the recording and Media Foundation must be
    // released before the COM apartment is left.
    recording: Option<Recording>,
    audio_test: Option<AudioTest>,
    media_foundation: Option<MediaFoundation>,
    _com: Option<ComApartment>,
}

pub fn run(endpoint: &RecorderEndpoint) {
    let (com, init_error) = match ComApartment::init_multithreaded() {
        Ok(com) => (Some(com), None),
        Err(error) => (None, Some(format!("COM initialization failed: {error}"))),
    };
    if let Some(message) = &init_error {
        emit(
            endpoint,
            RecorderEvent::Error {
                message: message.clone(),
            },
        );
    }
    let mut worker = Worker {
        endpoint,
        lifecycle: Lifecycle::new(),
        init_error,
        recording: None,
        audio_test: None,
        media_foundation: None,
        _com: com,
    };

    let mut running = true;
    while running {
        if worker.recording.is_none() && worker.audio_test.is_none() {
            match endpoint.commands.recv() {
                Ok(command) => running = worker.handle(command),
                Err(_) => break,
            }
            continue;
        }
        worker.service_recording();
        worker.service_audio_test();
        loop {
            match endpoint.commands.try_recv() {
                Ok(command) => {
                    if !worker.handle(command) {
                        running = false;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    running = false;
                    break;
                }
            }
        }
    }
    worker.stop_recording(StopReason::Shutdown, None);
    worker.stop_audio_test(None);
}

impl Worker<'_> {
    fn send(&self, event: RecorderEvent) {
        emit(self.endpoint, event);
    }

    fn error(&self, message: impl Into<String>) {
        self.send(RecorderEvent::Error {
            message: message.into(),
        });
    }

    /// Returns `false` when the worker must exit.
    fn handle(&mut self, command: RecorderCommand) -> bool {
        match command {
            RecorderCommand::StartAudioTest {
                options,
                output_directory,
            } => {
                if self.recording.is_some() || self.audio_test.is_some() {
                    self.error("Stop the current recording or audio test first.");
                } else {
                    match AudioTest::start(&options, &output_directory) {
                        Ok(test) => {
                            self.audio_test = Some(test);
                            self.send(RecorderEvent::AudioTestStarted);
                        }
                        Err(message) => self.send(RecorderEvent::AudioTestFinished {
                            path: None,
                            error: Some(message),
                        }),
                    }
                }
            }
            RecorderCommand::StopAudioTest => self.stop_audio_test(None),
            RecorderCommand::Shutdown => {
                self.stop_audio_test(None);
                self.stop_recording(StopReason::Shutdown, None);
                return false;
            }
            RecorderCommand::RefreshSources => match list_sources() {
                Ok(sources) => self.send(RecorderEvent::SourcesListed(sources)),
                Err(error) => self.error(format!("could not list capture sources: {error}")),
            },
            RecorderCommand::StartRecording { source, options } => {
                if self.audio_test.is_some() {
                    self.error("Stop the audio test first.");
                } else {
                    self.start_recording(&source, &options);
                }
            }
            RecorderCommand::StopRecording => match self.lifecycle.begin_stop() {
                Ok(()) => self.stop_recording_started(StopReason::User, None),
                Err(message) => self.error(message),
            },
            // A real replay engine will emit ReplaySaving then ReplaySaved/ReplaySaveFailed here.
            RecorderCommand::SaveReplay => {
                // Repeated hotkeys must not stall capture/audio when the UI
                // notification queue is full. This rejection is best effort.
                let _ = self
                    .endpoint
                    .events
                    .try_send(RecorderEvent::ReplaySaveFailed {
                        message: REPLAY_NOT_RUNNING.into(),
                    });
            }
        }
        true
    }

    fn stop_audio_test(&mut self, failure: Option<String>) {
        let Some(test) = self.audio_test.take() else {
            return;
        };
        let result = test.finish().and_then(|path| {
            if let Some(message) = failure {
                let _ = fs::remove_file(path);
                Err(message)
            } else {
                Ok(path)
            }
        });
        let (path, error) = match result {
            Ok(path) => (Some(path), None),
            Err(error) => (None, Some(error)),
        };
        self.send(RecorderEvent::AudioTestFinished { path, error });
        let _ = self.endpoint.events.try_send(RecorderEvent::AudioLevels {
            desktop: 0.0,
            microphone: 0.0,
        });
    }

    fn service_audio_test(&mut self) {
        let Some(test) = &mut self.audio_test else {
            return;
        };
        let _ = test.wake.recv_timeout(MAX_WAIT);
        if let Err(message) = test.pump(false) {
            self.stop_audio_test(Some(message));
            return;
        }
        if let Some((desktop, microphone)) = test.meter() {
            let _ = self.endpoint.events.try_send(RecorderEvent::AudioLevels {
                desktop,
                microphone,
            });
        }
        if test.elapsed() >= Duration::from_secs(30) {
            self.stop_audio_test(None);
        }
    }

    fn start_recording(&mut self, source: &CaptureSource, options: &RecordingOptions) {
        if let Err(message) = self.lifecycle.begin_start() {
            self.error(message);
            return;
        }
        match self.try_start(source, options) {
            Ok(recording) => {
                self.lifecycle.started();
                self.send(RecorderEvent::RecordingStarted {
                    path: recording.final_path.clone(),
                    encoder: recording.encoder.name().to_owned(),
                });
                self.recording = Some(recording);
            }
            Err(message) => {
                self.lifecycle.start_failed(&message);
                self.send(RecorderEvent::RecordingFailed { message });
            }
        }
    }

    fn try_start(
        &mut self,
        source: &CaptureSource,
        options: &RecordingOptions,
    ) -> Result<Recording, String> {
        if let Some(message) = &self.init_error {
            return Err(message.clone());
        }
        if self.media_foundation.is_none() {
            let mf = MediaFoundation::start().map_err(|error| encoder_message(&error))?;
            self.media_foundation = Some(mf);
        }
        let Some(mf) = self.media_foundation.as_ref() else {
            return Err("Media Foundation is not available".into());
        };

        fs::create_dir_all(&options.output_directory).map_err(|error| {
            format!(
                "could not create the output folder {}: {error}",
                options.output_directory.display()
            )
        })?;
        let (final_path, partial) = choose_paths(&options.output_directory);
        precheck_writable(&partial)?;

        let result = build_recording(mf, source, options, final_path, partial.clone());
        if result.is_err() {
            let _ = fs::remove_file(&partial);
        }
        result
    }

    /// Waits for a capture wake (bounded), moves frames and reports stats.
    fn service_recording(&mut self) {
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        match recording.wake.recv_timeout(MAX_WAIT) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_millis(10)),
        }
        if let Err(message) = recording.pump() {
            self.stop_recording(StopReason::Error, Some(message));
            return;
        }
        if recording.last_audio_meter.elapsed() >= Duration::from_millis(100) {
            if let Some(audio) = &recording.audio {
                let (desktop, microphone) = audio.levels();
                let _ = self.endpoint.events.try_send(RecorderEvent::AudioLevels {
                    desktop,
                    microphone,
                });
            }
            recording.last_audio_meter = Instant::now();
        }
        if recording.source_closed.load(Ordering::Acquire) {
            self.stop_recording(StopReason::SourceClosed, None);
            return;
        }
        if recording
            .meter
            .update(recording.started.elapsed(), STATS_WINDOW)
        {
            let stats = recording.stats();
            // Never block on stats: drop them if the UI queue is full.
            let _ = self.endpoint.events.try_send(RecorderEvent::Stats(stats));
        }
    }

    /// Stops an active recording (no-op when idle), validating via lifecycle.
    fn stop_recording(&mut self, reason: StopReason, failure: Option<String>) {
        if self.recording.is_none() {
            return;
        }
        let _ = self.lifecycle.begin_stop();
        self.stop_recording_started(reason, failure);
    }

    /// Finalizes the active recording after `begin_stop` succeeded.
    fn stop_recording_started(&mut self, reason: StopReason, failure: Option<String>) {
        let Some(recording) = self.recording.take() else {
            self.lifecycle.stopped();
            return;
        };
        let Recording {
            session,
            mut encoder,
            mut audio,
            audio_failed,
            video_end: _,
            _d3d,
            meter,
            counters,
            started,
            final_path,
            partial,
            ..
        } = recording;
        // Stop frames first, then drain and finalize the encoder.
        drop(session);
        let stats = counters.snapshot(started.elapsed(), meter.rate());
        let audio_finish = audio.as_mut().map_or(Ok(()), |audio| {
            let end = clipper_audio::windows::qpc_now_ticks().saturating_sub(audio.origin());
            encoder
                .video_gap(end)
                .map_err(|error| encoder_message(&error))?;
            audio.pump(&mut encoder, true, end)
        });
        drop(audio);
        let finish = encoder
            .finish()
            .map_err(|error| encoder_message(&error))
            .and(audio_finish);
        drop(_d3d);

        let finish = if audio_failed {
            Err(failure.clone().unwrap_or_default())
        } else {
            finish
        };
        let outcome = match classify_stop(counters.encoded, finish) {
            StopOutcome::Success => match fs::rename(&partial, &final_path) {
                Ok(()) => Ok(()),
                Err(error) => {
                    // The partial file is complete and playable: keep it.
                    self.lifecycle.stopped();
                    self.send(RecorderEvent::RecordingFailed {
                        message: format!(
                            "the recording is complete but could not be renamed to {} ({error}); it was kept as {}",
                            final_path.display(),
                            partial.display()
                        ),
                    });
                    return;
                }
            },
            StopOutcome::Failure(message) => Err(message),
        };
        self.lifecycle.stopped();
        match outcome {
            Ok(()) => {
                if let Some(message) = failure {
                    self.error(message);
                }
                self.send(RecorderEvent::RecordingFinished {
                    path: final_path,
                    stats,
                    reason,
                });
            }
            Err(message) => {
                let _ = fs::remove_file(&partial);
                let message = match failure {
                    Some(cause) if cause != message => format!("{cause}; {message}"),
                    _ => message,
                };
                self.send(RecorderEvent::RecordingFailed { message });
            }
        }
    }
}

fn build_recording(
    mf: &MediaFoundation,
    source: &CaptureSource,
    options: &RecordingOptions,
    final_path: PathBuf,
    partial: PathBuf,
) -> Result<Recording, String> {
    let d3d =
        D3DContext::new().map_err(|error| format!("could not create the GPU device: {error}"))?;

    let (wake_tx, wake) = sync_channel::<()>(1);
    let source_closed = Arc::new(AtomicBool::new(false));
    let session = CaptureSession::start(
        &d3d,
        source,
        signal_handler(wake_tx.clone(), Arc::clone(&source_closed)),
    )
    .map_err(|error| error.to_string())?;

    let (width, height) = session.size();
    let (output_width, output_height) =
        fit_output_size((width, height), (options.width, options.height));
    let config = EncoderConfig {
        input_width: round_up_even(width),
        input_height: round_up_even(height),
        output_width,
        output_height,
        fps: options.fps,
        bitrate_mbps: options.bitrate_mbps,
        profile: options.profile,
    };
    let encoder = HardwareEncoder::create_with_audio(
        mf,
        &d3d,
        &config,
        &partial,
        options.audio.track_count(),
    )
    .map_err(|error| encoder_message(&error))?;

    // Initialize expensive GPU/codec resources before audio starts, otherwise
    // hardware encoder discovery can overflow the bounded audio rings.
    let audio = RecordingAudio::start(&options.audio, wake_tx)?;
    let origin = audio.as_ref().map(RecordingAudio::origin);

    Ok(Recording {
        session: Some(session),
        encoder,
        audio,
        audio_failed: false,
        video_end: 0,
        last_audio_meter: Instant::now(),
        _d3d: d3d,
        pacer: origin.map_or_else(
            || FramePacer::new(options.fps),
            |origin| FramePacer::with_origin(options.fps, origin),
        ),
        meter: RateMeter::new(),
        counters: Counters::default(),
        started: Instant::now(),
        final_path,
        partial,
        wake,
        source_closed,
    })
}

/// Runs on a WGC thread: must never block or allocate.
fn signal_handler(
    wake: SyncSender<()>,
    source_closed: Arc<AtomicBool>,
) -> Box<dyn Fn(CaptureSignal) + Send + Sync + 'static> {
    Box::new(move |signal| {
        if signal == CaptureSignal::SourceClosed {
            source_closed.store(true, Ordering::Release);
        }
        // Full means a wake is already pending, which is enough.
        let _ = wake.try_send(());
    })
}

fn round_up_even(value: u32) -> u32 {
    value.max(2).saturating_add(1) & !1
}

#[allow(unsafe_code)]
fn choose_paths(directory: &Path) -> (PathBuf, PathBuf) {
    // SAFETY: GetLocalTime takes no arguments and only returns a value.
    let now = unsafe { GetLocalTime() };
    let base = recording_file_name(
        now.wYear,
        now.wMonth,
        now.wDay,
        now.wHour,
        now.wMinute,
        now.wSecond,
    );
    let name = unique_file_name(&base, |candidate| {
        let path = directory.join(candidate);
        path.exists() || partial_path(&path).exists()
    });
    let final_path = directory.join(name);
    let partial = partial_path(&final_path);
    (final_path, partial)
}

/// Fails early with a clear message if the output location is not writable.
fn precheck_writable(partial: &Path) -> Result<(), String> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial)
    {
        Ok(file) => {
            drop(file);
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::PermissionDenied => Err(format!(
            "no permission to write to {}",
            partial.parent().unwrap_or(partial).display()
        )),
        Err(error) => Err(format!("cannot create the output file: {error}")),
    }
}

fn encoder_message(error: &EncoderError) -> String {
    match error {
        EncoderError::DiskFull => "not enough disk space to keep recording".into(),
        EncoderError::AccessDenied => "no permission to write the output file".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::round_up_even;

    #[test]
    fn rounds_up_to_even() {
        assert_eq!(round_up_even(1920), 1920);
        assert_eq!(round_up_even(1081), 1082);
        assert_eq!(round_up_even(0), 2);
        assert_eq!(round_up_even(u32::MAX), u32::MAX - 1);
    }
}
