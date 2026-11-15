# Instant replay buffer correctness

Status: audit of `main` at `5c5b7c0`, two fixes, and a deterministic test
suite (`crates/clipper-replay/tests/correctness.rs`). All of it runs on macOS
and on the Windows CI runners; **no replay was captured on Windows hardware for
this report** and no process memory was measured. Byte figures below are the
buffer's logical accounting.

## Architecture (as implemented)

```text
PacketEncoder (H.264 AVCC track 0, AAC tracks 1..)       one session timeline,
  └─ ReplaySession::drain_packets ─ ReplayBuffer::push    100 ns ticks
       RamTier (ram.rs): GOP assembly, accounting
         · before the first video IDR everything is discarded
         · a video IDR seals the open GOP and opens the next
       Core (hybrid.rs): retention, budget, spill to SSD, snapshots
SaveReplay ─ ReplayBuffer::snapshot (Arc clones) ─ Exporter thread
  ─ plan_clip (keyframe start, discontinuity cut, audio trim) ─ write_mp4
```

- **Unit of storage: the GOP** (`gop.rs:13`). It holds every packet (video and
  all audio tracks) that arrived from one IDR up to the next, in arrival
  order. Payloads are `Arc<[u8]>`: snapshots, spills and exports share them
  without copying.
- **One buffer per replay session.** The `StreamLayout` (SPS/PPS, AAC config,
  labels) is fixed per buffer; the encoder keeps the first parameter sets it
  sees (`packet_encoder.rs:770`). A restart creates a new session, a new
  encoder and a new buffer, so sessions are never combined. The capture
  recovery work in #61 restarts exactly this way and needs no separate
  session identity.
- **Validation at `push`** (`ram.rs:61`): track index in the layout, video dts
  strictly increasing, and (new) `pts >= 0` and `duration > 0`. A rejected
  packet changes nothing. The recorder counts it as `invalid_packets` and
  goes on (`replay_session.rs:500`).

## Capacity and eviction

| Limit | Rule | Source |
| --- | --- | --- |
| Duration | When a GOP is sealed, every GOP before "one GOP before the GOP containing `newest_end - duration`" is evicted. The buffer therefore holds `duration` plus up to 3 GOPs (reserve, containing, open): 60 s → up to 66 s at a 2 s GOP. | `hybrid.rs:304`, `:323` |
| RAM, SSD tier on | Excess over the budget is handed to the spill thread. RAM is capped at 2 × budget (or the usage at the moment the budget was lowered, shrinking). | `hybrid.rs:343` |
| RAM, RAM only | Sealed GOPs are evicted down to the budget. The open GOP may add up to one more GOP (2 × budget at most). | `hybrid.rs:331` |
| Runaway open GOP | An open GOP above 2 × budget (no keyframes arriving, one huge keyframe) is discarded; saves are `NotReady` until the next IDR. | `ram.rs:142` |
| SSD | Per-session byte limit; segments are deleted when evicted and no snapshot holds them (`Arc<Segment>` drop). | `disk.rs:209` |

**Accounting.** Each packet is charged its payload plus 64 bytes
(`PACKET_OVERHEAD`) for bookkeeping. Sealed bytes are kept in a counter
(O(1) per push and pop), and the open GOP keeps its own sum. The
generated-history test checks after every one of 256,000 operations that the
counter equals the sum of the retained GOPs' charges. It is a logical budget:
`Vec` capacity, `Arc` headers, allocator overhead and snapshot-held GOPs are
not included, so it is not the process size.

**Eviction can never strand video without a keyframe**: whole GOPs are
evicted, and every GOP starts with its IDR. Spilled GOPs stay contiguous; a
gap (a GOP evicted before its spill finished) clears the older disk entries
(`hybrid.rs:241`).

## Keyframe and window policy (`plan_clip`)

- The clip starts at the **latest keyframe at or before `end - requested`**.
  It is between the requested length and one GOP longer, never shorter
  unless the buffer holds less. With keyframes at 239.5 / 241.5 / 243.5 s
  and a request starting at 240.0 s, the clip starts at 239.5 s.
- If no keyframe at or before that point is buffered any more, the **oldest
  buffered keyframe** is used and the clip is shorter. `SavedClip::duration`
  reports the real length; the UI shows it.
