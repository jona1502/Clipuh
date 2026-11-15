//! Deterministic audio/video synchronization scenarios (no devices, no GPU).
//!
//! The production pieces that decide timing are platform neutral and are
//! driven here directly: `FramePacer` + `SlotTimeline` (video slot times),
//! `TimelineMixer` (audio placement on the shared QPC timeline), and
//! `plan_clip` + `write_mp4` (replay window and container timing). Only the
//! inputs are synthetic: audio sources whose sample clock may run off QPC by
//! a given ppm, with packets stamped like WASAPI (`u64QPCPosition` of the
//! first frame), and video frames stamped like WGC (`SystemRelativeTime`).
//!
//! Audio markers carry their own index in the sample value, so a test finds
//! where each marker landed on the output timeline and compares it with the
//! real time it was captured. See `docs/quality/av-sync-audit.md`.

use std::path::Path;
use std::sync::Arc;

use clipper_audio::mixer::{CONTINUITY_FRAMES, ResyncStats, TimelineMixer};
use clipper_core::pipeline::{FramePacer, TICKS_PER_SECOND};
use clipper_replay::gop::Gop;
use clipper_replay::mp4::write_mp4;
use clipper_replay::plan::plan_clip;
use clipper_replay::{CodecConfig, EncodedPacket, StreamLayout, TrackInfo};

use crate::logic::SlotTimeline;

const RATE: i64 = 48_000;
const MS: i64 = 10_000;
/// WASAPI shared-mode packet: 10 ms.
const PACKET: usize = 480;
/// The recorder renders audio this far behind real time (`pump`).
const RENDER_LAG: i64 = 800_000;
/// Simulated service period of the recorder worker.
const STEP: i64 = 10 * MS;
/// Capture-to-ring latency of a packet after its last frame.
const DELIVERY: i64 = 5 * MS;
/// An arbitrary non-zero QPC origin (about 3.4 days of uptime).
const QPC_ORIGIN: i64 = 2_937_600_123_456_789;

// ---------------------------------------------------------------- audio --

#[derive(Clone, Copy, Debug)]
struct Source {
    /// Session time (ticks after the QPC origin) of the source's first frame.
    start: i64,
    /// Sample clock error against QPC: +100 = 100 ppm fast.
    ppm: f64,
    /// Session-time span in which the source delivers nothing (app closed,
    /// endpoint gone); its clock keeps running, as a device clock does.
    gap: Option<(i64, i64)>,
    /// Session time after which the source stops.
    end: i64,
    /// Packets carry zeros only (WASAPI `SILENT` packets).
    silent: bool,
}

impl Source {
    fn new(start: i64, end: i64) -> Self {
        Self {
            start,
            ppm: 0.0,
            gap: None,
            end,
            silent: false,
        }
    }

    /// Real session time (ticks, exact) of source frame `n`.
    fn time_of(&self, n: i64) -> f64 {
        self.start as f64
            + n as f64 * TICKS_PER_SECOND as f64 / (RATE as f64 * (1.0 + self.ppm * 1e-6))
    }
}

/// A marker every second of source clock; the value encodes its index.
fn marker_value(index: i64) -> f32 {
    0.1 + (index % 6000) as f32 * 1e-4
}

fn marker_index(value: f32) -> i64 {
    ((value - 0.1) / 1e-4).round() as i64
}

#[derive(Debug, Default)]
struct Placed {
    /// (marker index, output frame position) per source.
    markers: Vec<Vec<(i64, i64)>>,
    resync: ResyncStats,
    late: u64,
    /// Output frame position of the first rendered block.
    first_position: Option<i64>,
}

struct Session {
    origin: i64,
    sources: Vec<Source>,
    /// Session time of the first video slot (`ReplaySession::begin_video`).
    video_start: Option<i64>,
    /// Session time up to which video has been submitted, by real time;
    /// audio is never rendered past it (`RecordingAudio::pump`).
    video_end: fn(i64) -> i64,
    duration: i64,
}

fn video_follows_real_time(now: i64) -> i64 {
    now
}

impl Session {
    fn new(sources: Vec<Source>, duration: i64) -> Self {
        Self {
            origin: QPC_ORIGIN,
            sources,
            video_start: None,
            video_end: video_follows_real_time,
            duration,
        }
    }

