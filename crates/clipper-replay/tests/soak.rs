//! Soak, concurrency and recovery tests of the replay engine. Public API
//! only; synthetic 60 fps H.264 (AVCC shaped) + AAC packets.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use clipper_replay::ReplayError;
use clipper_replay::disk::DiskConfig;
use clipper_replay::export::{ExportJob, Exporter, SavedClip, export_clip};
use clipper_replay::hybrid::{BufferTier, GopRef, ReplayBuffer, ReplayConfig, ReplaySnapshot};
use common::*;

type Results = Arc<Mutex<Vec<(usize, Result<SavedClip, ReplayError>)>>>;

const MBPS: u64 = 1_000_000;
const GOP_SECS: f64 = 2.0;
const SEG_BATCH_SLACK: u64 = 64 * MIB as u64;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn frames_for(seconds: u64) -> u64 {
    seconds * 60
}

fn config(duration: u64, budget: usize, disk: Option<(&Path, u64)>) -> ReplayConfig {
    ReplayConfig {
        duration: secs(duration),
        memory_limit_bytes: budget,
        disk: disk.map(|(root, max_bytes)| DiskConfig {
            root: root.to_path_buf(),
            max_bytes,
        }),
    }
}

/// Feeds up to `frames` total frames. With `wait_ram = Some(bytes)` the feed
/// is throttled at every GOP boundary until the buffer's RAM use fell to
/// `bytes`: this models a real 60 fps capture (about 2 MB/s) that the spill
/// thread easily keeps up with, while the synthetic stream is far faster than
/// real time and would otherwise outrun the SSD on a loaded machine.
fn feed(
    synth: &mut Synth,
    buf: &mut ReplayBuffer,
    frames: u64,
    wait_ram: Option<usize>,
) -> Result<(), ReplayError> {
    while synth.frames() < frames {
        let target = (synth.frames() / GOP_FRAMES + 1) * GOP_FRAMES;
        synth.run_frames(target.min(frames), |p| buf.push(p))?;
        if let Some(limit) = wait_ram
            && synth.frames().is_multiple_of(GOP_FRAMES)
        {
            std::thread::sleep(Duration::from_millis(2));
            wait_for(secs(30), || buf.stats().ram_bytes <= limit);
        }
    }
    Ok(())
}

fn export(snapshot: ReplaySnapshot, dir: &Path, name: &str) -> Result<SavedClip, ReplayError> {
    export_clip(&ExportJob {
        snapshot,
        final_path: dir.join(format!("{name}.mp4")),
    })
}

fn assert_contiguous(snapshot: &ReplaySnapshot) {
    assert!(!snapshot.gops.is_empty());
    let mut last: Option<(u64, i64)> = None;
    for gop in &snapshot.gops {
        let key = (gop.sequence(), gop.start_pts());
        if let Some((seq, pts)) = last {
            assert_eq!(key.0, seq + 1, "snapshot sequences must be contiguous");
            assert!(key.1 > pts, "snapshot start_pts must increase");
        }
        last = Some(key);
        gop.load().expect("snapshot gop loads");
    }
}

/// Clip length contract: >= requested when enough footage was buffered, never
/// more than requested + one GOP (+ a frame of rounding).
fn assert_clip(clip: &SavedClip, requested: Duration, buffered: Duration) {
    assert!(clip.path.is_file(), "clip file missing");
    assert!(clip.bytes > 0);
    assert_eq!(std::fs::metadata(&clip.path).unwrap().len(), clip.bytes);
    let max = requested + Duration::from_millis(2100);
    assert!(
        clip.duration <= max.max(buffered),
        "clip {:?} longer than requested {:?} + 2.1 s",
        clip.duration,
        requested
    );
    if buffered >= requested {
        assert!(
            clip.duration >= requested,
            "clip {:?} shorter than requested {:?} with {:?} buffered",
            clip.duration,
            requested,
            buffered
        );
    }
}

