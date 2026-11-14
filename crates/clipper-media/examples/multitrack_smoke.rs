//! Native mixed-audio export smoke test on a real clip. The output is temporary.
//! `cargo run -p clipper-media --example multitrack_smoke -- <clip.mp4>`
#[cfg(windows)]
fn main() {
    use clipper_media::{
        audio, exact,
        player::{Player, PlayerEvent},
        trim,
    };
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};
    let source = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: <clip.mp4>"));
    let original = std::fs::read(&source).expect("read source");
    let info = trim::probe(&source).expect("probe source");
    let mut tracks = audio::discover(&source).expect("discover actual audio streams");
    println!(
        "source: {info:?}; {} discovered audio streams",
        tracks.len()
    );
    let waveform_started = Instant::now();
    let summaries = audio::waveform(&source)
        .recv_blocking()
        .expect("waveform worker response")
        .expect("extract real waveforms");
    assert_eq!(summaries.tracks.len(), tracks.len());
    for track in &tracks {
        let waveform = summaries
            .tracks
            .iter()
            .find(|waveform| waveform.stream_index == track.stream_index)
            .expect("summary retains source stream identity");
        assert!(waveform.buckets.len() <= 16_384);
        assert!(
            waveform
                .buckets
                .iter()
                .all(|bucket| bucket.peak.is_finite() && bucket.rms.is_finite())
        );
        let peak = waveform
            .buckets
            .iter()
            .map(|bucket| bucket.peak)
            .fold(0f32, f32::max);
        println!(
            "  waveform stream {}: {} buckets, real peak {:.4}",
            track.stream_index,
            waveform.buckets.len(),
            peak
        );
    }
    println!("waveforms generated in {:?}", waveform_started.elapsed());
    for (index, track) in tracks.iter_mut().enumerate() {
        track.gain_percent = 75;
        track.solo = index == 0;
        println!(
            "  stream {}: {} channels, {} Hz, gain {}%, solo {}",
            track.stream_index, track.channels, track.sample_rate, track.gain_percent, track.solo
        );
    }
    // Exercise the actual multitrack preview on the original, not only the
    // already mixed export. Content tests separately measure mute/solo/gain.
    let (source_player, source_events) = Player::open(&source, 1280);
    source_player.set_audio_tracks(tracks.clone());
    let preview_deadline = Instant::now() + Duration::from_secs(10);
    let mut preview_frames = 0;
    let mut preview_tracks = tracks.clone();
    while Instant::now() < preview_deadline && preview_frames < 30 {
        while let Ok(event) = source_events.try_recv() {
            match event {
                PlayerEvent::Loaded { .. } => source_player.play(),
                PlayerEvent::Frame(_) => {
                    preview_frames += 1;
                    if preview_frames == 10 {
                        if let Some(track) = preview_tracks.first_mut() {
                            track.muted = true;
                        }
                        source_player.set_audio_tracks(preview_tracks.clone());
                    } else if preview_frames == 20 {
                        if let Some(track) = preview_tracks.first_mut() {
                            track.muted = false;
                            track.gain_percent = 125;
                        }
                        source_player.set_audio_tracks(preview_tracks.clone());
                    }
                }
                PlayerEvent::AudioError(error) | PlayerEvent::Error(error) => {
                    panic!("source multitrack preview: {error}")
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(preview_frames >= 30, "real multitrack source must play");
    source_player.pause();
    let seek = info.duration / 2;
    source_player.seek(seek);
    let seek_deadline = Instant::now() + Duration::from_secs(5);
    let mut sought = false;
    while Instant::now() < seek_deadline && !sought {
        while let Ok(event) = source_events.try_recv() {
            match event {
                PlayerEvent::Frame(frame)
                    if frame.position.abs_diff(seek) < Duration::from_millis(100) =>
                {
                    sought = true
                }
                PlayerEvent::AudioError(error) | PlayerEvent::Error(error) => {
                    panic!("source seek: {error}")
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(sought, "paused source seek must deliver its target frame");
    source_player.play();
    drop(source_player);
    println!(
        "real multitrack preview: {preview_frames} frames, mute/gain updates and paused seek passed"
    );
    assert!(
        info.duration > Duration::from_secs(2),
        "use a clip longer than two seconds"
    );
    let part = info.duration / 8;
    let ranges = [
        (part + Duration::from_millis(75), part * 3),
        (part * 5 + Duration::from_millis(125), part * 7),
    ];
    let expected: Duration = ranges.iter().map(|(start, end)| *end - *start).sum();
    let folder =
        std::env::temp_dir().join(format!("clipuh-multitrack-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("create temporary output directory");
    let destination = folder.join("mixed.mp4");
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let outcome = exact::export_audio(
        &source,
        &destination,
        &ranges,
        &tracks,
        &mut |_| {},
        &cancel,
    )
    .expect("mixed export");
    let output = trim::probe(&destination).expect("probe export");
    println!(
        "export: {output:?}, requested {:?}, elapsed {:?}",
        outcome.duration,
        started.elapsed()
    );
    assert_eq!(output.audio_tracks, usize::from(!tracks.is_empty()));
    assert_eq!((output.width, output.height), (info.width, info.height));
    assert!(output.duration.abs_diff(expected) < Duration::from_millis(100));
    assert_eq!(
        std::fs::read(&source).expect("read unchanged source"),
        original
    );
    if !tracks.is_empty() {
        let decoded_tracks = audio::discover(&destination).expect("discover mixed output");
        assert_eq!(decoded_tracks.len(), 1);
        assert_eq!(
            (decoded_tracks[0].channels, decoded_tracks[0].sample_rate),
            (2, 48_000)
        );
        let mut decoder = audio::Decoder::open(&destination, &[decoded_tracks[0].stream_index])
            .expect("decode mixed audio");
        let mut frames = 0;
        let mut peak = 0.0f32;
        while let Some(chunk) = decoder.next(&cancel).expect("read output PCM") {
            assert!(chunk.samples.len() <= 8192 * 2);
            for sample in &chunk.samples {
                assert!(sample.is_finite() && sample.abs() <= 1.0);
                peak = peak.max(sample.abs());
            }
            frames += chunk.samples.len() / 2;
        }
        assert!(frames > 0);
        println!("mixed stereo PCM: {frames} frames, peak {peak:.4}");
    }
    let (player, events) = Player::open(&destination, 1280);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut loaded = false;
    let mut frames = 0;
    while Instant::now() < deadline {
        while let Ok(event) = events.try_recv() {
            match event {
                PlayerEvent::Loaded { has_audio, .. } => {
                    assert_eq!(has_audio, !tracks.is_empty());
                    loaded = true;
                    player.play();
                }
                PlayerEvent::Frame(_) => frames += 1,
                PlayerEvent::AudioError(message) | PlayerEvent::Error(message) => {
                    panic!("export playback: {message}")
                }
                _ => {}
            }
        }
        if frames >= 30 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(loaded && frames >= 30, "reopened export must load and play");
    drop(player);
    std::fs::remove_file(&destination).expect("remove temporary export");
    std::fs::remove_dir(&folder).expect("remove temporary directory");
    println!("OK: mixed export decoded, reopened, source unchanged");
}
#[cfg(not(windows))]
fn main() {}