    /// Runs the production mixer the way the recorder worker drives it.
    fn run(&self) -> Placed {
        let count = self.sources.len();
        let mut mixer = TimelineMixer::new(self.origin, vec![1.0; count], true);
        if let Some(start) = self.video_start {
            mixer.skip_to(self.origin + start);
        }
        let mut next_frame = vec![0i64; count];
        let mut placed = Placed {
            markers: vec![Vec::new(); count],
            ..Placed::default()
        };
        let mut packet = vec![0f32; PACKET];
        let mut now = 0;
        while now <= self.duration {
            for (index, source) in self.sources.iter().enumerate() {
                loop {
                    let n = next_frame[index];
                    let stamp = source.time_of(n);
                    let last = source.time_of(n + PACKET as i64 - 1);
                    if stamp >= source.end as f64 || last + DELIVERY as f64 > now as f64 {
                        break;
                    }
                    next_frame[index] += PACKET as i64;
                    if source
                        .gap
                        .is_some_and(|(from, to)| stamp >= from as f64 && stamp < to as f64)
                    {
                        continue;
                    }
                    for (offset, value) in packet.iter_mut().enumerate() {
                        let frame = n + offset as i64;
                        *value = if !source.silent && frame % RATE == 0 {
                            marker_value(frame / RATE)
                        } else {
                            0.0
                        };
                    }
                    // WASAPI stamps the packet with QPC of its first frame.
                    let qpc = self.origin + stamp.round() as i64;
                    mixer.push(index, qpc, 1, &packet);
                }
            }
            let end = (now - RENDER_LAG).min((self.video_end)(now));
            render(&mut mixer, self.origin + end, &mut placed);
            now += STEP;
        }
        placed.resync = mixer.resync_stats();
        placed.late = mixer.late_frames();
        placed
    }
}

fn render(mixer: &mut TimelineMixer, until: i64, placed: &mut Placed) {
    mixer
        .render_until(until, |track, time, samples| {
            // Block time is floor(position * 10^7 / 48000): invert exactly.
            // Times are never negative (the mixer clamps its position at 0).
            let scaled = time as u128 * RATE as u128;
            let position = scaled.div_ceil(TICKS_PER_SECOND as u128) as i64;
            if track == 0 {
                placed.first_position.get_or_insert(position);
                return Ok::<_, ()>(());
            }
            for (offset, frame) in samples.chunks_exact(2).enumerate() {
                if frame[0] != 0.0 {
                    placed.markers[track - 1]
                        .push((marker_index(frame[0]), position + offset as i64));
                }
            }
            Ok(())
        })
        .unwrap();
}

/// Largest |output position - capture time| of a source's markers, in frames.
fn max_marker_error(source: &Source, markers: &[(i64, i64)]) -> f64 {
    markers
        .iter()
        .map(|&(index, position)| {
            let truth = source.time_of(index * RATE) * RATE as f64 / TICKS_PER_SECOND as f64;
            (position as f64 - truth).abs()
        })
        .fold(0.0, f64::max)
}

fn assert_markers(
    name: &str,
    source: &Source,
    markers: &[(i64, i64)],
    expected: usize,
    tolerance: f64,
) {
    assert!(
        markers.len() >= expected,
        "{name}: found {} markers, expected at least {expected}",
        markers.len()
    );
    let worst = max_marker_error(source, markers);
    assert!(
        worst <= tolerance,
        "{name}: a marker landed {worst:.2} frames ({:.1} us) from its capture time; \
         tolerance {tolerance} frames. markers: {:?}",
        worst * 1e6 / RATE as f64,
        &markers[..markers.len().min(8)]
    );
}

/// Placement bound of a jitter-free source: packet stamps are rounded to
/// whole ticks and positions truncated to whole frames.
const EXACT_FRAMES: f64 = 1.0;
/// Bound under sample clock drift: the mixer continues a source until its
/// QPC position is more than CONTINUITY_FRAMES away, plus the exact bound.
const DRIFT_FRAMES: f64 = CONTINUITY_FRAMES as f64 + EXACT_FRAMES;