#[test]
fn ram_only_two_hour_session() {
    let budget = 64 * MIB;
    let mut synth = Synth::new(1, 16 * MBPS);
    let mut buf = ReplayBuffer::new(config(30, budget, None), layout(1, 1920, 1080)).unwrap();
    let clips = TempDir::new("ram-long");
    let total = frames_for(2 * 3600);
    let checkpoints = [100, frames_for(25), frames_for(3600), total];
    let mut next_check = 0;
    let mut peak = 0usize;
    let mut max_buffered = Duration::ZERO;

    while synth.frames() < total {
        let target = (synth.frames() + 600).min(total);
        feed(&mut synth, &mut buf, target, None).unwrap();
        let stats = buf.stats();
        peak = peak.max(stats.ram_bytes);
        max_buffered = max_buffered.max(stats.buffered);
        assert!(
            stats.ram_bytes <= budget,
            "ram {} over budget at frame {}",
            stats.ram_bytes,
            synth.frames()
        );
        assert_eq!(stats.disk_bytes, 0);
        assert_eq!(stats.tier, BufferTier::Ram);
        // Phase 1 retention is 30 s, then the replay is widened so that the
        // RAM budget (about 32 s at 16 Mbps) becomes the binding limit.
        if synth.frames() < frames_for(3600) {
            assert!(
                stats.buffered.as_secs_f64() <= 30.0 + 2.0 * GOP_SECS + 0.1,
                "buffered {:?} over duration + 2 GOPs",
                stats.buffered
            );
        } else {
            assert!(stats.buffered.as_secs_f64() <= 120.0);
        }
        if synth.frames() == frames_for(3600) {
            buf.set_duration(secs(600));
        }
        while next_check < checkpoints.len() && synth.frames() >= checkpoints[next_check] {
            next_check += 1;
            let requested = secs(20);
            let buffered = buf.stats().buffered;
            let snapshot = buf.snapshot(requested).unwrap();
            assert_contiguous(&snapshot);
            let clip = export(snapshot, clips.path(), &format!("clip{next_check}")).unwrap();
            assert_clip(&clip, requested, buffered);
            std::fs::remove_file(&clip.path).unwrap();
        }
    }
    assert!(peak > budget / 2, "budget never approached: {peak}");
    println!("peak ram {peak}, max buffered {max_buffered:?}");
}