- A clip never starts on a predicted frame: no keyframe means `NotReady`.
- **New: a clip never spans a video discontinuity**, meaning consecutive decode
  times more than `MAX_VIDEO_GAP` (1 s) apart (`plan.rs:40`, `:158`). It
  starts at the first keyframe after the newest one, and is `NotReady` until
  such a keyframe exists.
- Video is cut at the first frame (decode order) reaching past `end`; later
  B-frames would reference it.

## Audio/video alignment

The clip's time zero is the start keyframe's pts. Every audio packet
overlapping `[start, video_end)` is kept once. The packet straddling the start
is entered via the edit list `media_time`; audio starting after the keyframe
gets an empty `lead` edit (`plan.rs:218`). Audio is never shifted to zero
independently of the video. The muxer side (absolute-time conversion,
1024-sample AAC deltas) is covered by `docs/quality/av-sync-audit.md`, and
none of it changed here.

## Snapshots and concurrency

`snapshot` takes `Arc` clones of the needed GOPs (RAM or disk) plus a clone
of the open GOP's packet list, in O(GOPs + packets of the open GOP), under the
buffer's own lock and without I/O. Exports run on the export thread from that
snapshot only. Eviction, spilling, a dropped buffer (replay stopped) and a new
session cannot change or free what a snapshot references: RAM GOPs live as
long as their `Arc`, and segment files (and the session directory) are
deleted only after the last snapshot holding them is dropped. Memory held by
pending saves is bounded by the export queue (1 running + 3 queued snapshots).

## Defects found and fixed

| # | Defect | Reproduction | Fix | Test |
| --- | --- | --- | --- | --- |
| 1 | A packet with `duration <= 0` or `pts < 0` was accepted into the buffer although it breaks the packet contract (`packet.rs`). The planner rejects such packets for the whole clip. | One zero-duration audio packet made every save fail with "invalid replay data: packet with non-positive duration" for as long as it stayed in the replay window (up to duration + 3 GOPs). | Rejected at `push`, as already done for bad video dts. The recorder drops and counts it. | `v_contract_violations_are_rejected_and_saves_keep_working` (fails without the check) |
| 2 | A forward timestamp jump inside the buffer produced a clip spanning the jump. | 20 s of footage, a 1 h jump, 3 s more, then a 10 s save gave a clip reporting **3605 s**: the reserve GOP before the jump, then the jump written as one frozen frame. Players would also decode the audio after it back to back, out of sync. | `plan_clip` never spans a gap > 1 s between consecutive video decode times. | `t_clips_never_span_a_timestamp_discontinuity`, `generated_histories_keep_the_invariants` (both fail without the rule) |

Neither input is produced by the current recorder in normal operation. The
encoder keeps durations positive and pts non-negative, and the constant frame
rate grid fills empty slots with repeats. They become possible through
encoder or driver misbehaviour, or through the existing discard of a runaway
open GOP (which leaves a gap of about one GOP). The buffer and planner now
hold their own contract instead of relying on the producer.

One existing unit test (`plan::tests::negative_dts_lead_in_at_session_start_is_exportable`)
used a fixture with a 1.4 s hole between its two GOPs. It now shifts the
second GOP to follow the first; what it tests (negative dts lead-in) is
unchanged.

## Tests added (`tests/correctness.rs`)