/// TEST A + O: two sources starting with the video at a non-zero QPC origin
/// land on their capture times within one frame (20.8 us), and the mixer
/// output starts at the video start.
#[test]
fn a_perfect_sync_at_a_non_zero_origin() {
    let duration = 10 * TICKS_PER_SECOND;
    let mut session = Session::new(
        vec![Source::new(0, duration), Source::new(0, duration)],
        duration,
    );
    session.video_start = Some(0);
    let placed = session.run();
    for (index, source) in session.sources.iter().enumerate() {
        assert_markers(
            &format!("source {index}"),
            source,
            &placed.markers[index],
            9,
            EXACT_FRAMES,
        );
    }
    assert_eq!(placed.first_position, Some(0));
    assert_eq!(placed.resync, ResyncStats::default());
    assert_eq!(placed.late, 0);
}

/// TEST B: audio starting 150 ms after the video keeps its capture time; the
/// mix is silent before it instead of starting the audio early.
#[test]
fn b_constant_offset_is_kept_not_compressed() {
    let duration = 6 * TICKS_PER_SECOND;
    let source = Source::new(150 * MS, duration);
    let mut session = Session::new(vec![source], duration);
    session.video_start = Some(0);
    let placed = session.run();
    assert_markers("late source", &source, &placed.markers[0], 5, EXACT_FRAMES);
    // Marker 0 is the source's first frame: exactly 150 ms = 7200 frames in.
    assert_eq!(
        placed.markers[0][0],
        (0, 7200),
        "first sample must sit at +150 ms"
    );
    assert_eq!(placed.first_position, Some(0));
}

fn drift(minutes: i64, ppm: f64) {
    let duration = minutes * 60 * TICKS_PER_SECOND;
    let source = Source {
        ppm,
        ..Source::new(0, duration)
    };
    let placed = Session::new(vec![source], duration).run();
    let name = format!("{minutes} min at {ppm:+} ppm");
    // Markers overwritten by an overlap re-anchor are lost (fast clock
    // only); every other second must be present.
    let seconds = (minutes * 60 - 1) as usize;
    assert_markers(
        &name,
        &source,
        &placed.markers[0],
        seconds - placed.resync.count as usize,
        DRIFT_FRAMES,
    );
    // Detection: the offset grows by |ppm| * 48000 frames per second and is
    // corrected once it passes CONTINUITY_FRAMES, so the re-anchor count is
    // predictable from the drift rate.
    let drift_frames = ppm.abs() * 1e-6 * (RATE * minutes * 60) as f64;
    let expected = (drift_frames / (CONTINUITY_FRAMES + 1) as f64).floor() as u64;
    assert!(
        placed.resync.count.abs_diff(expected) <= 1,
        "{name}: {} re-anchors, expected about {expected} for {drift_frames:.0} frames of drift",
        placed.resync.count
    );
    assert!(
        placed.resync.max_frames <= CONTINUITY_FRAMES as u64 + 2,
        "{name}: a drift re-anchor of {} frames",
        placed.resync.max_frames
    );
    // The last marker is no further off than the first ones: no accumulation.
    let last = *placed.markers[0].last().unwrap();
    let last_error = max_marker_error(&source, &[last]);
    assert!(
        last_error <= DRIFT_FRAMES,
        "{name}: last marker {last_error:.1} frames off"
    );
    assert_eq!(
        placed.late, 0,
        "{name}: drift correction must not drop late audio"
    );
}

/// TEST C: a device clock 200 ppm off QPC (well above typical < 100 ppm)
/// over 5, 15 and 30 minutes. Drift is detected (re-anchor count matches the
/// rate) and bounded by CONTINUITY_FRAMES (2 ms) instead of accumulating
/// (it would reach 360 ms after 30 min).
#[test]
fn c_progressive_drift_is_detected_and_bounded_5_min() {
    drift(5, 200.0);
    drift(5, -200.0);
}

#[test]
fn c_progressive_drift_is_detected_and_bounded_15_min() {
    drift(15, 200.0);
}

#[test]
fn c_progressive_drift_is_detected_and_bounded_30_min() {
    drift(30, -200.0);
}

/// TEST F: the microphone starts 2.5 s after desktop audio. Desktop markers
/// land exactly where they land without the microphone, the microphone's on
/// its own capture times.
#[test]
fn f_delayed_microphone_does_not_shift_existing_audio() {
    let duration = 8 * TICKS_PER_SECOND;
    let desktop = Source::new(0, duration);
    let microphone = Source::new(2_500 * MS + 37, duration);
    let alone = Session::new(vec![desktop], duration).run();
    let both = Session::new(vec![desktop, microphone], duration).run();
    assert_eq!(
        both.markers[0], alone.markers[0],
        "desktop audio moved when the microphone started"
    );
    assert_markers("microphone", &microphone, &both.markers[1], 5, EXACT_FRAMES);
    assert_eq!(
        both.markers[1][0].1, 120_000,
        "microphone frame 0 belongs at 2.5 s"
    );
}

