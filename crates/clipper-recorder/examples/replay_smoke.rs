//! Hardware smoke test of the instant replay pipeline.
//!
//! Starts a replay on the first monitor, prints every `ReplayStatus`, saves
//! three clips in quick succession and a fourth five seconds later, waits for
//! all outcomes, stops the replay and prints every event. Process working set,
//! private bytes and CPU time are printed at start and end.
//!
//! Usage (all optional, `key=value`):
//! `cargo run --release -p clipper-recorder --example replay_smoke -- run=45
//! duration=30 desktop=1 mic=0 memory=64 setdur=60@20 fps=60 disk=4096`
//!
//! * `run`      seconds to wait before saving (default 45)
//! * `duration` replay length in seconds (default 30)
//! * `memory`   RAM budget in MiB (default 64)
//! * `disk`     SSD budget in MiB (default 4096)
//! * `setdur`   `SECONDS@AT`: send `SetReplayDuration` after AT seconds
//! * `desktop`/`mic` 1 or 0 (default 1 / 0)
#[cfg(windows)]
fn main() {
    smoke::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("replay_smoke needs Windows");
}

#[cfg(windows)]
mod smoke {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use clipper_core::recording::{
        QualityProfile, RecorderCommand, RecorderEvent, RecordingAudioOptions, RecordingOptions,
        ReplayOptions, SourceKind,
    };
    use clipper_recorder::Recorder;
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn filetime_secs(time: FILETIME) -> f64 {
        let ticks = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        ticks as f64 / 1e7
    }

    /// (working set MiB, private MiB, cpu seconds)
    #[allow(unsafe_code)]
    fn process_usage() -> (f64, f64, f64) {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let (mut create, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: the pseudo handle is always valid; the counters struct is
        // PROCESS_MEMORY_COUNTERS_EX whose prefix is PROCESS_MEMORY_COUNTERS
        // and `cb` holds its real size; all out-pointers are live locals.
        unsafe {
            let process = GetCurrentProcess();
            let _ = GetProcessMemoryInfo(
                process,
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX)
                    .cast::<PROCESS_MEMORY_COUNTERS>(),
                size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            );
            let _ = GetProcessTimes(process, &mut create, &mut exit, &mut kernel, &mut user);
        }
        let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
        (
            mib(counters.WorkingSetSize),
            mib(counters.PrivateUsage),
            filetime_secs(kernel) + filetime_secs(user),
        )
    }

    fn print_usage(label: &str) {
        let (working, private, cpu) = process_usage();
        println!(
            "[{label}] working_set={working:.1} MiB private={private:.1} MiB cpu_time={cpu:.2} s"
        );
    }

    fn args() -> HashMap<String, String> {
        std::env::args()
            .skip(1)
            .filter_map(|arg| {
                arg.split_once('=')
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
            })
            .collect()
    }

