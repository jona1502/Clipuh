//! Instant replay buffer correctness: deterministic scenarios against the
//! production `ReplayBuffer`, `plan_clip` and `export_clip` (no devices, no
//! GPU, no Windows APIs). See docs/quality/replay-buffer-correctness.md.
//!
//! Timestamps are 100 ns ticks on one session timeline. Synthetic streams use
//! 60 fps video with an IDR every 2 s and AAC frames of 1024 samples, like the
//! recorder, but with small payloads so hours of footage run in seconds.

mod common;

use std::sync::Arc;
use std::time::Duration;

use clipper_replay::export::{ExportJob, SavedClip, export_clip};
use clipper_replay::hybrid::{ReplayBuffer, ReplayConfig, ReplaySnapshot};
use clipper_replay::packet::PACKET_OVERHEAD;
use clipper_replay::plan::{ClipPlan, MAX_VIDEO_GAP, plan_clip};
use clipper_replay::{EncodedPacket, ReplayError, TICKS_PER_SECOND, Ticks};
use common::*;

const SEC: Ticks = TICKS_PER_SECOND;

fn ram_buffer(duration_s: u64, budget: usize, audio_tracks: usize) -> ReplayBuffer {
    ReplayBuffer::new(
        ReplayConfig {
            duration: Duration::from_secs(duration_s),
            memory_limit_bytes: budget,
            disk: None,
        },
        layout(audio_tracks, 1280, 720),
    )
    .unwrap()
}

fn plan_of(snapshot: &ReplaySnapshot) -> Result<ClipPlan, ReplayError> {
    let gops = snapshot
        .gops
        .iter()
        .map(|gop| gop.load())
        .collect::<Result<Vec<_>, _>>()?;
    plan_clip(
        &snapshot.layout,
        &gops,
        Ticks::try_from(snapshot.requested.as_nanos() / 100).unwrap_or(Ticks::MAX),
        snapshot.end_pts,
    )
}

fn export(snapshot: ReplaySnapshot, dir: &TempDir, name: &str) -> Result<SavedClip, ReplayError> {
    export_clip(&ExportJob {
        snapshot,
        final_path: dir.path().join(name),
    })
}

fn seconds(ticks: Ticks) -> f64 {
    ticks as f64 / SEC as f64
}

/// Structural invariants every plan must satisfy: starts on a keyframe whose
/// pts is the clip start, strictly increasing video dts, sorted unique audio
/// that overlaps the video span, positive duration.
fn assert_plan_valid(plan: &ClipPlan, context: &str) {
    let video = &plan.video.samples;
    assert!(!video.is_empty(), "{context}: no video");
    assert!(
        video[0].keyframe,
        "{context}: clip does not start on a keyframe"
    );
    assert_eq!(
        video.iter().map(|p| p.pts).min(),
        Some(plan.start_pts),
        "{context}: start is not the first presented frame"
    );
    assert!(
        video.windows(2).all(|w| w[1].dts > w[0].dts),
        "{context}: video dts not strictly increasing"
    );
    assert!(plan.duration > 0, "{context}: non-positive duration");
    let video_end = plan.start_pts + plan.duration;
    for track in &plan.audio {
        assert!(
            track.samples.windows(2).all(|w| w[1].pts > w[0].pts),
            "{context}: audio track {} not sorted/unique",
            track.track
        );
        for packet in &track.samples {
            assert!(
                packet.end() > plan.start_pts && packet.pts < video_end,
                "{context}: audio packet {}..{} outside the video span {}..{}",
                packet.pts,
                packet.end(),
                plan.start_pts,
                video_end
            );
        }
    }
}

/// Small hand-made stream: video frames on the 60 fps grid (optionally
/// dropped or jittered), keyframes every `keyframe_every` frames, and
/// continuous AAC frames on track 1, in timestamp order.
struct Stream {
    packets: Vec<EncodedPacket>,
}

impl Stream {
    fn video(pts: Ticks, key: bool, bytes: usize) -> EncodedPacket {
        EncodedPacket {
            track: 0,
            pts,
            dts: pts,
            duration: FRAME,
            keyframe: key,
            data: Arc::from(vec![0u8; bytes.max(5)]),
        }
    }

