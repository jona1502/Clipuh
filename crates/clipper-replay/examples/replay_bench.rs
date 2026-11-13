//! Replay buffer benchmark on synthetic 60 fps H.264 (AVCC shaped) + one AAC
//! track: push throughput, snapshot latency, export throughput, peak RAM and
//! SSD use, RAM only versus hybrid (real temp dir).
//!
//! Usage: cargo run --release -p clipper-replay --example replay_bench -- [quick] [only=1080|1440|2160]
//!
//! Notes: the synthetic stream is far faster than real time, so the hybrid
//! runs throttle the feed (outside the timed sections) until the spill thread
//! caught up, like a real capture would let it. Push time covers `push` plus
//! the cheap packet generator (an `Arc` clone), nothing else.

#[path = "../tests/common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use clipper_replay::disk::DiskConfig;
use clipper_replay::export::{ExportJob, export_clip};
use clipper_replay::hybrid::{ReplayBuffer, ReplayConfig};
use common::*;

struct Scenario {
    name: &'static str,
    width: u32,
    height: u32,
    mbps: u64,
    seconds: u64,
}

const SCENARIOS: [Scenario; 3] = [
    Scenario {
        name: "1080p60 16 Mbps 60 s",
        width: 1920,
        height: 1080,
        mbps: 16,
        seconds: 60,
    },
    Scenario {
        name: "1440p60 30 Mbps 120 s",
        width: 2560,
        height: 1440,
        mbps: 30,
        seconds: 120,
    },
    Scenario {
        name: "2160p60 60 Mbps 300 s",
        width: 3840,
        height: 2160,
        mbps: 60,
        seconds: 300,
    },
];

const BUDGET: usize = 64 * MIB;

struct Report {
    packets: u64,
    push_time: Duration,
    worst_gop_push: Duration,
    peak_ram: usize,
    peak_disk: u64,
    ssd_written: u64,
    dropped: u64,
    buffered: Duration,
    snapshot_us: Vec<u128>,
    snapshot_gops: usize,
    export_mb_s: f64,
    export_secs: f64,
    clip_secs: f64,
    clip_mb: f64,
}

fn scan_segments(dir: &Path, seen: &mut HashMap<String, u64>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".seg")
            && !seen.contains_key(&name)
            && let Ok(meta) = entry.metadata()
        {
            seen.insert(name, meta.len());
        }
    }
}

fn run(s: &Scenario, hybrid: bool, quick: bool) -> Report {
    let seconds = if quick { s.seconds / 4 } else { s.seconds };
    let root = TempDir::new("bench-cache");
    let clips = TempDir::new("bench-clips");
    let disk = hybrid.then(|| DiskConfig {
        root: root.path().to_path_buf(),
        max_bytes: 8 * 1024 * MIB as u64,
    });
    let mut buf = ReplayBuffer::new(
        ReplayConfig {
            duration: Duration::from_secs(seconds),
            memory_limit_bytes: BUDGET,
            disk,
        },
        layout(1, s.width, s.height),
    )
    .expect("buffer");
    let mut synth = Synth::new(1, s.mbps * 1_000_000);

    let total_frames = (seconds + seconds / 5) * 60;
    let mut push_time = Duration::ZERO;
    let mut worst_gop_push = Duration::ZERO;
    let mut peak_ram = 0usize;
    let mut peak_disk = 0u64;
    let mut seen = HashMap::new();
    let session = session_dirs(root.path()).into_iter().next();
    while synth.frames() < total_frames {
        let target = synth.frames() + GOP_FRAMES;
        let start = Instant::now();
        synth.run_frames(target, |p| buf.push(p)).expect("push");
        let took = start.elapsed();
        push_time += took;
        worst_gop_push = worst_gop_push.max(took);
        let stats = buf.stats();
        peak_ram = peak_ram.max(stats.ram_bytes);
        peak_disk = peak_disk.max(stats.disk_bytes);
        if hybrid {
            if let Some(session) = &session {
                scan_segments(session, &mut seen);
            }
            wait_for(Duration::from_secs(60), || {
                buf.stats().ram_bytes <= BUDGET * 7 / 4
            });
        }
    }
    if hybrid {
        // Let the last spill batches land so the totals are complete.
        std::thread::sleep(Duration::from_millis(300));
        if let Some(session) = &session {
            scan_segments(session, &mut seen);
        }
    }
    let stats = buf.stats();
    peak_ram = peak_ram.max(stats.ram_bytes);
    peak_disk = peak_disk.max(stats.disk_bytes);

    let requested = Duration::from_secs(seconds);
    let mut snapshot_us = Vec::new();
    let mut snapshot_gops = 0;
    for _ in 0..50 {
        let start = Instant::now();
        let snapshot = buf.snapshot(requested).expect("snapshot");
        snapshot_us.push(start.elapsed().as_micros());
        snapshot_gops = snapshot.gops.len();
    }
    snapshot_us.sort_unstable();

    let snapshot = buf.snapshot(requested).expect("snapshot");
    let start = Instant::now();
    let clip = export_clip(&ExportJob {
        snapshot,
        final_path: clips.path().join("bench.mp4"),
    })
    .expect("export");
    let export_secs = start.elapsed().as_secs_f64();
    let clip_mb = clip.bytes as f64 / 1e6;

    Report {
        packets: synth.packets,
        push_time,
        worst_gop_push,
        peak_ram,
        peak_disk,
        ssd_written: seen.values().sum(),
        dropped: stats.dropped_gops,
        buffered: stats.buffered,
        snapshot_us,
        snapshot_gops,
        export_mb_s: clip_mb / export_secs,
        export_secs,
        clip_secs: clip.duration.as_secs_f64(),
        clip_mb,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quick = args.iter().any(|a| a == "quick");
    let only = args.iter().find_map(|a| a.strip_prefix("only="));
    println!(
        "RAM budget {} MiB, 1 AAC track, GOP 2 s{}",
        BUDGET / MIB,
        if quick { ", quick (1/4 length)" } else { "" }
    );
    for s in &SCENARIOS {
        if only.is_some_and(|o| !s.name.starts_with(o)) {
            continue;
        }
        for hybrid in [false, true] {
            let mode = if hybrid { "hybrid  " } else { "RAM only" };
            let r = run(s, hybrid, quick);
            let ns = r.push_time.as_nanos() as f64 / r.packets as f64;
            println!("\n== {} | {mode}", s.name);
            println!(
                "push      {:>10.0} packets/s  {:>7.0} ns/packet  worst GOP ({} packets) {:.2} ms  ({} packets)",
                1e9 / ns,
                ns,
                GOP_FRAMES,
                r.worst_gop_push.as_secs_f64() * 1e3,
                r.packets
            );
            println!(
                "snapshot  median {} us  max {} us  ({} GOPs referenced)",
                r.snapshot_us[r.snapshot_us.len() / 2],
                r.snapshot_us.last().copied().unwrap_or(0),
                r.snapshot_gops
            );
            println!(
                "export    {:>8.1} MB/s  ({:.1} MB clip, {:.1} s of video, {:.2} s)",
                r.export_mb_s, r.clip_mb, r.clip_secs, r.export_secs
            );
            println!(
                "memory    peak ram {:.1} MiB  peak disk {:.1} MiB  ssd written {:.1} MiB  buffered {:.1} s  dropped GOPs {}",
                r.peak_ram as f64 / MIB as f64,
                r.peak_disk as f64 / MIB as f64,
                r.ssd_written as f64 / MIB as f64,
                r.buffered.as_secs_f64(),
                r.dropped
            );
        }
    }
}
