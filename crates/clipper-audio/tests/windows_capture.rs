//! Real-hardware integration tests for the Windows audio engine. They play a
//! tone through the default render device and capture it back. Tests are
//! serialized because they share the audio devices.
#![cfg(windows)]

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use clipper_audio::windows::{AudioCapture, capabilities, list_devices};
use clipper_audio::{
    AudioConfig, AudioEvent, AudioSelection, DeviceFlow, MicrophoneChoice, ProcessTarget,
    SourceRequest, SourceRequestKind, TrackContent,
};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Aborts the whole test process if a test hangs.
fn watchdog(seconds: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        eprintln!("WATCHDOG: test exceeded {seconds} s, aborting");
        std::process::exit(101);
    });
}

fn tone_path(seconds: u32) -> PathBuf {
    let dir = std::env::temp_dir().join("clipper-audio-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("tone_{seconds}s.wav"));
    let frames = 48_000 * seconds;
    let data = frames * 4;
    let mut f = BufWriter::new(File::create(&path).unwrap());
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&2u16.to_le_bytes()).unwrap();
    f.write_all(&48_000u32.to_le_bytes()).unwrap();
    f.write_all(&(48_000u32 * 4).to_le_bytes()).unwrap();
    f.write_all(&4u16.to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data.to_le_bytes()).unwrap();
    for n in 0..frames {
        let v = ((n as f32 * 1000.0 * std::f32::consts::TAU / 48_000.0).sin()
            * 0.5
            * f32::from(i16::MAX)) as i16;
        f.write_all(&v.to_le_bytes()).unwrap();
        f.write_all(&v.to_le_bytes()).unwrap();
    }
    f.flush().unwrap();
    path
}

/// Plays a 1 kHz stereo tone through a separate powershell process.
fn play_tone(seconds: u32) -> Child {
    let path = tone_path(seconds);
    Command::new("powershell")
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
        .spawn()
        .expect("spawn powershell")
}

fn kill(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Default, Clone, Copy)]
struct TrackStats {
    frames: u64,
    peak: f32,
}

/// Reads for `duration`, returning per-track frame count and sample peak.
fn read_for(capture: &mut AudioCapture, duration: Duration) -> Vec<TrackStats> {
    let mut stats = vec![TrackStats::default(); capture.tracks().len()];
    let end = Instant::now() + duration;
    while Instant::now() < end {
        capture.read_frames(|f| {
            let s = &mut stats[f.track];
            s.frames += f.frames() as u64;
            for v in f.samples {
                s.peak = s.peak.max(v.abs());
            }
        });
        std::thread::sleep(Duration::from_millis(10));
    }
    stats
}

fn desktop_only() -> AudioSelection {
    AudioSelection {
        sources: vec![SourceRequest {
            id: 1,
            kind: SourceRequestKind::Desktop { device: None },
        }],
    }
}

#[test]
#[ignore = "needs audio hardware; plays a test tone"]
fn desktop_loopback_captures_tone() {
    let _g = serial();
    watchdog(60);
    let mut capture = AudioCapture::start(&desktop_only(), AudioConfig::default()).unwrap();
    assert_eq!(capture.tracks().len(), 1);
    assert_eq!(capture.tracks()[0].content, TrackContent::DesktopFull);
    let tone = play_tone(4);
    let stats = read_for(&mut capture, Duration::from_millis(3500));
    let level = capture.levels(0);
    let summary = capture.stop();
    kill(tone);
    println!(
        "desktop: frames {} ({:.2}s) sample-peak {:.3} last-level peak {:.3} summary {summary:?}",
        stats[0].frames,
        stats[0].frames as f64 / 48_000.0,
        stats[0].peak,
        level.peak
    );
    assert!(stats[0].frames >= 24_000, "frames {}", stats[0].frames);
    assert!(stats[0].peak > 0.05, "peak {}", stats[0].peak);
}

#[test]
#[ignore = "needs audio hardware; plays a test tone"]
fn game_and_remainder_do_not_double_capture() {
    let _g = serial();
    watchdog(60);
    if !capabilities().process_loopback {
        println!("process loopback unsupported: test skipped (not run)");
        return;
    }
    // Ambient desktop audio (other apps) is unrelated to the tone and would
    // break the strict ratio, so measure it first.
    let mut probe = AudioCapture::start(&desktop_only(), AudioConfig::default()).unwrap();
    let ambient = read_for(&mut probe, Duration::from_secs(1))[0].peak;
    probe.stop();
    println!("ambient desktop peak before tone: {ambient:.3}");
    let tone = play_tone(5);
    let selection = AudioSelection {
        sources: vec![
            SourceRequest {
                id: 1,
                kind: SourceRequestKind::Process(ProcessTarget::Pid(tone.id())),
            },
            SourceRequest {
                id: 2,
                kind: SourceRequestKind::Desktop { device: None },
            },
        ],
    };
    let mut capture = AudioCapture::start(&selection, AudioConfig::default()).unwrap();
    let contents: Vec<_> = capture.tracks().iter().map(|t| t.content.clone()).collect();
    println!("tracks: {contents:?}");
    let game = contents
        .iter()
        .position(|c| matches!(c, TrackContent::Application(_)))
        .expect("game track");
    let rest = contents
        .iter()
        .position(|c| matches!(c, TrackContent::DesktopExcluding(_)))
        .expect("desktop remainder track");
    let stats = read_for(&mut capture, Duration::from_millis(4000));
    capture.stop();
    kill(tone);
    println!(
        "game: frames {} peak {:.3}; remainder: frames {} peak {:.3}",
        stats[game].frames, stats[game].peak, stats[rest].frames, stats[rest].peak
    );
    assert!(stats[game].peak > 0.05, "game peak {}", stats[game].peak);
    // A double capture would make the remainder about as loud as the game.
    assert!(
        stats[game].peak > 2.0 * stats[rest].peak,
        "game {} vs remainder {}",
        stats[game].peak,
        stats[rest].peak
    );
    if ambient < 0.05 {
        assert!(
            stats[game].peak > 4.0 * stats[rest].peak,
            "game {} vs remainder {}",
            stats[game].peak,
            stats[rest].peak
        );
    } else {
        println!("ambient audio present: strict 4x check not applied");
    }
}