/// TEST G: application audio (e.g. Discord) appearing 7.3 s after the video
/// started is placed at its capture time, not at the mixer position.
#[test]
fn g_late_application_audio_is_placed_on_the_shared_timeline() {
    let duration = 12 * TICKS_PER_SECOND;
    let game = Source::new(0, duration);
    let discord = Source::new(7_300 * MS, duration);
    let mut session = Session::new(vec![game, discord], duration);
    session.video_start = Some(0);
    let placed = session.run();
    assert_markers("game", &game, &placed.markers[0], 11, EXACT_FRAMES);
    assert_markers("discord", &discord, &placed.markers[1], 4, EXACT_FRAMES);
    assert_eq!(placed.markers[1][0].1, 350_400);
}

/// TEST H: a source vanishing for 3 s (endpoint lost, app closed) resumes on
/// its capture times: one re-anchor of exactly the gap, no lasting offset.
#[test]
fn h_interruption_resumes_without_offset() {
    let duration = 10 * TICKS_PER_SECOND;
    let source = Source {
        gap: Some((3 * TICKS_PER_SECOND, 6 * TICKS_PER_SECOND)),
        ..Source::new(0, duration)
    };
    let placed = Session::new(vec![source], duration).run();
    let indices: Vec<i64> = placed.markers[0].iter().map(|m| m.0).collect();
    assert_eq!(
        indices,
        vec![0, 1, 2, 6, 7, 8, 9],
        "markers inside the gap must be absent"
    );
    assert_markers(
        "resumed source",
        &source,
        &placed.markers[0],
        7,
        EXACT_FRAMES,
    );
    assert_eq!(placed.resync.count, 1);
    assert_eq!(placed.resync.max_frames, 3 * RATE as u64);
    assert_eq!(placed.late, 0);
}

/// TEST M: one packet with a bogus far-future QPC stamp (5 s ahead) is
/// rejected and counted; the following packets re-anchor to their real
/// times instead of continuing from the bogus one.
#[test]
fn m_timestamp_discontinuity_is_contained_and_diagnosable() {
    let mut mixer = TimelineMixer::new(QPC_ORIGIN, vec![1.0], true);
    let mut placed = Placed {
        markers: vec![Vec::new()],
        ..Placed::default()
    };
    let packet = |index: i64| -> Vec<f32> {
        let mut samples = vec![0.0; PACKET];
        samples[0] = marker_value(index);
        samples
    };
    let stamp = |n: i64| QPC_ORIGIN + n * PACKET as i64 * TICKS_PER_SECOND / RATE;
    for n in 0..50 {
        // Packet 20 claims to be 5 s in the future.
        let qpc = if n == 20 {
            stamp(n) + 5 * TICKS_PER_SECOND
        } else {
            stamp(n)
        };
        mixer.push(0, qpc, 1, &packet(n));
        render(&mut mixer, stamp(n) - RENDER_LAG, &mut placed);
    }
    render(&mut mixer, stamp(50), &mut placed);
    let positions: Vec<(i64, i64)> = placed.markers[0].clone();
    for &(index, position) in &positions {
        assert_eq!(
            position,
            index * PACKET as i64,
            "packet {index} moved after the discontinuity"
        );
    }
    assert!(
        !positions.iter().any(|m| m.0 == 20),
        "the bogus packet must not be rendered in place"
    );
    assert_eq!(positions.len(), 49);
    assert_eq!(
        mixer.late_frames(),
        PACKET as u64,
        "the bogus packet is counted, not hidden"
    );
    // Two re-anchors: onto the bogus time, then back to the real one.
    let resync = mixer.resync_stats();
    assert_eq!(
        resync.count, 2,
        "the jump and the return must both be counted"
    );
    assert_eq!(resync.max_frames, 5 * RATE as u64);
}