#[test]
fn hybrid_session_with_real_disk() {
    let root = TempDir::new("hybrid-root");
    let clips = TempDir::new("hybrid-clips");
    std::fs::write(root.path().join("foreign.txt"), b"keep me").unwrap();
    std::fs::create_dir(root.path().join("other")).unwrap();
    std::fs::write(root.path().join("other").join("keep.bin"), b"keep").unwrap();

    let mut synth = Synth::new(2, 16 * MBPS);
    let mut buf = ReplayBuffer::new(
        config(120, 16 * MIB, Some((root.path(), 512 * MIB as u64))),
        layout(2, 1920, 1080),
    )
    .unwrap();
    let sessions = session_dirs(root.path());
    assert_eq!(sessions.len(), 1);
    let session = sessions[0].clone();

    let bound = 120.0 * 2.0 * MIB as f64 + SEG_BATCH_SLACK as f64;
    let total = frames_for(240);
    let mut saw_hybrid = false;
    let mut peak_files = 0usize;
    while synth.frames() < total {
        let target = (synth.frames() + GOP_FRAMES * 5).min(total);
        feed(&mut synth, &mut buf, target, Some(16 * MIB * 7 / 4)).unwrap();
        let stats = buf.stats();
        saw_hybrid |= stats.tier == BufferTier::Hybrid;
        assert!(stats.ram_bytes <= 32 * MIB, "ram {}", stats.ram_bytes);
        assert!(
            stats.buffered.as_secs_f64() <= 120.0 + 2.0 * GOP_SECS + 0.1,
            "buffered {:?}",
            stats.buffered
        );
        let on_disk: u64 = files_with_suffix(&session, ".seg")
            .iter()
            .map(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
            .sum();
        assert!((on_disk as f64) <= bound, "disk files {on_disk} > {bound}");
        peak_files = peak_files.max(files_with_suffix(&session, ".seg").len());
    }
    assert!(saw_hybrid, "SSD tier never used");
    assert_eq!(
        buf.stats().dropped_gops,
        0,
        "paced feed must not drop footage"
    );

    // Spill thread idle: files on disk must match the stats exactly.
    let settled = wait_for(secs(10), || {
        let stats = buf.stats();
        let files = files_with_suffix(&session, ".seg");
        let on_disk: u64 = files
            .iter()
            .map(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
            .sum();
        on_disk == stats.disk_bytes && files_with_suffix(&session, ".seg.tmp").is_empty()
    });
    assert!(
        settled,
        "segment files do not match stats: {:?}",
        buf.stats()
    );
    assert!(files_with_suffix(&session, ".seg.tmp").is_empty());
    let stats = buf.stats();
    assert!(stats.disk_bytes > 0 && stats.disk_gops > 0);
    assert!(stats.buffered >= secs(100), "buffered {:?}", stats.buffered);
    println!(
        "hybrid: peak {peak_files} seg files, disk {} B, dropped {}",
        stats.disk_bytes, stats.dropped_gops
    );

    // Clip spanning the SSD tier.
    let requested = secs(90);
    let snapshot = buf.snapshot(requested).unwrap();
    assert!(
        snapshot
            .gops
            .iter()
            .any(|g| matches!(g, GopRef::Disk { .. }))
    );
    assert_contiguous(&snapshot);
    let clip = export(snapshot, clips.path(), "hybrid").unwrap();
    assert_clip(&clip, requested, stats.buffered);
    assert_eq!(clip.audio_tracks, 2);

    drop(buf);
    assert!(
        !session.exists(),
        "session dir must be removed after dropping buffer and snapshots"
    );
    assert!(root.path().join("foreign.txt").is_file());
    assert!(root.path().join("other").join("keep.bin").is_file());
    assert!(session_dirs(root.path()).is_empty());
}

#[test]
fn concurrent_saves_with_live_buffer() {
    let clips = TempDir::new("concurrent");
    let mut synth = Synth::new(1, 16 * MBPS);
    let mut buf = ReplayBuffer::new(config(60, 64 * MIB, None), layout(1, 1920, 1080)).unwrap();
    feed(&mut synth, &mut buf, frames_for(20), None).unwrap();

    let jobs: Vec<ExportJob> = (0..10)
        .map(|i| {
            feed(&mut synth, &mut buf, frames_for(20) + 30 * (i + 1), None).unwrap();
            ExportJob {
                snapshot: buf.snapshot(secs(15)).unwrap(),
                final_path: clips.path().join(format!("job{i}.mp4")),
            }
        })
        .collect();

    let exporter = Exporter::spawn(3).unwrap();
    let calls: Vec<Arc<AtomicUsize>> = (0..10).map(|_| Arc::default()).collect();
    let results: Results = Arc::default();
    let busy = AtomicUsize::new(0);
    let accepted = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, ExportJob)>();
    let rx = Mutex::new(rx);

    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                loop {
                    let item = rx.lock().unwrap().try_recv();
                    let Ok((i, job)) = item else { break };
                    let counter = Arc::clone(&calls[i]);
                    let results = Arc::clone(&results);
                    let done = Box::new(move |r: Result<SavedClip, ReplayError>| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        results.lock().unwrap().push((i, r));
                    });
                    match exporter.try_submit(job, done) {
                        Ok(()) => {
                            accepted.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(ReplayError::Busy) => {
                            busy.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(other) => panic!("unexpected submit error: {other}"),
                    }
                }
            });
        }
        for (i, job) in jobs.into_iter().enumerate() {
            tx.send((i, job)).unwrap();
        }
        // The capture keeps running while exports are in flight.
        let from = synth.frames();
        feed(&mut synth, &mut buf, from + frames_for(10), None).unwrap();
    });

    assert!(wait_for(secs(120), || exporter.in_flight() == 0));
    let accepted = accepted.load(Ordering::SeqCst);
    let busy = busy.load(Ordering::SeqCst);
    assert_eq!(accepted + busy, 10);
    assert!(accepted >= 1);
    println!("accepted {accepted}, busy {busy}");

    let results = results.lock().unwrap();
    assert_eq!(results.len(), accepted);
    let total_calls: usize = calls.iter().map(|c| c.load(Ordering::SeqCst)).sum();
    assert_eq!(total_calls, accepted);
    assert!(calls.iter().all(|c| c.load(Ordering::SeqCst) <= 1));
    let mut paths = std::collections::HashSet::new();
    for (_, result) in results.iter() {
        let clip = result.as_ref().expect("export ok");
        assert!(paths.insert(clip.path.clone()), "duplicate clip path");
        assert!(clip.path.is_file());
        assert!(std::fs::metadata(&clip.path).unwrap().len() > 0);
    }
    assert!(files_with_suffix(clips.path(), ".partial").is_empty());
    assert_eq!(files_with_suffix(clips.path(), ".mp4").len(), accepted);
}

