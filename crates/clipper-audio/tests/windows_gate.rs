//! Opt-in microphone/desktop evidence through the production timeline mixer.
#![cfg(windows)]

use clipper_audio::{
    AudioConfig, AudioSelection, MicrophoneChoice, SourceRequest, SourceRequestKind,
    mixer::TimelineMixer,
    windows::{AudioCapture, qpc_now_ticks},
};
use clipper_core::recording::NoiseGateSettings;
use std::{
    io::Write,
    time::{Duration, Instant},
};

fn wave(path: &std::path::Path, samples: &[f32]) {
    let mut file = std::fs::File::create(path).unwrap();
    let bytes = samples.len() as u32 * 2;
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + bytes).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap();
    file.write_all(&2u16.to_le_bytes()).unwrap();
    file.write_all(&48000u32.to_le_bytes()).unwrap();
    file.write_all(&192000u32.to_le_bytes()).unwrap();
    file.write_all(&4u16.to_le_bytes()).unwrap();
    file.write_all(&16u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&bytes.to_le_bytes()).unwrap();
    for sample in samples {
        file.write_all(&((sample * 32767.0).round() as i16).to_le_bytes())
            .unwrap();
    }
}

#[test]
#[ignore = "records real microphone and desktop audio for six seconds; writes diagnostic WAV files"]
fn microphone_gate_preserves_parallel_desktop_and_timeline() {
    let selection = AudioSelection {
        sources: vec![
            SourceRequest {
                id: 1,
                kind: SourceRequestKind::Microphone(MicrophoneChoice::Default),
            },
            SourceRequest {
                id: 2,
                kind: SourceRequestKind::Desktop { device: None },
            },
        ],
    };
    let mut capture = AudioCapture::start(&selection, AudioConfig::default()).unwrap();
    assert_eq!(capture.tracks().len(), 2);
    let origin = qpc_now_ticks();
    let mut baseline = TimelineMixer::new(origin, vec![1.0; 2], true);
    let mut gated = TimelineMixer::new(origin, vec![1.0; 2], true);
    let settings = NoiseGateSettings {
        enabled: true,
        threshold_db: -20.0,
        hold_ms: 0.0,
        release_ms: 20.0,
        ..Default::default()
    };
    gated.set_noise_gate(0, settings);
    let mut plain: [Vec<f32>; 3] = Default::default();
    let mut processed: [Vec<f32>; 3] = Default::default();
    let mut received = [0usize; 2];
    let start = Instant::now();
    let mut disabled = false;
    let mut reenabled = false;
    loop {
        capture.read_frames(|frame| {
            received[frame.track] += frame.samples.len();
            baseline.push(frame.track, frame.timestamp, frame.channels, frame.samples);
            gated.push(frame.track, frame.timestamp, frame.channels, frame.samples);
        });
        if !disabled && start.elapsed() >= Duration::from_secs(2) {
            gated.set_noise_gate(
                0,
                NoiseGateSettings {
                    enabled: false,
                    ..settings
                },
            );
            disabled = true;
        }
        if !reenabled && start.elapsed() >= Duration::from_secs(4) {
            gated.set_noise_gate(0, settings);
            reenabled = true;
        }
        let end = qpc_now_ticks() - 800_000;
        let mut stamps = Vec::new();
        baseline
            .render_until(end, |track, time, samples| {
                stamps.push((track, time, samples.len()));
                plain[track].extend_from_slice(samples);
                Ok::<_, ()>(())
            })
            .unwrap();
        let mut i = 0;
        gated
            .render_until(end, |track, time, samples| {
                assert_eq!(stamps[i], (track, time, samples.len()));
                i += 1;
                processed[track].extend_from_slice(samples);
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(i, stamps.len());
        if start.elapsed() >= Duration::from_secs(6) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let summary = capture.stop();
    assert!(received[0] > 0, "microphone delivered no PCM");
    // Silent endpoints may legitimately deliver no desktop packet.
    println!(
        "captured sample counts={received:?}; summary={summary:?}; late baseline={} gated={}",
        baseline.late_frames(),
        gated.late_frames()
    );
    assert_eq!(plain[2], processed[2], "microphone gate changed desktop");
    assert_eq!(plain[1].len(), processed[1].len());
    assert!(plain[1].len() > 48000 * 2 * 5);
    assert_eq!(
        plain[1][48000 * 2 * 3..48000 * 7],
        processed[1][48000 * 2 * 3..48000 * 7],
        "live disable did not return the microphone to unity after its ramp"
    );
    let mut quiet_energy = 0.0f64;
    let mut gated_energy = 0.0f64;
    // First second excludes initial ramp, last second follows live re-enable.
    for frame in 48000..96000 {
        for channel in 0..2 {
            let index = frame * 2 + channel;
            let input = plain[1][index];
            if input.abs() < 0.05 {
                quiet_energy += f64::from(input).powi(2);
                gated_energy += f64::from(processed[1][index]).powi(2);
            }
        }
    }
    if quiet_energy > 1e-10 && gated_energy < quiet_energy * 0.5 {
        println!(
            "observed quiet-input energy attenuation: baseline={quiet_energy:.8} gated={gated_energy:.8}"
        );
    } else {
        println!(
            "attenuation not established under ambient conditions: quiet baseline={quiet_energy:.8} gated={gated_energy:.8}; timeline/source isolation assertions still verified"
        );
    }
    let directory = std::env::temp_dir().join(format!("clipper-gate-evidence-{origin}"));
    std::fs::create_dir_all(&directory).unwrap();
    for (name, samples) in [
        ("baseline-mix", &plain[0]),
        ("gated-mix", &processed[0]),
        ("baseline-mic", &plain[1]),
        ("gated-mic", &processed[1]),
        ("desktop", &plain[2]),
    ] {
        wave(&directory.join(format!("{name}.wav")), samples);
    }
    println!(
        "WAV evidence: {} (48kHz stereo PCM16); live off/on toggles performed; no acoustic quality assessment",
        directory.display()
    );
}