    fn audio(pts: Ticks) -> EncodedPacket {
        EncodedPacket {
            track: 1,
            pts,
            dts: pts,
            duration: AAC_FRAME,
            keyframe: true,
            data: Arc::from(vec![0x21u8; 8]),
        }
    }

    fn build(
        start: Ticks,
        frames: i64,
        keyframe_every: i64,
        drop: impl Fn(i64) -> bool,
        jitter: impl Fn(i64) -> Ticks,
    ) -> Self {
        let mut packets = Vec::new();
        for k in 0..frames {
            if !drop(k) {
                packets.push(Self::video(
                    start + k * FRAME + jitter(k),
                    k % keyframe_every == 0,
                    2000,
                ));
            }
        }
        let end = start + frames * FRAME;
        let mut n = 0;
        loop {
            let pts = start + n * AAC_FRAME;
            if pts >= end {
                break;
            }
            packets.push(Self::audio(pts));
            n += 1;
        }
        packets.sort_by_key(|p| (p.pts, p.track));
        Self { packets }
    }

    fn feed(self, buffer: &mut ReplayBuffer) {
        for packet in self.packets {
            buffer.push(packet).unwrap();
        }
    }
}

// --------------------------------------------------------------- basics --

/// TEST A: nothing buffered -> NotReady, not an empty clip; audio before the
/// first video keyframe is discarded.
#[test]
fn a_empty_buffer_is_not_ready() {
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    assert!(matches!(
        buffer.snapshot(Duration::from_secs(30)),
        Err(ReplayError::NotReady)
    ));
    for n in 0..100 {
        buffer.push(Stream::audio(n * AAC_FRAME)).unwrap();
    }
    assert!(matches!(
        buffer.snapshot(Duration::from_secs(30)),
        Err(ReplayError::NotReady)
    ));
    assert_eq!(buffer.stats().ram_bytes, 0);
}

/// TEST B: less footage than configured: nothing is evicted and a save
/// reports the real (shorter) length instead of the requested one.
#[test]
fn b_short_buffer_keeps_everything_and_reports_the_real_length() {
    let dir = TempDir::new("short");
    let mut buffer = ram_buffer(60, 64 * MIB, 1);
    Stream::build(0, 600, 120, |_| false, |_| 0).feed(&mut buffer); // 10 s
    assert_eq!(buffer.stats().ram_gops, 5);
    let saved = export(
        buffer.snapshot(Duration::from_secs(60)).unwrap(),
        &dir,
        "b.mp4",
    )
    .unwrap();
    assert_eq!(saved.requested, Duration::from_secs(60));
    assert!(
        (saved.duration.as_secs_f64() - 10.0).abs() < 0.001,
        "{:?}",
        saved.duration
    );
    assert_eq!(saved.start_pts, 0);
}

/// TEST C + G: at the duration limit old GOPs are evicted, keeping one reserve
/// GOP before the window; a save starts at the latest keyframe at or before
/// `end - requested`, so it is between requested and requested + 1 GOP long.
#[test]
fn c_g_duration_limit_and_keyframe_before_the_requested_start() {
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    // 7224 frames end at 120.4 s: requested start 90.4 s, keyframe at 90 s.
    Stream::build(0, 7224, 120, |_| false, |_| 0).feed(&mut buffer);
    let buffered = buffer.stats().buffered.as_secs_f64();
    assert!((30.0..=34.5).contains(&buffered), "buffered {buffered} s");
    let plan = plan_of(&buffer.snapshot(Duration::from_secs(30)).unwrap()).unwrap();
    assert_plan_valid(&plan, "c");
    let end = plan.start_pts + plan.duration;
    let target = end - 30 * SEC;
    assert!(plan.start_pts <= target, "start after the requested start");
    assert!(
        target - plan.start_pts < GOP_TICKS,
        "more than one GOP early"
    );
    assert_eq!(plan.start_pts, 5400 * FRAME, "the keyframe at 90 s");
}