/// TEST N: a source delivering only silent packets neither delays nor
/// advances the others, and adds nothing to the mix.
#[test]
fn n_silent_source_does_not_move_other_sources() {
    let duration = 6 * TICKS_PER_SECOND;
    let game = Source::new(0, duration);
    let quiet = Source {
        silent: true,
        ..Source::new(1_234 * MS, duration)
    };
    let alone = Session::new(vec![game], duration).run();
    let both = Session::new(vec![game, quiet], duration).run();
    assert_eq!(both.markers[0], alone.markers[0]);
    assert!(both.markers[1].is_empty());
    assert_eq!(both.resync, ResyncStats::default());
}

/// TEST L: after a stop/start the new session has a new QPC origin. Packets
/// still stamped in the old session (before the new origin) are dropped
/// without counting as late, and the old origin does not leak into the new
/// timeline.
#[test]
fn l_restart_does_not_mix_timestamp_origins() {
    let new_origin = QPC_ORIGIN + 90 * TICKS_PER_SECOND;
    let mut mixer = TimelineMixer::new(new_origin, vec![1.0], true);
    let mut placed = Placed {
        markers: vec![Vec::new()],
        ..Placed::default()
    };
    // A packet from the previous session, 200 ms before the new origin.
    mixer.push(0, new_origin - 200 * MS, 1, &[marker_value(7); PACKET]);
    let mut first = vec![0.0; PACKET];
    first[0] = marker_value(1);
    mixer.push(0, new_origin + 100 * MS, 1, &first);
    render(&mut mixer, new_origin + 300 * MS, &mut placed);
    assert_eq!(placed.markers[0], vec![(1, 4800)]);
    assert_eq!(mixer.late_frames(), 0);

    // The video pacer of the new session rejects the old frame the same way.
    let mut pacer = FramePacer::with_origin(60, new_origin);
    assert_eq!(pacer.accept(new_origin - 200 * MS), None);
    assert_eq!(pacer.accept(new_origin + 100 * MS), Some(100 * MS));
}

// ---------------------------------------------------------------- video --

/// Feeds capture timestamps (session time) through the production pacer and
/// slot grid; returns (capture time, slot) of every frame that is encoded.
fn slots(fps: u32, captures: &[i64]) -> Vec<(i64, i64)> {
    let mut pacer = FramePacer::with_origin(fps, QPC_ORIGIN);
    let mut timeline = SlotTimeline::new(pacer.interval());
    let mut out = Vec::new();
    for &time in captures {
        let Some(sample_time) = pacer.accept(QPC_ORIGIN + time) else {
            continue;
        };
        let slot = timeline.slot_of(sample_time);
        if timeline.is_late(slot) {
            continue;
        }
        timeline.begin(slot);
        // Empty slots before it are filled with repeats (ReplaySession).
        while timeline.next() <= slot {
            timeline.advance();
        }
        out.push((time, slot));
    }
    out
}

fn assert_slots(name: &str, fps: u32, encoded: &[(i64, i64)]) {
    let interval = FramePacer::new(fps).interval();
    for &(time, slot) in encoded {
        let error = slot * interval - time;
        assert!(
            error.abs() <= interval / 2 + 1,
            "{name}: frame captured at {time} ticks shown at slot {slot} ({} ticks): {error} ticks off, \
             more than half an interval ({interval})",
            slot * interval
        );
    }
    assert!(
        encoded.windows(2).all(|pair| pair[1].1 > pair[0].1),
        "{name}: slots must increase"
    );
}

/// TEST D: frames lost under GPU load leave empty slots (filled by repeats),
/// and every frame that is encoded is still shown at its capture time. The
/// audio timeline does not depend on frame counts: the mixer has no video
/// input, only the render limit (see `d_audio_waits_for_video_but_keeps_its_times`).
#[test]
fn d_dropped_frames_keep_capture_times() {
    let interval = 166_667;
    let captures: Vec<i64> = (0..600)
        .filter(|n| !(100..130).contains(n) && n % 7 != 3)
        .map(|n| n * interval)
        .collect();
    let encoded = slots(60, &captures);
    assert_slots("60 fps with drops", 60, &encoded);
    let filled = encoded.last().unwrap().1 + 1 - encoded.len() as i64;
    assert!(filled >= 30 + 70, "{filled} slots must be repeats");
}

