//! WASAPI to aligned AAC on the recorder worker. No file I/O on capture threads.
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use clipper_audio::mixer::TimelineMixer;
use clipper_audio::windows::{AudioCapture, list_devices, qpc_now_ticks};
use clipper_audio::{
    AudioConfig, AudioEvent, AudioSelection, DeviceFlow, MicrophoneChoice, TrackContent,
};
use clipper_core::recording::RecordingAudioOptions;
use clipper_encoder::backends::media_foundation::HardwareEncoder;

pub struct RecordingAudio {
    capture: AudioCapture,
    mixer: TimelineMixer,
    origin: i64,
    initial_dropped: u64,
}

impl RecordingAudio {
    pub fn start(
        options: &RecordingAudioOptions,
        wake: SyncSender<()>,
    ) -> Result<Option<Self>, String> {
        if !options.desktop && !options.microphone {
            return Ok(None);
        }
        for (enabled, flow, selected) in [
            (options.desktop, DeviceFlow::Render, &options.output_device),
            (
                options.microphone,
                DeviceFlow::Capture,
                &options.input_device,
            ),
        ] {
            if !enabled {
                continue;
            }
            let devices = list_devices(flow).map_err(|e| e.to_string())?;
            if !devices
                .iter()
                .any(|d| selected.as_ref().map_or(d.is_default, |id| &d.id == id))
            {
                return Err(format!(
                    "Selected {flow:?} audio device is unavailable. Select an available device or disable this source."
                ));
            }
        }
        let selection = AudioSelection {
            desktop: options.desktop,
            microphone: options.microphone.then(|| {
                options
                    .input_device
                    .as_ref()
                    .map_or(MicrophoneChoice::Default, |id| {
                        MicrophoneChoice::Device(id.clone())
                    })
            }),
            ..Default::default()
        };
        let mut capture = AudioCapture::start_with_device(
            &selection,
            AudioConfig::default(),
            options.output_device.as_deref(),
            Some(wake),
        )
        .map_err(|e| e.to_string())?;
        let count = capture.tracks().len();
        let gains = capture
            .tracks()
            .iter()
            .map(|t| {
                f32::from(if t.content == TrackContent::Microphone {
                    options.microphone_gain_percent
                } else {
                    options.desktop_gain_percent
                }) / 100.0
            })
            .collect();
        let mut ready = vec![false; count];
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.iter().all(|ready| *ready) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("Audio startup timed out; recording was not started.".into());
            }
            match capture
                .events()
                .recv_timeout(left.min(Duration::from_millis(100)))
            {
                Ok(AudioEvent::TrackStarted { track }) => ready[track] = true,
                Ok(AudioEvent::Failed { message, .. }) => return Err(message),
                Ok(AudioEvent::SourceLost { reason, .. }) => return Err(reason),
                _ => {}
            }
            // Startup is not part of the recording. Discard early packets
            // until every selected device is ready, keeping startup bounded.
            capture.read_frames(|_| {});
        }
        let origin = qpc_now_ticks();
        let initial_dropped = capture.dropped_frames();
        let mixer = TimelineMixer::new(origin, gains, options.separate_tracks);
        Ok(Some(Self {
            capture,
            mixer,
            origin,
            initial_dropped,
        }))
    }

    pub fn levels(&self) -> (f32, f32) {
        let (mut desktop, mut microphone) = (0.0_f32, 0.0_f32);
        for track in self.capture.tracks() {
            let peak = self.capture.levels(track.index).peak;
            if track.content == TrackContent::Microphone {
                microphone = peak;
            } else {
                desktop = peak;
            }
        }
        (desktop, microphone)
    }

    pub fn origin(&self) -> i64 {
        self.origin
    }

    pub fn pump(
        &mut self,
        encoder: &mut HardwareEncoder,
        finish: bool,
        video_end: i64,
    ) -> Result<(), String> {
        if finish {
            self.capture.stop_and_drain(|frame| {
                self.mixer
                    .push(frame.track, frame.timestamp, frame.channels, frame.samples)
            });
        } else {
            self.capture.read_frames(|frame| {
                self.mixer
                    .push(frame.track, frame.timestamp, frame.channels, frame.samples)
            });
        }
        if self.capture.had_source_failure() {
            return Err("An audio source failed; recording is incomplete.".into());
        }
        let dropped = self
            .capture
            .dropped_frames()
            .saturating_sub(self.initial_dropped);
        if dropped > 0 {
            return Err(format!(
                "Audio capture dropped {dropped} frames; recording is incomplete."
            ));
        }
        while let Ok(event) = self.capture.events().try_recv() {
            match event {
                AudioEvent::Failed { message, .. } => {
                    return Err(format!("Audio capture failed: {message}"));
                }
                AudioEvent::SourceLost { reason, .. } => {
                    return Err(format!("Audio source lost: {reason}"));
                }
                // Persistent counters above cover even throttled/lost events.
                AudioEvent::Overflow { .. } => {}
                _ => {}
            }
        }
        let video_limit = self.origin.saturating_add(video_end);
        let end = if finish {
            video_limit
        } else {
            qpc_now_ticks().saturating_sub(800_000).min(video_limit)
        };
        self.mixer.render_until(end, |track, time, samples| {
            encoder
                .submit_audio(track, samples, time)
                .map_err(|e| e.to_string())
        })?;
        if finish && !self.mixer.all_sources_received() {
            return Err(
                "An enabled audio source delivered no samples; clip is not reported as successful."
                    .into(),
            );
        }
        if self.mixer.late_frames() > 0 {
            return Err(format!(
                "Audio packets missed the bounded mixing window ({} frames); clip is not reported as successful.",
                self.mixer.late_frames()
            ));
        }
        Ok(())
    }
}

