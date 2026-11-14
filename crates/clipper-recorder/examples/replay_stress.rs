//! Hardware check of replay save/stop reliability (P0). Captures the first
//! monitor into a temp folder; sends no input to other applications.
//!
//! A: three saves back to back, replay keeps running.
//! B: save, then stop immediately; stop must finish and the save must end.
//! C: restart, save, stop.
//! D: stop while several saves are queued.
//!
//! `cargo run --release -p clipper-recorder --example replay_stress -- fill=12`
#[cfg(windows)]
fn main() {
    stress::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("replay_stress needs Windows");
}

#[cfg(windows)]
mod stress {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use async_channel::Receiver;
    use clipper_core::recording::{
        AudioSourceTarget, CaptureSource, QualityProfile, RecorderCommand, RecorderEvent,
        RecordingAudioOptions, RecordingAudioSource, RecordingOptions, ReplayOptions, SourceKind,
    };
    use clipper_recorder::Recorder;

    struct Run {
        events: Receiver<RecorderEvent>,
        t0: Instant,
        saved: Vec<PathBuf>,
        failed: Vec<String>,
        running: bool,
    }

    impl Run {
        /// Next event within `timeout`, tracking outcomes.
        fn next(&mut self, timeout: Duration) -> Option<RecorderEvent> {
            let deadline = Instant::now() + timeout;
            loop {
                if let Ok(event) = self.events.try_recv() {
                    let at = self.t0.elapsed().as_secs_f32();
                    match &event {
                        RecorderEvent::ReplayStatus(_) | RecorderEvent::AudioLevels(_) => {}
                        other => println!("{at:>7.2}s {other:?}"),
                    }
                    match &event {
                        RecorderEvent::ReplaySaved { path, .. } => self.saved.push(path.clone()),
                        RecorderEvent::ReplaySaveFailed { message } => {
                            self.failed.push(message.clone())
                        }
                        RecorderEvent::ReplayStarted { .. } => self.running = true,
                        RecorderEvent::ReplayStopped { .. } => self.running = false,
                        _ => {}
                    }
                    return Some(event);
                }
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        fn drain_for(&mut self, duration: Duration) {
            let end = Instant::now() + duration;
            while Instant::now() < end {
                self.next(Duration::from_millis(50));
            }
        }

        /// Waits until `outcomes` saves ended (saved + failed), at most 60 s.
        fn wait_outcomes(&mut self, outcomes: usize) -> bool {
            let deadline = Instant::now() + Duration::from_secs(60);
            while self.saved.len() + self.failed.len() < outcomes {
                if Instant::now() > deadline {
                    return false;
                }
                self.next(Duration::from_millis(100));
            }
            true
        }

        fn wait_running(&mut self, want: bool, limit: Duration) -> Option<Duration> {
            let start = Instant::now();
            while self.running != want {
                if start.elapsed() > limit {
                    return None;
                }
                self.next(Duration::from_millis(20));
            }
            Some(start.elapsed())
        }
    }

    fn options(output: PathBuf, cache: PathBuf, duration: Duration) -> ReplayOptions {
        ReplayOptions {
            recording: RecordingOptions {
                audio: RecordingAudioOptions {
                    sources: vec![
                        RecordingAudioSource {
                            id: 1,
                            target: AudioSourceTarget::Desktop(None),
                            gain_percent: 100,
                        },
                        RecordingAudioSource {
                            id: 2,
                            target: AudioSourceTarget::Microphone(None),
                            gain_percent: 100,
                        },
                    ],
                    separate_tracks: true,
                },
                width: 1920,
                height: 1080,
                fps: 30,
                bitrate_mbps: 16,
                profile: QualityProfile::Balanced,
                output_directory: output,
            },
            duration,
            memory_limit_bytes: 32 << 20,
            cache_directory: Some(cache),
            disk_limit_bytes: 4096 << 20,
        }
    }

    pub fn run() {
        let fill: u64 = std::env::args()
            .skip(1)
            .find_map(|arg| arg.strip_prefix("fill=").and_then(|v| v.parse().ok()))
            .unwrap_or(12);
        let fill = Duration::from_secs(fill);
        let base =
            std::env::temp_dir().join(format!("clipper-replay-stress-{}", std::process::id()));
        let output = base.join("clips");
        let cache = base.join("cache");
        println!("output={}", output.display());

        let recorder = Recorder::spawn(32, 64).expect("spawn recorder");
        let handle = recorder.handle();
        let mut run = Run {
            events: handle.events(),
            t0: Instant::now(),
            saved: Vec::new(),
            failed: Vec::new(),
            running: false,
        };
        handle.send(RecorderCommand::RefreshSources).expect("send");
        let source: CaptureSource = loop {
            match run.next(Duration::from_secs(10)) {
                Some(RecorderEvent::SourcesListed(sources)) => {
                    break sources
                        .into_iter()
                        .find(|s| s.kind == SourceKind::Monitor)
                        .expect("no monitor");
                }
                Some(_) => {}
                None => panic!("no sources"),
            }
        };
        let start = |run: &mut Run| {
            handle
                .send(RecorderCommand::StartReplay {
                    source: source.clone(),
                    options: options(output.clone(), cache.clone(), Duration::from_secs(10)),
                })
                .expect("send start");
            run.wait_running(true, Duration::from_secs(20))
                .expect("replay did not start");
            run.drain_for(fill);
        };
        let save = || handle.send(RecorderCommand::SaveReplay).expect("send save");
        let mut verdicts = Vec::new();

        println!("== A: three saves back to back");
        start(&mut run);
        for _ in 0..3 {
            save();
        }
        let ok = run.wait_outcomes(3);
        run.drain_for(Duration::from_secs(2));
        verdicts.push((
            "A",
            ok && run.saved.len() == 3 && run.running,
            format!("saved={} failed={:?}", run.saved.len(), run.failed),
        ));

        println!("== B: save then stop immediately");
        let before = run.saved.len() + run.failed.len();
        save();
        handle.send(RecorderCommand::StopReplay).expect("send stop");
        let stopped = run.wait_running(false, Duration::from_secs(15));
        let ended = run.wait_outcomes(before + 1);
        verdicts.push((
            "B",
            stopped.is_some() && ended,
            format!("stop took {stopped:?}, save ended={ended}"),
        ));

        println!("== C: restart, save, stop");
        let before = run.saved.len();
        start(&mut run);
        save();
        let ok = run.wait_outcomes(run.saved.len() + run.failed.len() + 1);
        handle.send(RecorderCommand::StopReplay).expect("send stop");
        let stopped = run.wait_running(false, Duration::from_secs(15));
        verdicts.push((
            "C",
            ok && run.saved.len() == before + 1 && stopped.is_some(),
            format!("stop took {stopped:?}"),
        ));

        println!("== D: stop with queued saves");
        let before = run.saved.len() + run.failed.len();
        start(&mut run);
        for _ in 0..4 {
            save();
        }
        handle.send(RecorderCommand::StopReplay).expect("send stop");
        let stopped = run.wait_running(false, Duration::from_secs(15));
        let ended = run.wait_outcomes(before + 4);
        verdicts.push((
            "D",
            stopped.is_some() && ended,
            format!("stop took {stopped:?}, all 4 ended={ended}"),
        ));

        println!("== E: write failure, then recovery");
        // A file where the clip folder should be: creating the folder fails.
        let blocked = base.join("not-a-folder");
        std::fs::write(&blocked, b"x").expect("create blocker file");
        let failures = run.failed.len();
        handle
            .send(RecorderCommand::StartReplay {
                source: source.clone(),
                options: options(blocked, cache.clone(), Duration::from_secs(10)),
            })
            .expect("send start");
        run.wait_running(true, Duration::from_secs(20))
            .expect("replay did not start");
        run.drain_for(fill);
        save();
        let failed = run.wait_outcomes(run.saved.len() + run.failed.len() + 1)
            && run.failed.len() == failures + 1;
        handle.send(RecorderCommand::StopReplay).expect("send stop");
        let stopped = run.wait_running(false, Duration::from_secs(15));
        let before = run.saved.len();
        start(&mut run);
        save();
        let recovered = run.wait_outcomes(run.saved.len() + run.failed.len() + 1)
            && run.saved.len() == before + 1;
        handle.send(RecorderCommand::StopReplay).expect("send stop");
        let stopped_again = run.wait_running(false, Duration::from_secs(15));
        verdicts.push((
            "E",
            failed && stopped.is_some() && recovered && stopped_again.is_some(),
            format!("failure reported={failed}, next save ok={recovered}"),
        ));

        let mut names: Vec<_> = run.saved.iter().collect();
        names.sort();
        names.dedup();
        println!(
            "clips: {} saved, {} unique paths, {} failures",
            run.saved.len(),
            names.len(),
            run.failed.len()
        );
        for path in &run.saved {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            println!("clip {} ({size} bytes)", path.display());
        }
        for (name, pass, detail) in &verdicts {
            println!("{name}: {} {detail}", if *pass { "PASS" } else { "FAIL" });
        }
        drop(recorder);
    }
}