/// The audio render limit follows the submitted video (`pump(.., video_end)`);
/// a video stall delays rendering but never moves audio samples. The stall
/// must stay below the mixer's 1 s ring: the recorder keeps video moving with
/// repeats (replay) or `video_gap` (recording), and stops a session whose
/// clocks drift further apart.
#[test]
fn d_audio_waits_for_video_but_keeps_its_times() {
    fn stalled(now: i64) -> i64 {
        // Video stalls for 500 ms of real time, then catches up.
        if (2 * TICKS_PER_SECOND..2 * TICKS_PER_SECOND + 500 * MS).contains(&now) {
            2 * TICKS_PER_SECOND
        } else {
            now
        }
    }
    let duration = 7 * TICKS_PER_SECOND;
    let source = Source::new(0, duration);
    let mut session = Session::new(vec![source], duration);
    session.video_end = stalled;
    let placed = session.run();
    assert_markers(
        "audio during a video stall",
        &source,
        &placed.markers[0],
        6,
        EXACT_FRAMES,
    );
    assert_eq!(
        placed.late, 0,
        "audio pushed during the stall must wait in the ring"
    );
}

/// TEST E: irregular delivery (59.94 Hz source with +-4 ms jitter at 60 fps,
/// and a 144 Hz source at 60 fps) keeps every encoded frame within half an
/// interval of its capture time; no slot is used twice.
#[test]
fn e_irregular_delivery_keeps_presentation_times() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut jitter = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % 80_001) as i64 - 40_000
    };
    let ntsc: Vec<i64> = (0..3600)
        .map(|n| n * 166_833 + jitter())
        .filter(|t| *t >= 0)
        .collect();
    let mut ntsc_sorted = ntsc.clone();
    ntsc_sorted.sort_unstable();
    assert_slots("59.94 Hz jittered", 60, &slots(60, &ntsc_sorted));
    let fast: Vec<i64> = (0..1440).map(|n| n * 69_444).collect();
    let encoded = slots(60, &fast);
    assert_slots("144 Hz at 60 fps", 60, &encoded);
    assert!(
        encoded.len() >= 590,
        "144 Hz must still fill ~60 fps, got {}",
        encoded.len()
    );
}

// --------------------------------------------------- replay window / MP4 --

const AAC_FRAME: i64 = 1024;

fn layout() -> StreamLayout {
    StreamLayout {
        tracks: vec![
            TrackInfo {
                codec: CodecConfig::H264 {
                    width: 1280,
                    height: 720,
                    sps: vec![0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40],
                    pps: vec![0x68, 0xee, 0x3c, 0x80],
                },
                label: "Video".into(),
            },
            TrackInfo {
                codec: CodecConfig::Aac {
                    sample_rate: 48_000,
                    channels: 2,
                    audio_specific_config: vec![0x11, 0x90],
                },
                label: "Mix".into(),
            },
        ],
    }
}

/// Packets a replay session produces between `from` and `to` (session time):
/// 60 fps video on the slot grid with an IDR every 2 s, and AAC frames whose
/// times follow the encoder's input-derived rule (`aac_expected_time`:
/// first + n * 1024 / 48000 s, exact) starting at the mixer start `audio_start`.
fn session_gops(from_slot: i64, to: i64, audio_start: i64) -> Vec<Arc<Gop>> {
    let interval = FramePacer::new(60).interval();
    let mut packets = Vec::new();
    let mut slot = from_slot;
    while slot * interval < to {
        packets.push(EncodedPacket {
            track: 0,
            pts: slot * interval,
            dts: slot * interval,
            duration: interval,
            keyframe: (slot - from_slot) % 120 == 0,
            data: Arc::from(vec![0, 0, 0, 2, 0x65, 0]),
        });
        slot += 1;
    }
    let mut n = 0;
    loop {
        let pts = audio_start
            + (i128::from(n * AAC_FRAME) * i128::from(TICKS_PER_SECOND) / i128::from(RATE)) as i64;
        if pts >= to {
            break;
        }
        packets.push(EncodedPacket {
            track: 1,
            pts,
            dts: pts,
            duration: AAC_FRAME * TICKS_PER_SECOND / RATE,
            keyframe: true,
            data: Arc::from(vec![0x21, 0x10]),
        });
        n += 1;
    }
    // Arrival order is close to time order; GOPs open at every IDR.
    packets.sort_by_key(|packet| (packet.dts, packet.track));
    let mut gops: Vec<Gop> = Vec::new();
    for packet in packets {
        if packet.track == 0 && packet.keyframe {
            gops.push(Gop::new(gops.len() as u64, packet));
        } else if let Some(gop) = gops.last_mut() {
            gop.push(packet);
        }
    }
    gops.into_iter().map(Arc::new).collect()
}