/// Feeds a paced hybrid buffer until it holds disk GOPs.
fn hybrid_with_disk(
    root: &Path,
    duration: u64,
    budget: usize,
    max_bytes: u64,
) -> (ReplayBuffer, Synth) {
    let mut synth = Synth::new(1, 16 * MBPS);
    let mut buf = ReplayBuffer::new(
        config(duration, budget, Some((root, max_bytes))),
        layout(1, 1920, 1080),
    )
    .unwrap();
    let start = Instant::now();
    loop {
        let target = synth.frames() + GOP_FRAMES;
        feed(&mut synth, &mut buf, target, Some(budget * 7 / 4)).unwrap();
        if buf.stats().disk_gops >= 3 && synth.frames() >= frames_for(duration) {
            break;
        }
        assert!(start.elapsed() < secs(60), "disk tier never filled");
    }
    (buf, synth)
}

fn disk_segments(snapshot: &ReplaySnapshot) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = snapshot
        .gops
        .iter()
        .filter_map(|g| match g {
            GopRef::Disk { segment, .. } => Some(segment.path().to_path_buf()),
            GopRef::Ram(_) => None,
        })
        .collect();
    paths.dedup();
    paths
}

#[test]
fn eviction_while_exporting_keeps_segments_alive() {
    let root = TempDir::new("evict-root");
    let clips = TempDir::new("evict-clips");
    let (mut buf, mut synth) = hybrid_with_disk(root.path(), 20, 8 * MIB, 256 * MIB as u64);
    let snapshot = buf.snapshot(secs(20)).unwrap();
    let paths = disk_segments(&snapshot);
    assert!(!paths.is_empty(), "snapshot has no disk segments");

    // Push until the buffer evicted every segment the snapshot references.
    let from = synth.frames();
    let mut pushed = 0;
    while pushed < frames_for(120) {
        pushed += GOP_FRAMES;
        feed(&mut synth, &mut buf, from + pushed, Some(8 * MIB * 7 / 4)).unwrap();
        let live: Vec<PathBuf> = disk_segments(&buf.snapshot(secs(1000)).unwrap());
        if paths.iter().all(|p| !live.contains(p)) {
            break;
        }
    }
    let live = disk_segments(&buf.snapshot(secs(1000)).unwrap());
    assert!(
        paths.iter().all(|p| !live.contains(p)),
        "buffer still references the old segments"
    );
    // Evicted from the buffer but pinned by the snapshot.
    assert!(paths.iter().all(|p| p.is_file()), "pinned segment deleted");

    let job = ExportJob {
        snapshot,
        final_path: clips.path().join("evicted.mp4"),
    };
    let clip = export_clip(&job).expect("export of evicted footage");
    assert!(clip.path.is_file());
    assert!(clip.duration >= secs(20), "clip {:?}", clip.duration);
    drop(job);
    assert!(
        wait_for(secs(5), || paths.iter().all(|p| !p.exists())),
        "segments not released after the export finished: {:?} stats {:?}",
        paths.iter().filter(|p| p.exists()).collect::<Vec<_>>(),
        buf.stats()
    );
}

#[test]
fn corrupted_segment_fails_export_cleanly() {
    let root = TempDir::new("corrupt-root");
    let clips = TempDir::new("corrupt-clips");
    let (mut buf, _synth) = hybrid_with_disk(root.path(), 20, 8 * MIB, 256 * MIB as u64);
    let snapshot = buf.snapshot(secs(20)).unwrap();
    let (segment, index) = snapshot
        .gops
        .iter()
        .find_map(|g| match g {
            GopRef::Disk { segment, index } => Some((Arc::clone(segment), *index)),
            GopRef::Ram(_) => None,
        })
        .expect("snapshot with a disk gop");
    let gop = &segment.gops()[index];
    let mut bytes = std::fs::read(segment.path()).unwrap();
    let at = (gop.offset + gop.len / 2) as usize;
    bytes[at] ^= 0xFF;
    std::fs::write(segment.path(), &bytes).unwrap();

    let result = export(snapshot, clips.path(), "corrupt");
    assert!(
        matches!(result, Err(ReplayError::Corrupt(_))),
        "expected Corrupt, got {result:?}"
    );
    assert!(!clips.path().join("corrupt.mp4").exists());
    assert!(files_with_suffix(clips.path(), ".partial").is_empty());
    assert!(files_with_suffix(clips.path(), "").is_empty());
}

