# Audio/video synchronization audit

Status: code audit and deterministic regression tests, run on 2026-11-15
against `main` at `0427097`. **No live Windows recording was made for this
audit**; everything below about real devices is either read from the code or
listed as a hardware test still to run.

## Result

The audit found **no synchronization defect** in the timestamp path from capture
to the MP4 file. All tracks of a session share one clock (QPC, 100 ns ticks,
zero at the audio mixer origin), conversions are integer and computed from
absolute positions (no accumulating rounding), audio is placed by capture
timestamp rather than arrival order, device clock drift is bounded to 2 ms,
and replay clips start audio and video together at the selected keyframe.

This change therefore adds no timing fix. It adds:

- 16 deterministic scenario tests (`crates/clipper-recorder/src/av_sync_tests.rs`)
  that drive the production timing code with synthetic input and check the
  written MP4, verified by mutation (see [Test quality](#test-quality)).
- Two aggregated diagnostics: mixer re-anchors (count and largest correction)
  and repeated video frames, logged once when a replay stops.
- This report and a manual hardware test plan.

The largest remaining risk is outside what can be tested without hardware:
the AAC encoder's priming delay (see [Risks](#risks-and-open-questions)).

## Timestamp flow

```text
WGC frame ── SystemRelativeTime (QPC, 100 ns) ─┐
                                               ├─ FramePacer (origin = mixer origin) ─ relative t
                                               │     replay:    SlotTimeline: slot = round(t / I), pts = slot * I (CFR, repeats)
                                               │     recording: pts = t (VFR), Media Foundation sink writer
WASAPI packet ── u64QPCPosition (100 ns) ──────┤
  (48 kHz f32, first frame of the packet)      └─ TimelineMixer: position = (qpc - origin) * 48000 / 10^7
                                                     render: block time = position * 10^7 / 48000
                                                     ─ AAC MFT ─ pts = first + n * 1024 / 48000 s (exact)
Replay: ReplayBuffer (GOPs) ─ plan_clip (keyframe start, audio edit) ─ write_mp4 (90 kHz / 48 kHz / 1 kHz)
```

| Stage | Clock and unit | Source |
| --- | --- | --- |
| Video capture time | QPC, 100 ns (`TimeSpan`) | `crates/clipper-capture/src/windows/mod.rs:475` |
| Audio packet time | QPC of the packet's first frame, 100 ns (`u64QPCPosition`); on `TIMESTAMP_ERROR` or 0 the previous end is used and a discontinuity flagged | `crates/clipper-audio/src/windows/stream.rs:644`, `:656` |
| Audio format | Always 48 kHz f32: WASAPI converts other device rates (`AUTOCONVERTPCM`, `SRC_DEFAULT_QUALITY`), also for process loopback | `crates/clipper-audio/src/windows/stream.rs:212` |
| Session origin | QPC at audio start; the video pacer is created with the same origin | `crates/clipper-core/src/pipeline.rs:34`, `crates/clipper-recorder/src/replay_session.rs:143` |
| Frame interval | `10^7 / fps`, truncated (60 fps: 166 666 ticks) | `crates/clipper-core/src/pipeline.rs:27` |
| Replay video pts | `round(t / I) * I`: within ±I/2 of the capture time, empty slots repeat the previous frame | `crates/clipper-recorder/src/logic.rs:201` |
| Audio placement | Per source: continue seamlessly while the QPC position is within 96 frames (2 ms) of the previous packet's end, otherwise place at the QPC position (re-anchor) | `crates/clipper-audio/src/mixer.rs:15`, `:188` |
| Audio start (replay) | `skip_to(origin + first slot time)`; audio before the first video frame is discarded | `crates/clipper-recorder/src/replay_session.rs:349`, `:369` |
| Audio render limit | `min(now - 80 ms, video end)`; bounded to the 1 s ring | `crates/clipper-recorder/src/recording_audio.rs:680`, `crates/clipper-audio/src/mixer.rs:243` |
| AAC packet pts | MFT output time if within one AAC frame of `first + n * 1024 / 48000 s` (i128), else that expected time | `crates/clipper-encoder/src/backends/packet_encoder.rs:890`, `:899` |
| Video dts | Derived from pts with the encoder's reorder depth (B-frames supported) | `crates/clipper-encoder/src/backends/packet_encoder.rs:445` |
| Replay window | Start = latest keyframe at or before `end - requested`; audio keeps every packet overlapping the video span, entered via edit list `media_time`, late audio via an empty `lead` edit | `crates/clipper-replay/src/plan.rs:107`, `:200` |
| MP4 timing | Absolute times converted per sample (no delta accumulation): video 90 kHz, audio 48 kHz, edit lists in the 1 kHz movie timescale | `crates/clipper-replay/src/mp4.rs:132`, `:355`, `:26` |
| Manual recording | VFR video with real capture times; static scenes are declared as gaps (`video_gap`); MF sink writer muxes | `crates/clipper-recorder/src/windows_worker.rs:126` |

`clipper_audio::timeline::TrackTimeline` (pad/skip with a 10 ms tolerance and a
ppm estimator) is not used by the recorder, only by `examples/audio_smoke.rs`.
The production placement is `TimelineMixer`.

## Findings by area

**Common timebase.** One clock (QPC) and one unit (100 ns) everywhere up to the
muxer. No wall-clock time, millisecond truncation or per-stream origin is
involved. Conversions use i128 intermediates; i64 ticks overflow after
29 000 years. The test with session times beyond 31 days passes.

**Video.** WGC timestamps are preserved, not synthesized: a replay frame is shown
in the slot nearest its capture time (error ≤ I/2, 8.3 ms at 60 fps), so
irregular delivery and dropped frames never shift later frames. The truncated
interval (166 666 instead of 166 666.67 ticks) cannot accumulate: the slot is
recomputed from each frame's absolute time. Replay output is constant frame
rate (repeats), manual recordings are variable frame rate.

**Audio drift.** A device clock off QPC by *p* ppm moves a source by
*p* · 48 000 frames per second against its timestamps. The mixer follows the
sample count until the offset passes 96 frames, then re-anchors: a 2 ms gap
(silence) for a slow clock, a 2 ms overlap (overwritten) for a fast one. Drift
is therefore detected and corrected, bounded at 2 ms, at the cost of one tiny
discontinuity per re-anchor (every ~10 s at 200 ppm). It is not resampled.

**Multi-source.** Every source is placed by its own timestamps. Later starts,
pauses, silent packets or a missing source do not move other sources, and the
mix and the isolated tracks share positions. Sample rates are unified to 48 kHz
by WASAPI before the mixer, which assumes 48 kHz.

**Discontinuities.** A bogus far-future timestamp is rejected as late (counted)
and the next packets re-anchor to their real time. Packets stamped before the
session origin (a previous session) are ignored. A clock jump cannot render
more than one second of silence per step, and the session stops when audio and
video clocks drift apart (`replay_session.rs:457`, `windows_worker.rs:120`).

**Replay window.** The clip starts at the keyframe, not at the requested time,
and the audio starts with that keyframe: the AAC frame straddling it is entered
via `media_time`. A clip is between the requested length and one GOP (2 s)
longer. Audio is trimmed to the video span on AAC frame granularity (21.3 ms).

**Muxing.** stts deltas come from converted absolute times, so their sum equals
the converted track length. AAC deltas are exactly 1024 samples. Video uses
ctts plus an edit list for B-frames. Empty lead edits are quantized to the
1 ms movie timescale (at most 0.5 ms; only when audio starts after the
keyframe, i.e. at the very start of a session).

## Diagnostics added

When a replay stops, the recorder logs one extra line next to the existing
counters:

```text
replay stop: a/v sync: N audio re-anchors (largest M frames at 48 kHz), R repeated video frames, L audio frames lost
```

- *Re-anchors* (`TimelineMixer::resync_stats`): a steady rate means device clock
  drift (rate ≈ |ppm| · 48 000 / 97 per second); a large maximum means a
  discontinuity (device restart, application pause, bad timestamp).
- *Repeated video frames*: slots filled with the previous frame (static scene,
  dropped or late capture frames).

Cost: one comparison already present plus two integer updates per re-anchored
packet; no allocation, no per-packet log, no new thread or polling.

## Regression tests

`cargo test -p clipper-recorder --lib av_sync` runs on any OS, with no devices
or GPU. Audio markers encode their index in the sample value, so each test
compares where every marker landed with the real time it was captured.

| Test | Scenario | Expected | Tolerance |
| --- | --- | --- | --- |
| A + O | Two sources and video start together; QPC origin ≈ 3.4 days | Markers at capture time; output starts at video start | 1 frame (20.8 µs): tick rounding of stamps, frame truncation |
| B | Audio starts 150 ms after video | First sample at exactly 7200 frames; silence before | exact / 1 frame |
| C | Device clock ±200 ppm for 5, 15, 30 min | Re-anchor count = drift / 97 ± 1; error ≤ 97 frames at the end as at the start; nothing late | 97 frames (2 ms bound) |
| D | 30 consecutive + every 7th frame dropped at 60 fps | Encoded frames within ±I/2 of capture time; ≥ 100 repeats | I/2 + 1 tick |
| D (stall) | Video submission stalls 500 ms | Audio renders later but at unchanged positions; nothing lost | 1 frame |
| E | 59.94 Hz with ±4 ms jitter; 144 Hz source at 60 fps | Within ±I/2, slots strictly increasing, ≥ 590 of 600 slots | I/2 + 1 tick |
| F | Microphone starts 2.5 s after desktop | Desktop markers identical to a run without microphone | exact |
| G | Application audio starts 7.3 s in | Placed at 7.3 s (350 400 frames) | exact / 1 frame |
| H | Source absent for 3 s | One re-anchor of exactly 3 s; markers after it exact | 1 frame |
| J | Request starts 0.4 s after a keyframe (save at 300.4 s, 60 s) | Clip starts at the 240 s keyframe; audio starts with it, ends with the video; each AAC frame presented at its session time relative to video | 0.53 ms (1 ms movie timescale + 90 kHz + 48 kHz rounding) |
| K | Session time > 31 days, three saves from one buffer | As J for every save; AAC stts deltas exactly 1024 | as J |
| L | Restart: packet/frame of the previous session | Ignored without counting late; pacer rejects it | exact |
| M | One packet stamped 5 s in the future | Counted late (480 frames), not rendered; two re-anchors of 5 s reported; all other packets exact | exact |
| N | One source only sends silent packets | Other sources identical to running alone | exact |

The MP4 checks parse the written file (mdhd timescale, elst, stts) and compute
presentation times the way a player does.

Not covered as separate tests: different device sample rates (TEST I), because
the conversion happens inside WASAPI, before any clipuh code. The mixer and
encoder only ever see 48 kHz. B-frame reordering (`DtsAssigner`) is Windows-only
code and is covered by the existing tests in `plan.rs`/`mp4.rs` and the encoder's
hardware tests.

### Test quality

Each property was checked by breaking the production code and confirming that
the tests fail (changes reverted):

| Mutation | Caught by |
| --- | --- |
| Mixer always continues a source (arrival-order placement, never re-anchors) | C ×3, H, L, M |
| Planner ignores the audio edit offset (`media_time = 0`) | J, K (20 ms / 10 ms off) |
| Planner trims audio at the requested time instead of the keyframe | J, K (audio starts 406 ms / 11 ms late) |
| Slot = floor instead of round | E (frame 15 ms off) |
| Mixer block time computed with 48 001 Hz (21 ppm) | A, B, D, H (growing marker error) |

## Risks and open questions

1. **AAC encoder priming delay (unverified, highest risk).** AAC-LC encoders
   typically add one or two frames of priming at the start of the bitstream.
   The packet times follow the input times, and the MP4 writer adds no edit to
   skip priming. If the Microsoft AAC MFT emits priming samples, decoded audio
   could be late by about 21–43 ms in every clip. This can only be measured
   with real output (flash and beep test below). If confirmed, the fix belongs
   in the audio edit list (`media_time`), not in a global delay.
2. **MFT output-time tolerance.** `aac_presentation_time` trusts the MFT's time
   when it is within one AAC frame (21.3 ms) of the expected time. A constant
   MFT offset below that would pass through unnoticed. The hardware test
   measures it.
3. **Drift handling is a 2 ms bounded re-anchor, not resampling.** It is inaudible
   as a sync error but causes a sub-millisecond gap/overlap every 10–50 s on
   drifting devices. The new re-anchor counter makes the rate visible.
4. **Session-start audio hole.** Audio read while no video existed is discarded;
   if the first video slot time is earlier than the last discard, the first few
   tens of milliseconds of the session are silent. This affects only the first
   seconds of a replay session, never alignment.
5. **Manual recording.** Muxing is done by Media Foundation's sink writer from
   the same timestamps; its internal behaviour is not covered by these tests.
6. **WASAPI timestamp semantics** (QPC of the first frame) and WGC
   `SystemRelativeTime` are taken from the documentation and the code; their
   accuracy on real drivers is a hardware question.

## Hardware validation plan (to be run, not done)

Reference signal: a test page or program that shows a full-screen white flash
and plays a 1 kHz, 10 ms click at the same instant once per second (vary the
page so frames change). Measure with a frame-accurate tool, for example
`ffprobe -show_frames` for the flash frame (luma jump) and the click onset in
the decoded audio (`ffmpeg -i clip.mp4 -map 0:a:0 -f f32le`). Offset = audio
onset − video flash, per event. Pass: |offset| ≤ one video frame (16.7 ms at
60 fps) everywhere, and the offset does not trend over time (first vs last
event differ by < 5 ms).

| # | Setup | Check |
| --- | --- | --- |
| 1 | AMD Radeon (AMF), 1080p60 instant replay, desktop audio | Offset per event over a 60 s clip; record the constant part (priming, risk 1) |
| 2 | NVIDIA (NVENC), same | Same |
| 3 | Intel Quick Sync, same | Same |
| 4 | 30 min manual recording with game audio + microphone | Offset at minute 0, 15 and 30; no trend |
| 5 | Replay running 2 h, saves every 10 min | Offset per save, `a/v sync` log line at stop |
| 6 | Game + Discord + microphone, separate tracks | Offset per isolated track and in the mix |
| 7 | Discord started after the replay | Its track starts at the right time |
| 8 | Save while the encoder is at high GPU load (game at 100 % GPU) | Offset, repeated frames, re-anchors |
| 9 | Unplug/replug a USB microphone during a replay | Offset after replug; re-anchor logged |
| 10 | Restart replay (stop/start) and save from the new session | Offset; no audio from the old session |
| 11 | Play and seek the saved files in Windows Media Player, VLC and the clipuh editor | Sync after seeking; durations match |