/// TEST H: the window reaches back before the oldest buffered keyframe (the
/// buffer evicted it): the clip starts at the oldest keyframe, decodable,
/// and reports the shorter length. Retention runs when a GOP is sealed, so
/// up to `duration` + 3 GOPs (reserve, containing, open) stay buffered.
#[test]
fn h_oldest_keyframe_when_history_is_short() {
    let mut buffer = ram_buffer(15, 64 * MIB, 1);
    Stream::build(0, 3600, 120, |_| false, |_| 0).feed(&mut buffer); // 60 s
    let plan = plan_of(&buffer.snapshot(Duration::from_secs(300)).unwrap()).unwrap();
    assert_plan_valid(&plan, "h");
    let length = seconds(plan.duration);
    assert!((15.0..=21.0).contains(&length), "{length}");
}

/// TEST D: a RAM-only byte budget bounds the buffer even when the duration
/// would keep more; the replay gets shorter, the budget does not grow.
#[test]
fn d_byte_budget_bounds_ram_only_buffers() {
    let budget = 2 * MIB;
    let mut buffer = ram_buffer(300, budget, 1);
    let mut synth = Synth::new(1, 8_000_000);
    let mut max_ram = 0;
    synth
        .run_frames(60 * 120, |packet| {
            buffer.push(packet)?;
            max_ram = max_ram.max(buffer.stats().ram_bytes);
            Ok::<_, ReplayError>(())
        })
        .unwrap();
    // Sealed GOPs are evicted down to the budget; the open GOP may add up to
    // one more GOP (it is only dropped above twice the budget).
    assert!(
        max_ram <= 2 * budget,
        "RAM accounting reached {max_ram} bytes"
    );
    let buffered = buffer.stats().buffered.as_secs_f64();
    assert!(buffered < 300.0 && buffered > 1.0, "{buffered}");
    assert_plan_valid(
        &plan_of(&buffer.snapshot(Duration::from_secs(300)).unwrap()).unwrap(),
        "d",
    );
}

/// TEST E: one keyframe larger than the whole budget: the buffer drops that
/// GOP instead of growing, is NotReady meanwhile and recovers at the next
/// normal keyframe.
#[test]
fn e_oversized_keyframe_is_dropped_not_kept() {
    let budget = MIB;
    let mut buffer = ram_buffer(30, budget, 0);
    for k in 0..240 {
        let bytes = if k == 120 { 3 * budget } else { 1000 };
        buffer
            .push(Stream::video(k * FRAME, k % 120 == 0, bytes))
            .unwrap();
        assert!(buffer.stats().ram_bytes <= 2 * budget, "frame {k}");
    }
    assert!(buffer.stats().dropped_gops >= 1);
    assert!(matches!(
        buffer.snapshot(Duration::from_secs(30)),
        Err(ReplayError::NotReady)
    ));
    buffer.push(Stream::video(240 * FRAME, true, 1000)).unwrap();
    let plan = plan_of(&buffer.snapshot(Duration::from_secs(30)).unwrap()).unwrap();
    assert_plan_valid(&plan, "e");
}

/// TEST F: no keyframe ever: nothing is buffered and nothing claims to be a
/// clip.
#[test]
fn f_missing_keyframe_never_yields_a_clip() {
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    Stream::build(0, 600, i64::MAX, |k| k == 0, |_| 0).feed(&mut buffer);
    assert!(matches!(
        buffer.snapshot(Duration::from_secs(30)),
        Err(ReplayError::NotReady)
    ));
    assert_eq!(buffer.stats().ram_bytes, 0);
}

// ------------------------------------------------------------- timing --