#[test]
fn tiny_disk_limit_degrades_but_keeps_working() {
    let root = TempDir::new("tiny-root");
    let clips = TempDir::new("tiny-clips");
    let budget = 16 * MIB;
    let mut synth = Synth::new(1, 16 * MBPS);
    let mut buf = ReplayBuffer::new(
        config(30, budget, Some((root.path(), MIB as u64))),
        layout(1, 1920, 1080),
    )
    .unwrap();
    let mut saw_error = false;
    let total = frames_for(60);
    while synth.frames() < total {
        let target = synth.frames() + GOP_FRAMES;
        feed(&mut synth, &mut buf, target, None).unwrap();
        let stats = buf.stats();
        assert!(stats.ram_bytes <= 2 * budget, "ram {}", stats.ram_bytes);
        assert_eq!(stats.disk_bytes, 0);
        saw_error |= stats.disk_error.is_some();
    }
    saw_error |= wait_for(secs(5), || buf.stats().disk_error.is_some());
    assert!(saw_error, "disk limit never reported");
    let stats = buf.stats();
    assert!(stats.dropped_gops > 0);
    assert!(stats.buffered >= secs(6), "buffered {:?}", stats.buffered);
    assert!(files_with_suffix(&session_dirs(root.path())[0], ".seg").is_empty());
    let requested = secs(4);
    let clip = export(buf.snapshot(requested).unwrap(), clips.path(), "tiny").unwrap();
    assert_clip(&clip, requested, stats.buffered);
}

#[test]
fn exporter_shutdown_semantics() {
    let clips = TempDir::new("shutdown");
    let mut synth = Synth::new(1, 30 * MBPS);
    let mut buf = ReplayBuffer::new(config(120, 256 * MIB, None), layout(1, 1920, 1080)).unwrap();
    feed(&mut synth, &mut buf, frames_for(110), None).unwrap();

    let exporter = Exporter::spawn(3).unwrap();
    let results: Results = Arc::default();
    for i in 0..3 {
        let results = Arc::clone(&results);
        exporter
            .try_submit(
                ExportJob {
                    snapshot: buf.snapshot(secs(100)).unwrap(),
                    final_path: clips.path().join(format!("s{i}.mp4")),
                },
                Box::new(move |r| results.lock().unwrap().push((i, r))),
            )
            .unwrap();
    }
    // Wait until the first job is really running, then shut down.
    assert!(wait_for(secs(30), || {
        !files_with_suffix(clips.path(), ".partial").is_empty()
            || !files_with_suffix(clips.path(), ".mp4").is_empty()
    }));
    drop(exporter);

    let results = results.lock().unwrap();
    assert_eq!(
        results.len(),
        3,
        "every accepted job gets exactly one callback"
    );
    let mut seen = [0; 3];
    for (i, result) in results.iter() {
        seen[*i] += 1;
        match result {
            Ok(clip) => assert!(clip.path.is_file()),
            Err(ReplayError::Shutdown) => {
                assert!(!clips.path().join(format!("s{i}.mp4")).exists())
            }
            Err(other) => panic!("unexpected result {other}"),
        }
    }
    assert_eq!(seen, [1; 3]);
    let first = results.iter().find(|(i, _)| *i == 0).unwrap();
    assert!(first.1.is_ok(), "the running job must complete");
    assert!(files_with_suffix(clips.path(), ".partial").is_empty());
}

