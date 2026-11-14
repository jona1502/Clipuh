//! Real-hardware validation tool for the audio engine.
//!
//! `audio_smoke [seconds=5] [--game <exe|pid>] [--discord] [--no-desktop] [--mic]
//!  [--test-tone]`
//!
//! Writes one 32-bit float WAV per track plus `mix.wav` into
//! `<temp>/clipper-audio-smoke/`. File I/O happens on the main thread only.

#[cfg(not(windows))]
fn main() {
    eprintln!("audio_smoke only runs on Windows");
}

#[cfg(windows)]
fn main() {
    if let Err(error) = imp::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::io::{BufWriter, Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use clipper_audio::mix::{clamp, mix_into_stereo};
    use clipper_audio::timeline::TrackTimeline;
    use clipper_audio::windows::{AudioCapture, capabilities, list_devices};
    use clipper_audio::{
        AudioConfig, AudioSelection, DeviceFlow, MicrophoneChoice, ProcessTarget, SAMPLE_RATE,
        SourceRequest, SourceRequestKind,
    };

    type Res<T> = Result<T, Box<dyn std::error::Error>>;

    /// Minimal 32-bit float WAV writer; sizes are patched in `finish`.
    struct WavWriter {
        file: BufWriter<File>,
        data_bytes: u64,
    }

    impl WavWriter {
        fn create(path: &Path, channels: u16) -> std::io::Result<Self> {
            let mut file = BufWriter::new(File::create(path)?);
            let block_align = channels * 4;
            file.write_all(b"RIFF")?;
            file.write_all(&0u32.to_le_bytes())?;
            file.write_all(b"WAVEfmt ")?;
            file.write_all(&16u32.to_le_bytes())?;
            file.write_all(&3u16.to_le_bytes())?;
            file.write_all(&channels.to_le_bytes())?;
            file.write_all(&SAMPLE_RATE.to_le_bytes())?;
            file.write_all(&(SAMPLE_RATE * u32::from(block_align)).to_le_bytes())?;
            file.write_all(&block_align.to_le_bytes())?;
            file.write_all(&32u16.to_le_bytes())?;
            file.write_all(b"data")?;
            file.write_all(&0u32.to_le_bytes())?;
            Ok(Self {
                file,
                data_bytes: 0,
            })
        }

        fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
            for s in samples {
                self.file.write_all(&s.to_le_bytes())?;
            }
            self.data_bytes += samples.len() as u64 * 4;
            Ok(())
        }

        fn finish(mut self) -> std::io::Result<()> {
            self.file.flush()?;
            let data = self.data_bytes as u32;
            let file = self.file.get_mut();
            file.seek(SeekFrom::Start(4))?;
            file.write_all(&(36 + data).to_le_bytes())?;
            file.seek(SeekFrom::Start(40))?;
            file.write_all(&data.to_le_bytes())?;
            file.flush()
        }
    }

    struct TrackOut {
        label: String,
        wav: WavWriter,
        path: PathBuf,
        timeline: TrackTimeline,
        /// Aligned samples as stereo, for the mix.
        stereo: Vec<f32>,
        frames_written: u64,
    }

    impl TrackOut {
        fn put(&mut self, samples: &[f32], channels: u16, scratch: &mut Vec<f32>) {
            let frames = samples.len() / usize::from(channels);
            if let Err(error) = self.wav.write(samples) {
                eprintln!("wav write failed: {error}");
            }
            scratch.clear();
            scratch.resize(frames * 2, 0.0);
            mix_into_stereo(scratch, samples, channels, 1.0);
            self.stereo.extend_from_slice(scratch);
            self.frames_written += frames as u64;
        }
    }

    fn dbfs(linear: f32) -> f32 {
        if linear <= 1e-6 {
            -120.0
        } else {
            20.0 * linear.log10()
        }
    }

    fn write_tone(path: &Path, seconds: u32) -> std::io::Result<()> {
        let frames = 48_000 * seconds;
        let data = frames * 4;
        let mut f = BufWriter::new(File::create(path)?);
        f.write_all(b"RIFF")?;
        f.write_all(&(36 + data).to_le_bytes())?;
        f.write_all(b"WAVEfmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&1u16.to_le_bytes())?;
        f.write_all(&2u16.to_le_bytes())?;
        f.write_all(&48_000u32.to_le_bytes())?;
        f.write_all(&(48_000u32 * 4).to_le_bytes())?;
        f.write_all(&4u16.to_le_bytes())?;
        f.write_all(&16u16.to_le_bytes())?;
        f.write_all(b"data")?;
        f.write_all(&data.to_le_bytes())?;
        for n in 0..frames {
            let v = ((n as f32 * 1000.0 * std::f32::consts::TAU / 48_000.0).sin()
                * 0.5
                * f32::from(i16::MAX)) as i16;
            f.write_all(&v.to_le_bytes())?;
            f.write_all(&v.to_le_bytes())?;
        }
        f.flush()
    }

    fn spawn_tone(dir: &Path, seconds: u32) -> Res<Child> {
        let path = dir.join("tone.wav");
        write_tone(&path, seconds)?;
        Ok(Command::new("powershell")
            .args([
                "-NoProfile",
                "-c",
                &format!(
                    "(New-Object Media.SoundPlayer '{}').PlaySync()",
                    path.display()
                ),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?)
    }

    pub fn run() -> Res<()> {
        let mut seconds = 5u64;
        let mut game = None;
        let mut discord = false;
        let mut desktop = true;
        let mut tone = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--game" => {
                    let v = args.next().ok_or("--game needs a value")?;
                    game = Some(match v.parse::<u32>() {
                        Ok(pid) => ProcessTarget::Pid(pid),
                        Err(_) => ProcessTarget::Executable(v),
                    });
                }
                "--discord" => discord = true,
                "--no-desktop" => desktop = false,
                // Microphone is on by default when a capture device exists.
                "--mic" => {}
                "--test-tone" => tone = true,
                other => {
                    seconds = other
                        .parse()
                        .map_err(|_| format!("bad argument: {other}"))?
                }
            }
        }

        let mut have_mic = false;
        for flow in [DeviceFlow::Render, DeviceFlow::Capture] {
            let devices = list_devices(flow)?;
            println!("{flow:?} devices: {}", devices.len());
            for d in &devices {
                println!(
                    "  {}{} [{} ch @ {} Hz] id={}",
                    if d.is_default { "* " } else { "  " },
                    d.name,
                    d.channels,
                    d.sample_rate,
                    d.id
                );
            }
            if flow == DeviceFlow::Capture {
                have_mic = !devices.is_empty();
            }
        }
        println!("capabilities: {:?}", capabilities());

        let mut sources = Vec::new();
        let mut kinds = Vec::new();
        kinds.extend(game.map(SourceRequestKind::Process));
        if discord {
            kinds.push(SourceRequestKind::Process(ProcessTarget::Executable(
                "Discord.exe".to_owned(),
            )));
        }
        if desktop {
            kinds.push(SourceRequestKind::Desktop { device: None });
        }
        if have_mic {
            kinds.push(SourceRequestKind::Microphone(MicrophoneChoice::Default));
        }
        for (id, kind) in kinds.into_iter().enumerate() {
            sources.push(SourceRequest {
                id: id as u32,
                kind,
            });
        }
        let selection = AudioSelection { sources };
        println!("selection: {selection:?}");

        let dir = std::env::temp_dir().join("clipper-audio-smoke");
        std::fs::create_dir_all(&dir)?;

        let mut capture = AudioCapture::start(&selection, AudioConfig::default())?;
        for w in capture.warnings() {
            println!("warning: {}", w.1.message());
        }
        let mut outs = Vec::new();
        for t in capture.tracks() {
            let label: String = t
                .content
                .label()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let path = dir.join(format!("track{}_{label}.wav", t.index));
            println!(
                "track {}: {} ({} ch, {} Hz) -> {}",
                t.index,
                t.content.label(),
                t.channels,
                t.sample_rate,
                path.display()
            );
            outs.push(TrackOut {
                label: t.content.label().to_owned(),
                wav: WavWriter::create(&path, t.channels)?,
                path,
                timeline: TrackTimeline::new(capture.started_at()),
                stereo: Vec::new(),
                frames_written: 0,
            });
        }

        let mut child = if tone {
            Some(spawn_tone(&dir, seconds as u32 + 1)?)
        } else {
            None
        };

        let start = Instant::now();
        let mut next_report = Duration::from_secs(1);
        let mut events_seen = 0usize;
        let mut scratch = Vec::<f32>::new();
        while start.elapsed() < Duration::from_secs(seconds) {
            capture.read_frames(|frame| {
                let out = &mut outs[frame.track];
                let frames = frame.frames() as u64;
                let a = out.timeline.align(frame.timestamp, frames);
                let ch = usize::from(frame.channels);
                if a.pad_frames > 0 {
                    let silence = vec![0.0f32; a.pad_frames as usize * ch];
                    out.put(&silence, frame.channels, &mut scratch);
                }
                let skip = (a.skip_frames.min(frames) as usize) * ch;
                if skip < frame.samples.len() {
                    out.put(&frame.samples[skip..], frame.channels, &mut scratch);
                }
            });
            while let Ok(event) = capture.events().try_recv() {
                events_seen += 1;
                println!("event: {event:?}");
            }
            if start.elapsed() >= next_report {
                next_report += Duration::from_secs(1);
                for (i, out) in outs.iter().enumerate() {
                    let l = capture.levels(i);
                    println!(
                        "[{:>4.1}s] track {i} {:<24} peak {:>7.1} dBFS rms {:>7.1} dBFS written {} frames, events so far {events_seen}",
                        start.elapsed().as_secs_f32(),
                        out.label,
                        dbfs(l.peak),
                        dbfs(l.rms),
                        out.frames_written,
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let summary = capture.stop();
        if let Some(c) = child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }

        println!("summary: {summary:?}");
        let mut max_frames = 0u64;
        let mut stereo_tracks = Vec::new();
        for (i, out) in outs.into_iter().enumerate() {
            let s = out.timeline.stats();
            println!(
                "track {i} {}: padded {} skipped {} drift {:.1} ppm (max {} frames), {} frames = {:.2}s -> {}",
                out.label,
                s.padded_frames,
                s.skipped_frames,
                s.drift_ppm,
                s.max_drift_frames,
                out.frames_written,
                out.frames_written as f64 / f64::from(SAMPLE_RATE),
                out.path.display()
            );
            max_frames = max_frames.max(out.frames_written);
            out.wav.finish()?;
            stereo_tracks.push(out.stereo);
        }

        // Mix: 10 ms blocks in timeline order, summed and clamped.
        let mix_path = dir.join("mix.wav");
        let mut mix = WavWriter::create(&mix_path, 2)?;
        let block = SAMPLE_RATE as usize / 100;
        let mut pos = 0usize;
        let mut buf = vec![0.0f32; block * 2];
        let mut mixed_frames = 0u64;
        while (pos as u64) < max_frames {
            buf.fill(0.0);
            for stereo in &stereo_tracks {
                let from = (pos * 2).min(stereo.len());
                let to = ((pos + block) * 2).min(stereo.len());
                mix_into_stereo(&mut buf, &stereo[from..to], 2, 1.0);
            }
            clamp(&mut buf);
            let n = block.min(max_frames as usize - pos);
            mix.write(&buf[..n * 2])?;
            mixed_frames += n as u64;
            pos += block;
        }
        mix.finish()?;
        println!(
            "mix: {} ({:.2}s)",
            mix_path.display(),
            mixed_frames as f64 / f64::from(SAMPLE_RATE)
        );
        Ok(())
    }
}