/// TEST I + J: dropped frames and irregular intervals: the clip length comes
/// from timestamps (not from frame counts) and frames keep their times.
#[test]
fn i_j_dropped_and_irregular_frames_keep_their_timestamps() {
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    let jitter = |k: i64| {
        if k % 120 == 0 {
            0
        } else {
            (k * 7919 % 41) * 1000 - 20_000
        }
    };
    Stream::build(
        0,
        1800,
        120,
        |k| k % 9 == 4 || (600..630).contains(&k),
        jitter,
    )
    .feed(&mut buffer);
    let plan = plan_of(&buffer.snapshot(Duration::from_secs(20)).unwrap()).unwrap();
    assert_plan_valid(&plan, "i/j");
    let frames = plan.video.samples.len() as f64;
    let length = seconds(plan.duration);
    assert!(
        frames < length * 60.0 * 0.9,
        "{frames} frames for {length} s: count-based length?"
    );
    let last = plan.video.samples.iter().map(|p| p.end()).max().unwrap();
    assert_eq!(
        plan.start_pts + plan.duration,
        last,
        "length must end at the last frame's end"
    );
}

/// TEST K: the AAC frame straddling the keyframe start is kept (entered via
/// the edit list), none is duplicated, and audio never extends past video.
#[test]
fn k_audio_overlapping_the_clip_start_is_kept_once() {
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    Stream::build(0, 1800, 120, |_| false, |_| 0).feed(&mut buffer);
    let plan = plan_of(&buffer.snapshot(Duration::from_secs(10)).unwrap()).unwrap();
    assert_plan_valid(&plan, "k");
    let audio = &plan.audio[0];
    let first = &audio.samples[0];
    assert!(first.pts <= plan.start_pts && first.end() > plan.start_pts);
    assert_eq!(audio.media_time, plan.start_pts - first.pts);
    assert_eq!(audio.lead, 0);
}

/// TEST L + U: a non-zero, huge session origin (weeks of uptime, and near
/// the i64 limit) changes nothing relative: the same clip as at origin 0.
#[test]
fn l_u_large_origins_preserve_relative_timing() {
    let reference = {
        let mut buffer = ram_buffer(30, 64 * MIB, 1);
        Stream::build(0, 1800, 120, |_| false, |_| 0).feed(&mut buffer);
        plan_of(&buffer.snapshot(Duration::from_secs(20)).unwrap()).unwrap()
    };
    for origin in [37 * 24 * 3600 * SEC, 1 << 60, i64::MAX - 3600 * SEC] {
        let mut buffer = ram_buffer(30, 64 * MIB, 1);
        Stream::build(origin, 1800, 120, |_| false, |_| 0).feed(&mut buffer);
        let plan = plan_of(&buffer.snapshot(Duration::from_secs(20)).unwrap()).unwrap();
        assert_plan_valid(&plan, &format!("origin {origin}"));
        assert_eq!(plan.start_pts - origin, reference.start_pts);
        assert_eq!(plan.duration, reference.duration);
        assert_eq!(
            plan.audio[0].media_time, reference.audio[0].media_time,
            "audio/video offset changed at origin {origin}"
        );
        // The file is written relative to the clip start: no overflow.
        let dir = TempDir::new("origin");
        export(
            buffer.snapshot(Duration::from_secs(20)).unwrap(),
            &dir,
            "l.mp4",
        )
        .unwrap();
    }
}

// -------------------------------------------------- snapshots and saves --

/// TEST M + N: two snapshots taken at different times stay independent, and
/// eviction after a snapshot does not change what it exports: the file
/// written after 6 more minutes of recording is byte-identical.
#[test]
fn m_n_snapshots_are_independent_and_immune_to_eviction() {
    let dir = TempDir::new("snap");
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    let mut synth = Synth::new(1, 4_000_000);
    synth.run_frames(60 * 60, |p| buffer.push(p)).unwrap();
    let first = buffer.snapshot(Duration::from_secs(30)).unwrap();
    let early = export(first.clone(), &dir, "early.mp4").unwrap();
    synth.run_frames(60 * 70, |p| buffer.push(p)).unwrap();
    let second = buffer.snapshot(Duration::from_secs(30)).unwrap();
    synth.run_frames(60 * 430, |p| buffer.push(p)).unwrap(); // evicts both windows
    let late = export(first, &dir, "late.mp4").unwrap();
    let other = export(second, &dir, "other.mp4").unwrap();
    assert_eq!(
        std::fs::read(&early.path).unwrap(),
        std::fs::read(&late.path).unwrap(),
        "eviction changed an existing snapshot"
    );
    assert_eq!(late.start_pts, early.start_pts);
    assert!(other.start_pts > late.start_pts + 9 * SEC);
}