#[test]
#[ignore = "needs audio hardware; plays a test tone"]
fn start_stop_five_times() {
    let _g = serial();
    watchdog(30);
    for i in 0..5 {
        let t = Instant::now();
        let mut capture = AudioCapture::start(&desktop_only(), AudioConfig::default()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        capture.read_frames(|_| {});
        let summary = capture.stop();
        println!("cycle {i}: {:?} in {:?}", summary, t.elapsed());
        assert_eq!(summary.frames.len(), 1);
    }
}

#[test]
#[ignore = "needs audio hardware; plays a test tone"]
fn microphone_delivers_frames() {
    let _g = serial();
    watchdog(30);
    let devices = list_devices(DeviceFlow::Capture).unwrap();
    if devices.is_empty() {
        println!("no capture device: test skipped (not run)");
        return;
    }
    println!("capture devices: {}", devices.len());
    let selection = AudioSelection {
        sources: vec![SourceRequest {
            id: 1,
            kind: SourceRequestKind::Microphone(MicrophoneChoice::Default),
        }],
    };
    let mut capture = AudioCapture::start(&selection, AudioConfig::default()).unwrap();
    let ch = capture.tracks()[0].channels;
    let stats = read_for(&mut capture, Duration::from_secs(1));
    let summary = capture.stop();
    println!(
        "mic: {ch} ch, frames {} peak {:.4} summary {summary:?}",
        stats[0].frames, stats[0].peak
    );
    assert!(stats[0].frames > 0);
}

#[test]
#[ignore = "needs audio hardware; plays a test tone"]
fn overflow_is_counted_and_capture_recovers() {
    let _g = serial();
    watchdog(60);
    let config = AudioConfig {
        ring_ms: 10,
        ..Default::default()
    };
    let tone = play_tone(8);
    let mut capture = AudioCapture::start(&desktop_only(), config).unwrap();
    // Do not read while the tone starts and plays.
    std::thread::sleep(Duration::from_millis(3500));
    let mut overflow_events = 0;
    while let Ok(event) = capture.events().try_recv() {
        if matches!(event, AudioEvent::Overflow { .. }) {
            overflow_events += 1;
        }
    }
    // Resume reading: frames must flow again.
    let stats = read_for(&mut capture, Duration::from_millis(1500));
    // Drop every pending notification: final reliability must not depend
    // on the lossy, throttled event channel.
    while capture.events().try_recv().is_ok() {}
    let persistent_drops = capture.dropped_frames();
    let summary = capture.stop_and_drain(|_| {});
    assert!(persistent_drops > 0);
    assert_eq!(
        summary.dropped_frames.iter().sum::<u64>(),
        capture.dropped_frames()
    );
    kill(tone);
    println!(
        "overflow: dropped {:?}, overflow events {overflow_events}, frames after resume {}, summary {summary:?}",
        summary.dropped_frames, stats[0].frames
    );
    assert!(
        summary.dropped_frames[0] > 0 || overflow_events > 0,
        "no overflow observed"
    );
    assert!(stats[0].frames > 0, "no frames after reading resumed");
}

#[test]
#[ignore = "needs audio hardware; plays a test tone"]
fn missing_executable_reports_source_lost() {
    let _g = serial();
    watchdog(30);
    if !capabilities().process_loopback {
        println!("process loopback unsupported: test skipped (not run)");
        return;
    }
    let selection = AudioSelection {
        sources: vec![SourceRequest {
            id: 1,
            kind: SourceRequestKind::Process(ProcessTarget::Executable(
                "clipper_not_running_xyz.exe".to_owned(),
            )),
        }],
    };
    let mut capture = AudioCapture::start(&selection, AudioConfig::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    let mut lost = false;
    let mut frames = 0u64;
    while Instant::now() < end && !lost {
        capture.read_frames(|f| frames += f.frames() as u64);
        while let Ok(event) = capture.events().try_recv() {
            println!("event: {event:?}");
            lost |= matches!(event, AudioEvent::SourceLost { .. });
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let t = Instant::now();
    capture.stop();
    println!("lost {lost}, frames {frames}, stop took {:?}", t.elapsed());
    assert!(lost, "no SourceLost event within 3 s");
    assert_eq!(frames, 0);
    assert!(t.elapsed() < Duration::from_secs(2));
}