/// Minimal reader of what a player needs to place samples: per track the
/// media timescale, the edit list and the decode times from `stts`.
#[derive(Debug)]
struct ParsedTrack {
    video: bool,
    timescale: u64,
    /// Empty edit (movie timescale 1000) before the media starts.
    lead_ms: u64,
    /// First presented media time (track timescale).
    media_time: u64,
    /// Decode time of each sample (track timescale, from 0).
    times: Vec<u64>,
}

fn be32(b: &[u8], at: usize) -> u64 {
    u64::from(u32::from_be_bytes(b[at..at + 4].try_into().unwrap()))
}

fn boxes(b: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= b.len() {
        let size = be32(b, at) as usize;
        out.push((&b[at + 4..at + 8], &b[at + 8..at + size]));
        at += size;
    }
    out
}

fn find<'a>(b: &'a [u8], kind: &[u8]) -> &'a [u8] {
    boxes(b)
        .into_iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, body)| body)
        .unwrap_or_else(|| panic!("missing box {}", String::from_utf8_lossy(kind)))
}

fn parse(file: &[u8]) -> Vec<ParsedTrack> {
    let moov = find(file, b"moov");
    boxes(moov)
        .into_iter()
        .filter(|(kind, _)| *kind == b"trak")
        .map(|(_, trak)| {
            let mdia = find(trak, b"mdia");
            let mdhd = find(mdia, b"mdhd");
            assert_eq!(mdhd[0], 0, "mdhd version 0 expected");
            let timescale = be32(mdhd, 12);
            let video = &find(mdia, b"hdlr")[8..12] == b"vide";
            let elst = find(find(trak, b"edts"), b"elst");
            assert_eq!(elst[0], 0, "32-bit edit list expected for short clips");
            let (mut lead_ms, mut media_time) = (0, 0);
            for entry in 0..be32(elst, 4) as usize {
                let at = 8 + entry * 12;
                let time = i32::from_be_bytes(elst[at + 4..at + 8].try_into().unwrap());
                if time < 0 {
                    lead_ms += be32(elst, at);
                } else {
                    media_time = time as u64;
                }
            }
            let stts = find(find(find(mdia, b"minf"), b"stbl"), b"stts");
            let mut times = Vec::new();
            let mut time = 0;
            for run in 0..be32(stts, 4) as usize {
                let (count, delta) = (be32(stts, 8 + run * 8), be32(stts, 12 + run * 8));
                for _ in 0..count {
                    times.push(time);
                    time += delta;
                }
            }
            ParsedTrack {
                video,
                timescale,
                lead_ms,
                media_time,
                times,
            }
        })
        .collect()
}

/// Presentation time (seconds) of sample `index` as a player computes it.
fn presented(track: &ParsedTrack, index: usize) -> f64 {
    track.lead_ms as f64 / 1000.0
        + (track.times[index] as f64 - track.media_time as f64) / track.timescale as f64
}