| Test | Scenario |
| --- | --- |
| `a_empty_buffer_is_not_ready` | A: empty / audio-only → `NotReady`, nothing retained |
| `b_short_buffer_keeps_everything_and_reports_the_real_length` | B: 10 s buffered, 60 s requested → 10 s clip, reported as such |
| `c_g_duration_limit_and_keyframe_before_the_requested_start` | C + G: eviction bound; start = keyframe at 90 s for a request starting at 90.4 s |
| `h_oldest_keyframe_when_history_is_short` | H: request beyond history → oldest keyframe, shorter clip |
| `d_byte_budget_bounds_ram_only_buffers` | D: RAM-only budget never exceeded beyond 2 × |
| `e_oversized_keyframe_is_dropped_not_kept` | E: 3 × budget keyframe dropped, `NotReady`, recovery at the next IDR |
| `f_missing_keyframe_never_yields_a_clip` | F: no IDR → nothing buffered, no clip |
| `i_j_dropped_and_irregular_frames_keep_their_timestamps` | I + J: every 9th frame and a 30-frame run dropped, ±2 ms jitter; length from timestamps |
| `k_audio_overlapping_the_clip_start_is_kept_once` | K: straddling AAC frame kept once via `media_time` |
| `l_u_large_origins_preserve_relative_timing` | L + U: origins of 37 days, 2^60 and `i64::MAX` − 1 h give the same relative clip; export works |
| `m_n_snapshots_are_independent_and_immune_to_eviction` | M + N: an export after 7 more minutes of recording is byte-identical |
| `r_rapid_repeated_saves` | R: 400 saves while recording; accounting unchanged |
| `o_p_x_restart_and_drop_during_save` | O + P + X: SSD-tier snapshot exported after its buffer was dropped and a new session (other resolution, timeline from 0) started; segment files removed afterwards |
| `s_extreme_durations_are_handled` | S: `ZERO`, 1 ns and `Duration::MAX` |
| `t_clips_never_span_a_timestamp_discontinuity` | T: fix 2; short gaps (< 1 s) stay inside clips |
| `v_contract_violations_are_rejected_and_saves_keep_working` | V: fix 1; duplicate/out-of-order video rejected, duplicate/out-of-order audio normalized |
| `w_audio_interruption_keeps_the_buffer_consistent` | W: 5 s without audio packets |
| `q_long_sessions_stay_bounded_and_saveable` | Q: 30 min, 1 h, 3 h, 6 h at 60 fps (≈ 2.3 M packets) |
| `generated_histories_keep_the_invariants` | Property test: 64 seeds × 4000 random operations |

The property test uses a small seeded xorshift generator instead of a new
dependency (the workspace has no property-testing crate). Failures name the
seed and step. Generated operations: variable GOP lengths and packet sizes,
dropped frames, ±2 ms interval jitter, forward discontinuities resuming
mid-GOP, live duration and budget changes, and snapshots with random lengths.
Checked after every step:

- the RAM accounting equals the retained charges and stays within the bound;
- every plan is structurally valid and starts on a keyframe;
- no plan spans a discontinuity;
- nothing panics.

Results on macOS (debug build), from the test output:

- **Long sessions:** 60 s buffer, 8 MiB budget, 300 kbit/s synthetic stream. At 30 min, 1 h, 3 h and 6 h the clip is 60.0001 s long and ends at the newest footage. The RAM accounting is constant at about 4.04 MB (≤ 33 GOPs), with no growth over time.
- **Speed:** the whole file runs in about 2.3 s.
- **Totals:** `clipper-replay` and `clipper-recorder` together pass 155 tests, 0 failing.

## Performance

- `push`: two integer comparisons added (O(1)).
- `plan_clip` (export thread, once per save): one linear pass over the video
  packets it already sorts.
- No change to the encoder, capture, buffer data structures or copying.

## Known limitations and observations

- `BufferStats::buffered` measures from the oldest retained keyframe to the
  newest packet, so right after a discontinuity it includes the gap until
  retention drops the pre-gap GOPs (status display only; clips are cut).
- An audio-only gap inside a clip (never produced by the gap-free mixer) is
  written as one stretched AAC sample; players may then shift later audio.
- With the SSD tier on, the 2 × RAM cap can evict a GOP whose spill is still
  in flight. That GOP is then neither in RAM nor on disk, and the disk tier
  restarts after the gap. It only happens when the disk cannot keep up
  (a GOP larger than the budget, or a spill slower than real time).
- The spill and SSD paths depend on real disk timing; their tests pace input
  like real time.

## Windows hardware validation (pending)

1. 60 s replay at 1080p60 for 2 hours; save every 10 minutes; every clip
   60–62 s, playable, no stutter at the start.
2. Save twice within one second; both clips valid and independent.
3. Save, then stop the replay immediately; the clip still completes.
4. Lower the RAM budget to 16 MiB at 100 Mbps with the SSD tier on; check
   `dropped_gops` and the buffered length.
5. Disable the SSD tier, fill the budget, save; the clip is shorter but valid.
6. Sleep/resume the PC with replay running, then save; the clip does not span
   the sleep.
7. Restart replay with another resolution and save from both sessions.