#[test]
fn dropping_buffer_during_spill_is_prompt() {
    let root = TempDir::new("drop-root");
    let mut synth = Synth::new(1, 16 * MBPS);
    let mut buf = ReplayBuffer::new(
        config(60, 8 * MIB, Some((root.path(), 1024 * MIB as u64))),
        layout(1, 1920, 1080),
    )
    .unwrap();
    // No pacing: the spill queue is busy when the buffer is dropped.
    feed(&mut synth, &mut buf, frames_for(40), None).unwrap();
    let start = Instant::now();
    drop(buf);
    let took = start.elapsed();
    // Only the write in progress is awaited.
    // Generous bound: the machine may be busy with other tests.
    assert!(took < secs(5), "drop took {took:?}");
    assert!(session_dirs(root.path()).is_empty());
}

fn fake_session(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lock"), b"").unwrap();
    std::fs::write(dir.join("seg-0-3.seg"), b"stale").unwrap();
    std::fs::write(dir.join("seg-4-7.seg.tmp"), b"stale").unwrap();
    dir
}

#[test]
fn crash_recovery_removes_stale_sessions_only() {
    let root = TempDir::new("recovery");
    let stale = fake_session(root.path(), "session-1-1");
    let foreign = fake_session(root.path(), "session-2-2");
    std::fs::write(foreign.join("my-clip.mp4"), b"user data").unwrap();
    std::fs::write(root.path().join("notes.txt"), b"keep").unwrap();

    let buf = ReplayBuffer::new(
        config(30, 16 * MIB, Some((root.path(), 64 * MIB as u64))),
        layout(1, 1920, 1080),
    )
    .unwrap();
    assert!(!stale.exists(), "stale session not removed");
    assert!(
        foreign.join("my-clip.mp4").is_file(),
        "foreign file deleted"
    );
    assert!(root.path().join("notes.txt").is_file());
    // Own files of the kept dir are removed, the foreign one blocks the dir.
    assert!(foreign.is_dir());
    drop(buf);
    assert!(foreign.join("my-clip.mp4").is_file());
}

#[test]
fn live_retention_and_budget_changes_keep_snapshots_valid() {
    let root = TempDir::new("live-change");
    let clips = TempDir::new("live-change-clips");
    let mut synth = Synth::new(1, 16 * MBPS);
    let mut buf = ReplayBuffer::new(
        config(60, 16 * MIB, Some((root.path(), 512 * MIB as u64))),
        layout(1, 1920, 1080),
    )
    .unwrap();
    let mut pace = Some(16 * MIB * 7 / 4);
    let mut step = 0;
    let mut check = |buf: &mut ReplayBuffer, requested: u64, label: &str| {
        step += 1;
        let buffered = buf.stats().buffered;
        let snapshot = buf.snapshot(secs(requested)).unwrap();
        assert_contiguous(&snapshot);
        let clip = export(snapshot, clips.path(), &format!("{label}{step}")).unwrap();
        assert_clip(&clip, secs(requested), buffered);
        std::fs::remove_file(clip.path).unwrap();
    };

    feed(&mut synth, &mut buf, frames_for(100), pace).unwrap();
    check(&mut buf, 30, "base");

    buf.set_duration(secs(20));
    let stats = buf.stats();
    assert!(stats.buffered.as_secs_f64() <= 20.0 + 2.0 * GOP_SECS + 0.1);
    check(&mut buf, 15, "shrunk");
    let from = synth.frames();
    feed(&mut synth, &mut buf, from + frames_for(30), pace).unwrap();
    assert!(buf.stats().buffered.as_secs_f64() <= 20.0 + 2.0 * GOP_SECS + 0.1);

    buf.set_duration(secs(90));
    let from = synth.frames();
    feed(&mut synth, &mut buf, from + frames_for(100), pace).unwrap();
    check(&mut buf, 60, "grown");

    buf.set_memory_limit(8 * MIB);
    check(&mut buf, 30, "less-ram");
    let from = synth.frames();
    feed(&mut synth, &mut buf, from + frames_for(40), pace).unwrap();
    check(&mut buf, 30, "less-ram-fed");

    buf.set_memory_limit(64 * MIB);
    // RAM may legitimately hold up to the full budget now: no throttling.
    pace = None;
    let from = synth.frames();
    feed(&mut synth, &mut buf, from + frames_for(20), pace).unwrap();
    check(&mut buf, 30, "more-ram");
}