/// Explicit, bounded 30-second audio diagnostic. File writes happen only on
/// the recorder worker; the WASAPI threads continue to feed bounded rings.
pub struct AudioTest {
    audio: RecordingAudio,
    file: std::io::BufWriter<std::fs::File>,
    path: std::path::PathBuf,
    bytes: u32,
    pub wake: std::sync::mpsc::Receiver<()>,
    started: Instant,
    last_meter: Instant,
}

impl AudioTest {
    pub fn start(
        options: &RecordingAudioOptions,
        directory: &std::path::Path,
    ) -> Result<Self, String> {
        use std::io::Write;
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let (tx, wake) = std::sync::mpsc::sync_channel(1);
        let audio =
            RecordingAudio::start(options, tx)?.ok_or("Enable an audio source before testing.")?;
        let path = directory.join(format!("Clipper_AudioTest_{}.wav", qpc_now_ticks()));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let mut file = std::io::BufWriter::new(file);
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&36_u32.to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16_u32.to_le_bytes());
        header.extend_from_slice(&1_u16.to_le_bytes());
        header.extend_from_slice(&2_u16.to_le_bytes());
        header.extend_from_slice(&48_000_u32.to_le_bytes());
        header.extend_from_slice(&192_000_u32.to_le_bytes());
        header.extend_from_slice(&4_u16.to_le_bytes());
        header.extend_from_slice(&16_u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&0_u32.to_le_bytes());
        if let Err(error) = file.write_all(&header).and_then(|()| file.flush()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error.to_string());
        }
        Ok(Self {
            audio,
            file,
            path,
            bytes: 0,
            wake,
            started: Instant::now(),
            last_meter: Instant::now(),
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn meter(&mut self) -> Option<(f32, f32)> {
        if self.last_meter.elapsed() < Duration::from_millis(100) {
            return None;
        }
        self.last_meter = Instant::now();
        Some(self.audio.levels())
    }

    pub fn pump(&mut self, finish: bool) -> Result<(), String> {
        use std::io::Write;
        if finish {
            self.audio.capture.stop_and_drain(|f| {
                self.audio
                    .mixer
                    .push(f.track, f.timestamp, f.channels, f.samples)
            });
        } else {
            self.audio.capture.read_frames(|f| {
                self.audio
                    .mixer
                    .push(f.track, f.timestamp, f.channels, f.samples)
            });
        }
        if self.audio.capture.had_source_failure() {
            return Err("An audio source failed during the diagnostic.".into());
        }
        if self.audio.capture.dropped_frames() > self.audio.initial_dropped {
            return Err("Audio capture dropped frames during the diagnostic.".into());
        }
        while let Ok(event) = self.audio.capture.events().try_recv() {
            match event {
                AudioEvent::Failed { message, .. } => return Err(message),
                AudioEvent::SourceLost { reason, .. } => return Err(reason),
                AudioEvent::Overflow { .. } => {}
                _ => {}
            }
        }
        let end = qpc_now_ticks().saturating_sub(if finish { 0 } else { 800_000 });
        let file = &mut self.file;
        let bytes = &mut self.bytes;
        self.audio.mixer.render_until(end, |track, _, samples| {
            if track == 0 {
                for sample in samples {
                    file.write_all(&((*sample * 32767.0).round() as i16).to_le_bytes())
                        .map_err(|e| e.to_string())?;
                }
                *bytes += samples.len() as u32 * 2;
            }
            Ok::<_, String>(())
        })?;
        if finish && !self.audio.mixer.all_sources_received() {
            return Err("An enabled source delivered no samples.".into());
        }
        if self.audio.mixer.late_frames() > 0 {
            return Err("Audio packets missed the mixing window.".into());
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<std::path::PathBuf, String> {
        use std::io::{Seek, SeekFrom, Write};
        let result = (|| {
            self.pump(true)?;
            self.file.flush().map_err(|e| e.to_string())?;
            self.file
                .seek(SeekFrom::Start(4))
                .map_err(|e| e.to_string())?;
            self.file
                .write_all(&(36 + self.bytes).to_le_bytes())
                .map_err(|e| e.to_string())?;
            self.file
                .seek(SeekFrom::Start(40))
                .map_err(|e| e.to_string())?;
            self.file
                .write_all(&self.bytes.to_le_bytes())
                .map_err(|e| e.to_string())?;
            self.file.flush().map_err(|e| e.to_string())?;
            Ok(self.path.clone())
        })();
        let path = self.path.clone();
        drop(self);
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_sources_do_not_start_audio_workers() {
        let (wake, _) = std::sync::mpsc::sync_channel(1);
        let options = RecordingAudioOptions {
            desktop: false,
            microphone: false,
            ..Default::default()
        };
        assert!(RecordingAudio::start(&options, wake).unwrap().is_none());
    }

    #[test]
    fn unavailable_explicit_devices_fail_before_recording_starts() {
        let (wake, _) = std::sync::mpsc::sync_channel(1);
        let options = RecordingAudioOptions {
            desktop: true,
            microphone: false,
            output_device: Some("clipper-nonexistent-test-endpoint".into()),
            ..Default::default()
        };
        let result = RecordingAudio::start(&options, wake);
        assert!(matches!(result, Err(message) if message.contains("unavailable")));
    }
}