/// TEST R: hundreds of saves in a row while recording continues: every plan
/// is valid and nothing leaks into the buffer's accounting.
#[test]
fn r_rapid_repeated_saves() {
    let dir = TempDir::new("rapid");
    let mut buffer = ram_buffer(30, 64 * MIB, 2);
    let mut synth = Synth::new(2, 4_000_000);
    synth.run_frames(60 * 40, |p| buffer.push(p)).unwrap();
    let steady = buffer.stats().ram_bytes;
    for round in 0..400u64 {
        let until = synth.frames() + 3;
        synth.run_frames(until, |p| buffer.push(p)).unwrap();
        let snapshot = buffer.snapshot(Duration::from_secs(30)).unwrap();
        if round % 100 == 0 {
            export(snapshot, &dir, &format!("r{round}.mp4")).unwrap();
        } else {
            assert_plan_valid(&plan_of(&snapshot).unwrap(), &format!("round {round}"));
        }
    }
    let ram = buffer.stats().ram_bytes;
    assert!(
        ram < steady * 2,
        "{steady} -> {ram}: snapshots inflated the accounting"
    );
}

/// TEST O + P + X: a new session (restart, other resolution, timeline from 0
/// again) gets a new buffer; a snapshot of the old session, taken before its
/// buffer was dropped, still exports with its own data and is not mixed with
/// the new one. With the SSD tier, the snapshot's segment files outlive the
/// dropped buffer until the export finished, then they are removed.
#[test]
fn o_p_x_restart_and_drop_during_save() {
    let dir = TempDir::new("restart");
    let cache = TempDir::new("restart-cache");
    let mut old = ReplayBuffer::new(
        ReplayConfig {
            duration: Duration::from_secs(60),
            memory_limit_bytes: MIB,
            disk: Some(clipper_replay::disk::DiskConfig {
                root: cache.path().to_path_buf(),
                max_bytes: 512 * MIB as u64,
            }),
        },
        layout(1, 1920, 1080),
    )
    .unwrap();
    // Fed in 2 s chunks with a short pause, like the existing disk test:
    // capture is paced in reality, so the spill thread keeps up (pushed all
    // at once, the 2x RAM cap would evict GOPs before their spill finished).
    let mut synth = Synth::new(1, 1_000_000);
    for chunk in 1..=30u64 {
        synth.run_frames(chunk * 120, |p| old.push(p)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        wait_for(Duration::from_secs(10), || old.stats().disk_gops > 0),
        "nothing spilled: {:?}",
        old.stats()
    );
    let snapshot = old.snapshot(Duration::from_secs(60)).unwrap();
    drop(old); // replay stopped while the save is queued

    let mut new = ram_buffer(30, 64 * MIB, 1);
    let mut synth = Synth::new(1, 6_000_000);
    synth.run_frames(600, |p| new.push(p)).unwrap();
    let new_plan = plan_of(&new.snapshot(Duration::from_secs(30)).unwrap()).unwrap();

    let saved = export(snapshot, &dir, "old.mp4").unwrap();
    assert!(
        saved.duration >= Duration::from_secs(55),
        "{:?}",
        saved.duration
    );
    assert!(new_plan.duration < 11 * SEC, "new session sees old footage");
    assert!(
        wait_for(Duration::from_secs(5), || session_dirs(cache.path())
            .is_empty()),
        "segment files outlived the last snapshot"
    );
}

// ----------------------------------------------------- failure handling --

/// TEST S: extreme configured durations never panic and stay bounded.
#[test]
fn s_extreme_durations_are_handled() {
    for duration in [Duration::ZERO, Duration::from_nanos(1), Duration::MAX] {
        let budget = 4 * MIB;
        let mut buffer = ReplayBuffer::new(
            ReplayConfig {
                duration,
                memory_limit_bytes: budget,
                disk: None,
            },
            layout(1, 1280, 720),
        )
        .unwrap();
        Stream::build(0, 3600, 120, |_| false, |_| 0).feed(&mut buffer);
        buffer.set_duration(duration);
        assert!(buffer.stats().ram_bytes <= 2 * budget);
        let plan = plan_of(&buffer.snapshot(duration).unwrap()).unwrap();
        assert_plan_valid(&plan, &format!("{duration:?}"));
    }
}

/// TEST T (fixed): a forward clock jump inside the buffer. Before the fix a
/// 10 s save shortly after a 1 h jump produced a "3605 s" clip: the reserve
/// GOP before the jump, then the jump as one frozen frame. Now the clip
/// starts after the discontinuity, and is NotReady while no keyframe
/// followed it.
#[test]
fn t_clips_never_span_a_timestamp_discontinuity() {
    let jump = 3600 * SEC;
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    Stream::build(0, 1200, 120, |_| false, |_| 0).feed(&mut buffer); // 0..20 s
    // After the jump the stream resumes mid-GOP (frames 30.. of a GOP).
    Stream::build(20 * SEC + jump - 30 * FRAME, 60, 120, |k| k < 30, |_| 0).feed(&mut buffer);
    assert!(
        matches!(
            plan_of(&buffer.snapshot(Duration::from_secs(10)).unwrap()),
            Err(ReplayError::NotReady)
        ),
        "no keyframe after the jump yet"
    );
    Stream::build(20 * SEC + jump + 30 * FRAME, 150, 120, |_| false, |_| 0).feed(&mut buffer);
    let plan = plan_of(&buffer.snapshot(Duration::from_secs(10)).unwrap()).unwrap();
    assert_plan_valid(&plan, "after jump");
    assert!(
        plan.start_pts >= 20 * SEC + jump,
        "clip starts before the jump"
    );
    assert!(seconds(plan.duration) < 3.0, "{} s", seconds(plan.duration));

    // Gaps up to MAX_VIDEO_GAP (one second) stay inside clips.
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    let gap_frames = MAX_VIDEO_GAP / FRAME - 1;
    Stream::build(
        0,
        1200,
        120,
        |k| (300..300 + gap_frames).contains(&k),
        |_| 0,
    )
    .feed(&mut buffer);
    let plan = plan_of(&buffer.snapshot(Duration::from_secs(30)).unwrap()).unwrap();
    assert_eq!(plan.start_pts, 0, "a short gap must not cut the clip");
}

/// TEST V (fixed): duplicate or decreasing video dts and packets that break
/// the contract (negative pts, zero duration, unknown track) are rejected
/// without side effects. Before the fix a zero-duration or negative-pts
/// packet was accepted and made every save fail ("invalid replay data")
/// while it stayed in the replay window.
#[test]
fn v_contract_violations_are_rejected_and_saves_keep_working() {
    let dir = TempDir::new("reject");
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    Stream::build(0, 600, 120, |_| false, |_| 0).feed(&mut buffer);
    let before = buffer.stats();
    let bad = [
        EncodedPacket {
            duration: 0,
            ..Stream::audio(5 * SEC)
        },
        EncodedPacket {
            duration: -AAC_FRAME,
            ..Stream::audio(5 * SEC)
        },
        EncodedPacket {
            pts: -1,
            dts: -1,
            ..Stream::audio(0)
        },
        EncodedPacket {
            duration: 0,
            ..Stream::video(600 * FRAME, false, 100)
        },
        Stream::video(599 * FRAME, false, 100), // dts not increasing
        Stream::video(100 * FRAME, false, 100), // out of order
        EncodedPacket {
            track: 9,
            ..Stream::audio(5 * SEC)
        },
    ];
    for packet in bad {
        let description = format!("{packet:?}");
        assert!(
            matches!(buffer.push(packet), Err(ReplayError::Invalid(_))),
            "accepted {description}"
        );
    }
    assert_eq!(
        buffer.stats(),
        before,
        "a rejected packet changed the buffer"
    );
    Stream::build(600 * FRAME, 300, 120, |_| false, |_| 0).feed(&mut buffer);
    let saved = export(
        buffer.snapshot(Duration::from_secs(10)).unwrap(),
        &dir,
        "v.mp4",
    )
    .unwrap();
    assert!(saved.duration >= Duration::from_secs(10));

    // Duplicate and out-of-order audio is accepted and normalized by the plan.
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    let mut stream = Stream::build(0, 600, 120, |_| false, |_| 0);
    let audio: Vec<EncodedPacket> = stream
        .packets
        .iter()
        .filter(|p| p.track == 1)
        .take(50)
        .cloned()
        .collect();
    stream.packets.extend(audio);
    stream.packets.swap(10, 30);
    for packet in stream.packets {
        let _ = buffer.push(packet);
    }
    let plan = plan_of(&buffer.snapshot(Duration::from_secs(30)).unwrap()).unwrap();
    assert_plan_valid(&plan, "duplicate audio");
}

/// TEST W: an audio track that stops delivering for 5 s while video goes on
/// (in production the mixer renders silence instead, so this is the
/// contract-violating worst case) keeps the buffer and saves consistent.
#[test]
fn w_audio_interruption_keeps_the_buffer_consistent() {
    let dir = TempDir::new("audio-gap");
    let mut buffer = ram_buffer(30, 64 * MIB, 1);
    let stream = Stream::build(0, 1800, 120, |_| false, |_| 0);
    for packet in stream.packets {
        let silent_span = (10 * SEC..15 * SEC).contains(&packet.pts);
        if !(packet.track == 1 && silent_span) {
            buffer.push(packet).unwrap();
        }
    }
    let snapshot = buffer.snapshot(Duration::from_secs(25)).unwrap();
    let plan = plan_of(&snapshot).unwrap();
    assert_plan_valid(&plan, "w");
    assert_eq!(plan.audio.len(), 1, "the audio track must stay in the clip");
    export(snapshot, &dir, "w.mp4").unwrap();
}

// ------------------------------------------------------- long sessions --

/// TEST Q: 30 min, 1 h, 3 h and 6 h of simulated 60 fps capture with one
/// audio track. The logical RAM accounting stays bounded, the buffered span
/// stays at the configured duration, and a save at every checkpoint is a
/// valid clip of the requested length ending at the newest footage.
/// (Logical byte accounting, not process memory.)
#[test]
fn q_long_sessions_stay_bounded_and_saveable() {
    let budget = 8 * MIB;
    let mut buffer = ram_buffer(60, budget, 1);
    let mut synth = Synth::new(1, 300_000);
    let mut checkpoints = Vec::new();
    for minutes in [30u64, 60, 180, 360] {
        synth
            .run_frames(minutes * 60 * 60, |p| buffer.push(p))
            .unwrap();
        let stats = buffer.stats();
        assert!(
            stats.ram_bytes <= budget,
            "{minutes} min: {} bytes",
            stats.ram_bytes
        );
        assert!(
            (60.0..=64.5).contains(&stats.buffered.as_secs_f64()),
            "{minutes} min: buffered {:?}",
            stats.buffered
        );
        assert!(
            stats.ram_gops <= 33,
            "{minutes} min: {} GOPs",
            stats.ram_gops
        );
        let plan = plan_of(&buffer.snapshot(Duration::from_secs(60)).unwrap()).unwrap();
        assert_plan_valid(&plan, &format!("{minutes} min"));
        let length = seconds(plan.duration);
        assert!(
            (60.0..=62.1).contains(&length),
            "{minutes} min: clip {length} s"
        );
        let end = seconds(plan.start_pts + plan.duration);
        assert!(
            end >= (minutes * 60) as f64 - 1.0,
            "{minutes} min: stale end {end}"
        );
        checkpoints.push((minutes, length, stats.ram_bytes));
    }
    assert!(synth.packets > 2_000_000, "{} packets", synth.packets);
    println!(
        "long session (min, clip s, RAM accounting B; {} B overhead per packet): {checkpoints:?}",
        PACKET_OVERHEAD
    );
}

// ----------------------------------------------- generated sequences --

/// Small deterministic xorshift generator: reproducible without a new
/// dependency; failures name the seed and step.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Random valid capture histories: variable GOP lengths and packet sizes,
/// dropped frames, irregular intervals, forward discontinuities resuming
/// mid-GOP, live duration and budget changes, snapshots at random points.
/// Invariants: the RAM accounting equals the charged bytes of everything
/// retained and stays bounded; every plan is structurally valid, starts on a
/// keyframe and never spans a discontinuity; nothing panics.
#[test]
fn generated_histories_keep_the_invariants() {
    for seed in 1..=64u64 {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
        let mut budget = (1 + rng.below(16) as usize) * MIB;
        let mut buffer = ram_buffer(15 + rng.below(60), budget, 1 + rng.below(2) as usize);
        let tracks = buffer.layout().tracks.len() as u8;
        let mut video_t: Ticks = rng.below(1 << 40) as Ticks;
        let mut audio_t = video_t;
        let mut frame = 0u64;
        let mut gop = 30 + rng.below(240);
        let mut discontinuity = Ticks::MIN;
        for step in 0..4000u64 {
            let context = format!("seed {seed} step {step}");
            match rng.below(100) {
                0 => {
                    video_t += (2 + rng.below(3600)) as Ticks * SEC;
                    audio_t = video_t;
                    discontinuity = video_t;
                    frame = 1 + rng.below(gop - 1); // resume mid-GOP
                }
                1 => buffer.set_duration(Duration::from_secs(rng.below(400))),
                2 => {
                    budget = (1 + rng.below(16) as usize) * MIB;
                    buffer.set_memory_limit(budget);
                }
                3..=6 => {
                    let requested = Duration::from_secs(1 + rng.below(300));
                    if let Ok(snapshot) = buffer.snapshot(requested) {
                        match plan_of(&snapshot) {
                            Ok(plan) => {
                                assert_plan_valid(&plan, &context);
                                assert!(
                                    plan.start_pts >= discontinuity
                                        || plan.start_pts + plan.duration <= discontinuity,
                                    "{context}: clip spans a discontinuity"
                                );
                            }
                            Err(ReplayError::NotReady) => {}
                            Err(error) => panic!("{context}: {error}"),
                        }
                    }
                }
                _ => {
                    // One video frame (maybe dropped) and the audio up to it.
                    while audio_t <= video_t {
                        for track in 1..tracks {
                            buffer
                                .push(EncodedPacket {
                                    track,
                                    pts: audio_t,
                                    dts: audio_t,
                                    duration: AAC_FRAME,
                                    keyframe: true,
                                    data: Arc::from(vec![1u8; 1 + rng.below(400) as usize]),
                                })
                                .unwrap_or_else(|e| panic!("{context}: audio rejected: {e}"));
                        }
                        audio_t += AAC_FRAME;
                    }
                    let key = frame.is_multiple_of(gop);
                    if key && rng.below(4) == 0 {
                        gop = 30 + rng.below(240);
                        frame = 0;
                    }
                    if key || rng.below(20) != 0 {
                        let size = if key { 20_000 } else { 500 } + rng.below(30_000) as usize;
                        buffer
                            .push(Stream::video(video_t, key, size))
                            .unwrap_or_else(|e| panic!("{context}: video rejected: {e}"));
                    }
                    frame += 1;
                    video_t += FRAME - 20_000 + rng.below(40_000) as Ticks;
                }
            }
            let stats = buffer.stats();
            assert!(
                stats.ram_bytes <= 2 * budget.max(16 * MIB) + 64 * 1024,
                "{context}: RAM accounting {} over the bound",
                stats.ram_bytes
            );
            if let Ok(all) = buffer.snapshot(Duration::MAX) {
                let retained: usize = all
                    .gops
                    .iter()
                    .map(|gop| gop.load().unwrap().charged_bytes)
                    .sum();
                assert_eq!(retained, stats.ram_bytes, "{context}: accounting drifted");
            }
        }
    }
}
