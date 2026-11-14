//! End-to-end check of the clip editor's media path on a real clip:
//! play, seek while paused, trim the middle and play the export.
//!
//! `cargo run -p clipper-media --example edit_smoke -- <clip.mp4>`
#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    use clipper_media::player::{Player, PlayerEvent};
    use clipper_media::{exact, trim};

    let source = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: <clip.mp4>"));
    let before = std::fs::read(&source).expect("read source");
    let info = trim::probe(&source).expect("probe source");
    println!("source: {info:?}");

    // Collects events until `stop` returns true or the timeout passes.
    let drain = |events: &async_channel::Receiver<PlayerEvent>,
                 timeout: Duration,
                 stop: &mut dyn FnMut(&PlayerEvent) -> bool| {
        let deadline = Instant::now() + timeout;
        let mut frames = 0;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(Duration::from_millis(5).min(left));
            while let Ok(event) = events.try_recv() {
                match &event {
                    PlayerEvent::Frame(_) => frames += 1,
                    PlayerEvent::Error(message) => panic!("player error: {message}"),
                    _ => {}
                }
                if stop(&event) {
                    return frames;
                }
            }
        }
        frames
    };

    let check_playback = |path: &std::path::Path, label: &str| {
        let (player, events) = Player::open(path, 1280);
        let mut loaded = None;
        drain(&events, Duration::from_secs(10), &mut |event| {
            if let PlayerEvent::Loaded {
                duration,
                width,
                height,
                has_audio,
            } = event
            {
                loaded = Some((*duration, *width, *height, *has_audio));
                true
            } else {
                false
            }
        });
        let loaded = loaded.expect("Loaded event");
        println!("{label}: loaded {loaded:?}");
        player.play();
        let started = Instant::now();
        let frames = drain(&events, Duration::from_secs(2), &mut |_| false);
        let fps = frames as f64 / started.elapsed().as_secs_f64();
        println!("{label}: {frames} preview frames in 2 s while playing ({fps:.1} fps)");
        assert!(frames > 10, "playback delivers frames");
        player.pause();
        drain(&events, Duration::from_millis(300), &mut |_| false);
        let target = loaded.0 / 2;
        player.seek(target);
        let mut seek_frame = None;
        drain(&events, Duration::from_secs(3), &mut |event| {
            if std::env::var_os("EDIT_SMOKE_TRACE").is_some() {
                eprintln!("  {event:?}");
            }
            if let PlayerEvent::Frame(frame) = event {
                seek_frame = Some((frame.position, frame.width, frame.height));
                true
            } else {
                false
            }
        });
        let seek_frame = seek_frame.expect("frame after paused seek");
        println!("{label}: paused seek to {target:?} -> frame {seek_frame:?}");
        let dropped = Instant::now();
        drop(player);
        println!("{label}: player released in {:?}", dropped.elapsed());
    };

    check_playback(&source, "source");

    let (start, end) = (info.duration / 3, info.duration * 2 / 3);
    let destination = trim::export_path(&source, start, end);
    let began = Instant::now();
    let quarter = info.duration / 4;
    let ranges = [
        (Duration::from_millis(150), quarter),
        (quarter * 2 + Duration::from_millis(75), quarter * 3),
    ];
    let expected: Duration = ranges.iter().map(|(start, end)| *end - *start).sum();
    let outcome = exact::export(
        &source,
        &destination,
        &ranges,
        &mut |_| {},
        &std::sync::atomic::AtomicBool::new(false),
    )
    .expect("exact segment export");
    println!(
        "exact ranges {ranges:?} -> {outcome:?} in {:?}",
        began.elapsed()
    );
    let exported = trim::probe(&destination).expect("probe export");
    println!("export: {exported:?}");
    assert_eq!(exported.audio_tracks, info.audio_tracks);
    assert_eq!(outcome.start, ranges[0].0);
    assert!(exported.duration.abs_diff(expected) < Duration::from_millis(100));
    assert_eq!(
        std::fs::read(&source).expect("reread source"),
        before,
        "source unchanged"
    );

    check_playback(&destination, "export");
    std::fs::remove_file(&destination).expect("remove export");
    println!("OK");
}

#[cfg(not(windows))]
fn main() {}