/// Saves `requested` ending at `end_pts` and checks, in the written file, that
/// every audio frame is presented at the same clip time as session time says
/// relative to the video: |(audio - video) in file - (audio - video) in
/// session| within the container's rounding (90 kHz video, 48 kHz audio,
/// 1 ms movie timescale for an empty lead edit).
fn check_saved_clip(name: &str, gops: &[Arc<Gop>], requested: i64, end_pts: i64) -> (i64, f64) {
    let layout = layout();
    let plan = plan_clip(&layout, gops, requested, end_pts).unwrap();
    let mut file = Vec::new();
    write_mp4(&mut file, Path::new("sync.mp4"), &layout, &plan).unwrap();
    let tracks = parse(&file);
    let video = tracks.iter().find(|t| t.video).unwrap();
    let audio = tracks.iter().find(|t| !t.video).unwrap();
    assert_eq!(video.times.len(), plan.video.samples.len());
    assert_eq!(audio.times.len(), plan.audio[0].samples.len());

    // Video sample i is presented at (pts_i - start_pts) by construction.
    let v0 = presented(video, 0);
    let first_video = plan.video.samples[0].pts;
    assert!(
        (v0 - (first_video - plan.start_pts) as f64 / 1e7).abs() < 1.0 / 90_000.0,
        "{name}: video does not start at the keyframe"
    );
    let tolerance = 0.5 / 1000.0 + 1.0 / 90_000.0 + 1.0 / 48_000.0;
    let mut worst = 0.0f64;
    for (index, sample) in plan.audio[0].samples.iter().enumerate() {
        let expected = (sample.pts - first_video) as f64 / 1e7;
        let actual = presented(audio, index) - v0;
        worst = worst.max((actual - expected).abs());
    }
    assert!(
        worst <= tolerance,
        "{name}: audio presented {:.1} us away from its session time relative to video \
         (tolerance {:.1} us; start_pts {} ticks, audio lead {} ms, media_time {})",
        worst * 1e6,
        tolerance * 1e6,
        plan.start_pts,
        audio.lead_ms,
        audio.media_time
    );
    // AAC frames are exactly 1024 samples apart in the 48 kHz track.
    assert!(
        audio.times.windows(2).all(|pair| pair[1] - pair[0] == 1024),
        "{name}: AAC stts deltas are not exactly 1024"
    );
    // Coverage: the buffered audio spans the whole clip, so the clip's audio
    // must start with the video keyframe (not at the requested time, which
    // would leave silence the edit list hides) and last as long as the video,
    // to one AAC frame.
    let aac = 1024.0 / 48_000.0;
    // The first AAC frame may begin before the keyframe (media_time skips
    // into it), never after it.
    let audio_start = presented(audio, 0) - v0;
    assert!(
        audio.lead_ms == 0 && audio_start <= 1e-9 && audio_start > -aac,
        "{name}: audio starts {:.1} ms after the video keyframe (lead {} ms)",
        audio_start * 1e3,
        audio.lead_ms
    );
    let audio_end = presented(audio, audio.times.len() - 1) + aac;
    let video_end = plan.duration as f64 / 1e7;
    assert!(
        (audio_end - video_end).abs() < 2.0 * aac,
        "{name}: audio ends at {audio_end:.4} s, video at {video_end:.4} s"
    );
    (plan.start_pts, worst)
}

/// TEST J: the requested start (end - 60 s) is 0.4 s after a keyframe. The
/// clip starts at that keyframe and the audio starts with it, not at the
/// requested time; the audio packet straddling the keyframe is entered via
/// the edit list's media_time.
#[test]
fn j_replay_start_on_a_keyframe_keeps_audio_aligned() {
    // 300 s session, end at 300.4 s: requested start 240.4 s, keyframe 240 s.
    let gops = session_gops(0, 300 * TICKS_PER_SECOND + 4_000_000, 0);
    let end = gops.iter().map(|gop| gop.end_pts).max().unwrap();
    let (start, _) = check_saved_clip("keyframe boundary", &gops, 60 * TICKS_PER_SECOND, end);
    let interval = FramePacer::new(60).interval();
    assert_eq!(
        start,
        14_400 * interval,
        "the keyframe at 240 s must start the clip"
    );
    assert!(end - start >= 60 * TICKS_PER_SECOND && end - start <= 62 * TICKS_PER_SECOND);
}

/// TEST K + O: a long-running replay (session time beyond 30 days) and
/// several saves from the same packets; each clip is aligned, nothing
/// overflows, and the session time origin does not leak into the file.
#[test]
fn k_long_running_replay_saves_stay_aligned() {
    let interval = FramePacer::new(60).interval();
    let day = 24 * 3600 * TICKS_PER_SECOND;
    let first_slot = (31 * day) / interval;
    let from = first_slot * interval;
    // Audio started at the first video slot, as `begin_video` does, but the
    // mixer block time is floored to whole frames.
    let audio_start =
        (i128::from((i128::from(from) * 48_000 / 10_000_000) as i64) * 10_000_000 / 48_000) as i64;
    let gops = session_gops(first_slot, from + 130 * TICKS_PER_SECOND, audio_start);
    let newest = gops.iter().map(|gop| gop.end_pts).max().unwrap();
    for (save, end) in [
        newest - 50 * TICKS_PER_SECOND,
        newest - 20 * TICKS_PER_SECOND,
        newest,
    ]
    .into_iter()
    .enumerate()
    {
        let (start, worst) =
            check_saved_clip(&format!("save {save}"), &gops, 60 * TICKS_PER_SECOND, end);
        assert!(start >= from && start <= end - 60 * TICKS_PER_SECOND);
        assert!(worst < 1e-3);
    }
}
