//! Recorder worker: owns capture, encoder and output file on one dedicated
//! thread, driven by bounded channels from `clipper-core`.
//!
//! Thread model: exactly one worker thread. Idle, it blocks in `recv()` on the
//! command channel (zero CPU). While recording it waits on a capacity-1 wake
//! channel fed by the capture callback with a non-blocking `try_send`, with a
//! timeout of at most 250 ms so commands and stats are serviced promptly even
//! when no frames arrive.
#![deny(unsafe_code)]

mod logic;
#[cfg(windows)]
mod recording_audio;
#[cfg(windows)]
mod windows_worker;

use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(not(windows))]
use clipper_core::recording::RecorderCommand;
use clipper_core::recording::{RecorderEndpoint, RecorderEvent, RecorderHandle, recorder_channels};

/// Owns the worker thread. Dropping it requests shutdown and joins.
pub struct Recorder {
    handle: RecorderHandle,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Spawns the single recorder worker thread. It blocks on the command
    /// channel while idle (no polling, no CPU use).
    pub fn spawn(command_capacity: usize, event_capacity: usize) -> std::io::Result<Self> {
        let (handle, endpoint) = recorder_channels(command_capacity, event_capacity);
        let thread = std::thread::Builder::new()
            .name("clipper-recorder".into())
            .spawn(move || {
                // A panic must still tell the UI the worker is gone, otherwise
                // it would wait forever in "Starting" or "Recording".
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&endpoint);
                }));
                if result.is_err() {
                    emit(
                        &endpoint,
                        RecorderEvent::RecordingFailed {
                            message: "the recorder stopped unexpectedly".into(),
                        },
                    );
                }
                emit(&endpoint, RecorderEvent::Stopped);
            })?;
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }

    pub fn handle(&self) -> RecorderHandle {
        self.handle.clone()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        use clipper_core::recording::{RecorderCommand, SendCommandError};

        let Some(thread) = self.thread.take() else {
            return;
        };
        // The worker may be blocked sending events (we hold a receiver clone),
        // so keep draining while waiting for it.
        let events = self.handle.events();
        loop {
            match self.handle.send(RecorderCommand::Shutdown) {
                Ok(()) | Err(SendCommandError::Disconnected) => break,
                Err(SendCommandError::Busy) => {
                    while events.try_recv().is_ok() {}
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        while !thread.is_finished() {
            while events.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = thread.join();
    }
}

/// Important lifecycle event: may block the worker, never the UI.
pub(crate) fn emit(endpoint: &RecorderEndpoint, event: RecorderEvent) {
    let _ = endpoint.events.send_blocking(event);
}

#[cfg(windows)]
fn run(endpoint: &RecorderEndpoint) {
    windows_worker::run(endpoint);
}

#[cfg(not(windows))]
fn run(endpoint: &RecorderEndpoint) {
    while let Ok(command) = endpoint.commands.recv() {
        if command == RecorderCommand::Shutdown {
            break;
        }
        emit(
            endpoint,
            RecorderEvent::Error {
                message: "recording is only supported on Windows".into(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clipper_core::recording::RecorderCommand;

    use super::*;

    fn next(handle: &RecorderHandle) -> RecorderEvent {
        let events = handle.events();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(event) = events.try_recv() {
                return event;
            }
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn shutdown_joins_and_emits_stopped() {
        let recorder = Recorder::spawn(4, 8).unwrap();
        let handle = recorder.handle();
        handle.send(RecorderCommand::Shutdown).unwrap();
        assert_eq!(next(&handle), RecorderEvent::Stopped);
        drop(recorder);
    }

    #[test]
    fn stop_while_idle_reports_error_and_worker_stays_usable() {
        let recorder = Recorder::spawn(4, 8).unwrap();
        let handle = recorder.handle();
        for _ in 0..2 {
            handle.send(RecorderCommand::StopRecording).unwrap();
            assert!(matches!(next(&handle), RecorderEvent::Error { .. }));
        }
        drop(recorder);
    }

    #[test]
    fn drop_joins_with_full_queues() {
        let recorder = Recorder::spawn(1, 1).unwrap();
        let handle = recorder.handle();
        let _ = handle.send(RecorderCommand::StopRecording);
        let _ = handle.send(RecorderCommand::StopRecording);
        drop(recorder);
    }
    #[test]
    #[ignore = "captures real microphone audio; requires an enabled Windows microphone"]
    fn microphone_test_finalizes_a_pcm_wave_and_leaves_worker_usable() {
        let recorder = Recorder::spawn(8, 64).unwrap();
        let handle = recorder.handle();
        let output_directory = std::env::temp_dir().join("clipper-audio-diagnostic-tests");
        handle
            .send(RecorderCommand::StartAudioTest {
                options: clipper_core::recording::RecordingAudioOptions {
                    desktop: false,
                    microphone: true,
                    separate_tracks: false,
                    ..Default::default()
                },
                output_directory,
            })
            .unwrap();
        assert_eq!(next(&handle), RecorderEvent::AudioTestStarted);
        let mut peak = 0.0_f32;
        let start = std::time::Instant::now();
        let mut stopped = false;
        let path = loop {
            match next(&handle) {
                RecorderEvent::AudioLevels { microphone, .. } => peak = peak.max(microphone),
                RecorderEvent::AudioTestFinished { path, error } => {
                    assert!(error.is_none(), "{error:?}");
                    break path.unwrap();
                }
                other => panic!("unexpected {other:?}"),
            }
            if !stopped && start.elapsed() >= Duration::from_secs(30) {
                handle.send(RecorderCommand::StopAudioTest).unwrap();
                stopped = true;
            }
        };
        let wave = std::fs::read(&path).unwrap();
        assert_eq!(&wave[..4], b"RIFF");
        assert_eq!(&wave[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wave[24..28].try_into().unwrap()), 48_000);
        let data_bytes = u32::from_le_bytes(wave[40..44].try_into().unwrap());
        assert_eq!(wave.len(), data_bytes as usize + 44);
        assert!(data_bytes > 48_000 * 4 * 29);
        println!(
            "real microphone WAV seconds={:.3} peak={peak:.5}",
            f64::from(data_bytes) / 192_000.0
        );
        std::fs::remove_file(path).unwrap();
        handle.send(RecorderCommand::StopRecording).unwrap();
        // A final zero-level event can precede the idle command response.
        loop {
            if matches!(next(&handle), RecorderEvent::Error { .. }) {
                break;
            }
        }
        drop(recorder);
    }
}
