//! End-to-end smoke test: record the first monitor twice for ~5 s each.
//!
//! Usage: `record_smoke [WIDTHxHEIGHT] [FPS] [performance|balanced|quality]`
//! (defaults: 1280x720 30 balanced).
use std::time::{Duration, Instant};

use clipper_core::recording::{
    QualityProfile, RecorderCommand, RecorderEvent, RecordingOptions, SourceKind,
};
use clipper_recorder::Recorder;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (width, height) = args
        .first()
        .and_then(|size| size.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1280, 720));
    let fps = args.get(1).and_then(|fps| fps.parse().ok()).unwrap_or(30);
    let profile = match args.get(2).map(String::as_str) {
        Some("performance") => QualityProfile::Performance,
        Some("quality") => QualityProfile::Quality,
        _ => QualityProfile::Balanced,
    };
    println!("target {width}x{height}@{fps} {profile:?}");

    let recorder = Recorder::spawn(8, 64).expect("spawn recorder");
    let handle = recorder.handle();
    let events = handle.events();
    let dir = std::env::temp_dir().join("clipper-smoke");

    handle.send(RecorderCommand::RefreshSources).expect("send");
    let sources = loop {
        match events.recv_blocking() {
            Ok(RecorderEvent::SourcesListed(sources)) => break sources,
            Ok(other) => println!("{other:?}"),
            Err(_) => return,
        }
    };
    println!("{} sources", sources.len());
    let Some(source) = sources.into_iter().find(|s| s.kind == SourceKind::Monitor) else {
        println!("no monitor source");
        return;
    };
    println!("using {source:?}");

    let seconds = args
        .get(3)
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    let rounds = args
        .get(4)
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);
    for round in 1..=rounds {
        println!("--- round {round}");
        handle
            .send(RecorderCommand::StartRecording {
                source: source.clone(),
                options: RecordingOptions {
                    audio: Default::default(),
                    width,
                    height,
                    fps,
                    bitrate_mbps: 16,
                    profile,
                    output_directory: dir.clone(),
                },
            })
            .expect("send");
        let mut stop_sent = false;
        let mut started_at = None;
        loop {
            if started_at.is_some_and(|t: Instant| t.elapsed() > Duration::from_secs(seconds))
                && !stop_sent
            {
                handle.send(RecorderCommand::StopRecording).expect("send");
                stop_sent = true;
            }
            let Ok(event) = events.recv_blocking() else {
                return;
            };
            println!("{event:?}");
            match event {
                RecorderEvent::RecordingStarted { .. } => started_at = Some(Instant::now()),
                RecorderEvent::RecordingFinished { path, .. } => {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    println!("file {} ({size} bytes)", path.display());
                    break;
                }
                RecorderEvent::RecordingFailed { .. } => break,
                _ => {}
            }
        }
    }

    handle.send(RecorderCommand::Shutdown).expect("send");
    while let Ok(event) = events.recv_blocking() {
        println!("{event:?}");
        if event == RecorderEvent::Stopped {
            break;
        }
    }
}