    pub fn run() {
        let args = args();
        let number = |key: &str, default: u64| -> u64 {
            args.get(key)
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let run_for = Duration::from_secs(number("run", 45));
        let duration = Duration::from_secs(number("duration", 30));
        let memory = number("memory", 64) << 20;
        let disk = number("disk", 4096) << 20;
        let fps = number("fps", 60) as u32;
        let desktop = number("desktop", 1) != 0;
        let mic = number("mic", 0) != 0;
        let set_duration = args.get("setdur").and_then(|value| {
            let (seconds, at) = value.split_once('@')?;
            Some((
                Duration::from_secs(seconds.parse().ok()?),
                Duration::from_secs(at.parse().ok()?),
            ))
        });

        let base =
            std::env::temp_dir().join(format!("clipper-replay-smoke-{}", std::process::id()));
        let output = base.join("clips");
        let cache = base.join("cache");
        println!("output={} cache={}", output.display(), cache.display());
        print_usage("start");

        let recorder = Recorder::spawn(16, 512).expect("spawn recorder");
        let handle = recorder.handle();
        let events = handle.events();
        let t0 = Instant::now();
        let wait = |timeout: Duration| {
            let deadline = Instant::now() + timeout;
            loop {
                if let Ok(event) = events.try_recv() {
                    return Some(event);
                }
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        };

        handle.send(RecorderCommand::RefreshSources).expect("send");
        let source = loop {
            match wait(Duration::from_secs(10)) {
                Some(RecorderEvent::SourcesListed(sources)) => {
                    break sources
                        .into_iter()
                        .find(|source| source.kind == SourceKind::Monitor)
                        .expect("no monitor");
                }
                Some(other) => println!("{:>7.2}s {other:?}", t0.elapsed().as_secs_f32()),
                None => panic!("no sources"),
            }
        };
        println!("source: {} {}x{}", source.name, source.width, source.height);

        handle
            .send(RecorderCommand::StartReplay {
                source,
                options: ReplayOptions {
                    recording: RecordingOptions {
                        audio: RecordingAudioOptions {
                            desktop,
                            microphone: mic,
                            separate_tracks: false,
                            ..Default::default()
                        },
                        width: 1920,
                        height: 1080,
                        fps,
                        bitrate_mbps: 16,
                        profile: QualityProfile::Balanced,
                        output_directory: output.clone(),
                    },
                    duration,
                    memory_limit_bytes: memory,
                    cache_directory: Some(cache.clone()),
                    disk_limit_bytes: disk,
                },
            })
            .expect("send start");

        let mut log: Vec<(f32, RecorderEvent)> = Vec::new();
        let mut started = false;
        let start_deadline = Instant::now() + Duration::from_secs(20);
        while !started {
            let event = wait(Duration::from_secs(1));
            if Instant::now() > start_deadline {
                panic!("replay did not start");
            }
            let Some(event) = event else { continue };
            println!("{:>7.2}s {event:?}", t0.elapsed().as_secs_f32());
            match event {
                RecorderEvent::ReplayStarted { .. } => started = true,
                RecorderEvent::ReplayStopped { error } => panic!("start failed: {error:?}"),
                _ => {}
            }
        }

        let begin = Instant::now();
        let mut set_sent = false;
        let mut max_tier = None;
        while begin.elapsed() < run_for {
            if let Some((seconds, at)) = set_duration
                && !set_sent
                && begin.elapsed() >= at
            {
                handle
                    .send(RecorderCommand::SetReplayDuration(seconds))
                    .expect("send");
                println!("-- SetReplayDuration({seconds:?})");
                set_sent = true;
            }
            if let Some(event) = wait(Duration::from_millis(200)) {
                let at = t0.elapsed().as_secs_f32();
                if let RecorderEvent::ReplayStatus(status) = &event {
                    let (working, private, cpu) = process_usage();
                    println!(
                        "{at:>7.2}s status buffered={:.1}s/{:.0}s tier={:?} ram={:.1}MiB disk={:.1}MiB fps={:.1} enc={} drop={} exports={} warn={:?} | ws={working:.0} priv={private:.0} cpu={cpu:.1}s",
                        status.buffered.as_secs_f32(),
                        status.configured.as_secs_f32(),
                        status.tier,
                        status.ram_bytes as f64 / 1048576.0,
                        status.disk_bytes as f64 / 1048576.0,
                        status.stats.current_fps,
                        status.stats.frames_encoded,
                        status.stats.frames_dropped,
                        status.exports_in_flight,
                        status.warning
                    );
                    max_tier = Some(status.tier);
                } else {
                    println!("{at:>7.2}s {event:?}");
                }
                log.push((at, event));
            }
        }

        let mut sent = Vec::new();
        let save = |label: &str, sent: &mut Vec<Instant>| {
            handle.send(RecorderCommand::SaveReplay).expect("send save");
            sent.push(Instant::now());
            println!("-- SaveReplay {label}");
        };
        for index in 0..3 {
            save(&format!("burst {index}"), &mut sent);
        }
        let pause = Instant::now() + Duration::from_secs(5);
        while Instant::now() < pause {
            if let Some(event) = wait(Duration::from_millis(100)) {
                println!("{:>7.2}s {event:?}", t0.elapsed().as_secs_f32());
                log.push((t0.elapsed().as_secs_f32(), event));
            }
        }
        save("after 5 s", &mut sent);

        let mut outcomes = 0;
        let mut clips = Vec::new();
        for (_, event) in &log {
            match event {
                RecorderEvent::ReplaySaved { path, .. } => {
                    outcomes += 1;
                    clips.push(path.clone());
                }
                RecorderEvent::ReplaySaveFailed { .. } => outcomes += 1,
                _ => {}
            }
        }
        let deadline = Instant::now() + Duration::from_secs(90);
        while outcomes < 4 && Instant::now() < deadline {
            if let Some(event) = wait(Duration::from_millis(200)) {
                let at = t0.elapsed().as_secs_f32();
                println!("{at:>7.2}s {event:?}");
                match &event {
                    RecorderEvent::ReplaySaved { path, .. } => {
                        outcomes += 1;
                        clips.push(path.clone());
                    }
                    RecorderEvent::ReplaySaveFailed { .. } => outcomes += 1,
                    _ => {}
                }
                log.push((at, event));
            }
        }

        handle.send(RecorderCommand::StopReplay).expect("send stop");
        let stop_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let Some(event) = wait(Duration::from_millis(200)) else {
                assert!(Instant::now() < stop_deadline, "no ReplayStopped");
                continue;
            };
            println!("{:>7.2}s {event:?}", t0.elapsed().as_secs_f32());
            if matches!(event, RecorderEvent::ReplayStopped { .. }) {
                break;
            }
        }
        print_usage("end");
        println!("last tier={max_tier:?} outcomes={outcomes}/4");
        for clip in &clips {
            println!("clip {}", clip.display());
        }
        drop(recorder);
        println!("recorder joined; cache left over: {}", cache.exists());
    }
}
