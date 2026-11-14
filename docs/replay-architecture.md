# Hybrid instant replay buffer

## Pipeline

```text
WGC capture ─▶ FramePacer ─▶ PacketEncoder (D3D11 VP BGRA→NV12, async HW H.264 MFT)
                                  │ AVCC packets (track 0)
WASAPI ─▶ TimelineMixer ─▶ AAC MFT ─┤ raw AAC packets (tracks 1..)
                                  ▼
                       ReplayBuffer::push  (recorder worker thread, no I/O)
                       ├── RAM tier: Vec<Arc<Gop>> within memory budget
                       └── spill thread ─▶ SegmentStore (SSD, *.tmp + rename)
SaveReplay ─▶ ReplayBuffer::snapshot (Arc clones, O(GOPs))
          ─▶ Exporter thread ─▶ plan (keyframe select, trim, rebase)
          ─▶ mp4 muxer ─▶ <clip>.mp4.partial ─▶ flush ─▶ rename ─▶ ReplaySaved
```

Crates: `clipper-replay` (pure Rust, no UI, no platform code except a free-space
query) owns buffer, segments, planning and MP4 muxing. `clipper-encoder` owns
the Windows packet encoder. `clipper-recorder` wires capture, audio, encoder,
buffer and exporter on its worker thread. The UI only sends commands and
renders events.

## Contract (`clipper-replay::packet`)

* Time base: 100 ns ticks, one monotonic session timeline for all tracks
  (QPC; zero = audio mixer origin, same as manual recordings).
* Track 0 = H.264 video (AVCC payload, SPS/PPS in `CodecConfig`), tracks 1..
  = AAC-LC raw access units. Track 1 is the primary mix.
* `dts <= pts`, `dts` strictly increasing per track. Audio: `pts == dts`.
* Packets are encoded data only: no raw frames are ever buffered beyond the
  4-texture encoder input pool.

## GOP grouping and keyframes

Packets are grouped by arrival into GOPs, each opened by a video IDR. The GOP
is the unit of eviction, spill and export. GOP length is 2 s (`gop_frames =
2 * fps`). Audio for the first ~100 ms of a GOP may arrive while the previous
GOP is still open, so retention and snapshots keep one extra GOP before the
first exported keyframe.

Without re-encoding a clip must start at an IDR. The exporter picks the
latest keyframe at or before `end - requested`, so a clip is between the
requested length and requested + one GOP (≤ 2 s) longer; the actual length is
reported in `SavedClip::duration`. Audio is trimmed to the video start/end on
AAC frame granularity (21.3 ms).

## Memory and SSD

RAM holds at most `memory_limit_bytes` (default 64 MiB) of charged packet
bytes (payload + 64 B overhead per packet). Older GOPs are handed to a spill
thread over a bounded channel and written as immutable segment files in a
locked per-session directory under the cache root. Segments are deleted when
evicted and no export snapshot references them (`Arc<Segment>` drop). Stale
session directories of crashed processes are removed on the next start; only
files under the app-owned cache root are ever deleted, never saved clips.

If the SSD tier fails (disk full, permissions) the buffer continues RAM only
and reports `RamOnlyDegraded` with the reason; the replay may then be shorter
than configured.

## Integration (`clipper-recorder`)

One worker thread owns everything; `windows_worker.rs` holds
`replay: Option<ReplaySession>` and a lazily spawned `Exporter` (queue 3) that
outlives replay sessions, so `StopReplay` never cancels a running export.

`ReplaySession` (`replay_session.rs`) owns capture, `PacketEncoder`,
`RecordingAudio`, the `ReplayBuffer` (created when the first packet defines
the layout) and one GPU texture with a copy of the last captured frame.
Teardown order: capture, encoder, audio, buffer, D3D device.

Per service iteration (wake wait at most 30 ms, never more than ~250 ms of
work): at most 4 captured frames -> `FramePacer` (audio origin, same as manual
recordings) -> constant frame rate grid -> `PacketEncoder::submit`; then, when
WGC was silent (static scene), the last frame is re-submitted up to real time
minus one frame interval; then `RecordingAudio::pump` through the `AudioSink`
trait (shared with manual recording); then `poll` -> `ReplayBuffer::push`.
Track labels: `Mix`, then `Desktop`/`Microphone` with separate tracks.

Constant frame rate: the video timeline is a grid of slots of one frame
interval (`SlotTimeline`). A real frame goes to slot `round(sample_time / I)`
and is submitted at `slot * I`; a frame whose slot is already taken is
discarded (counted); every empty slot before it is filled with a repeat of the
previous frame (GPU `CopySubresourceRegion` copy only, no readback; at most 16
repeats per iteration, the real frame stays pending and the rest carries
over). Output is therefore exactly the configured fps (59.9-60.1 measured by
`probe_clip`), also for 17-59 fps content.

First frame: until the first video frame exists, audio is read and discarded
(rings never back up), the status warning is "waiting for the first video
frame", the audio/video clock drift check is off, and after 30 s without a
frame the session stops with `ReplayStopped{"the capture source delivered no
frames"}`. The audio timeline then starts at the first video sample.

Audio resilience (replay only; manual recording stays strict): dropped capture
frames and frames that miss the mixing window are counted, the mixer renders
silence for the gap and the status warning reads "audio glitch: N frames
lost". Loss or failure of a source (device removed) still stops the replay. A
`ReplayBuffer::push` `Invalid` error (dts not increasing) drops that packet,
counts it and warns. Pacer skips, late discards, audio glitches and invalid
packets are logged to stderr when the session stops.

File I/O on the worker thread: `ReplayBuffer::new` creates the SSD session
directory once. Clip folder creation and the on-disk name check run on the
export thread (`Exporter::try_submit_with` prepare hook); the worker only
reserves the name in memory (`ClipNames`). Once per replay start the export
thread removes `Clipper_Replay_*.mp4.partial` files older than 1 h in the clip
folder (no other file). Spilling runs on the spill thread, export on the
export thread.

### Event flows

* `StartReplay` ok: `ReplayStarted{encoder}`, then `ReplayStatus` every
  second (`try_send`). Failure: `ReplayStopped{error: Some(reason)}`
  (lowercase fragment). Already running: `ReplayStarted` again. While a manual
  recording or audio test runs: `ReplayStopped{"stop the current recording first"}`.
* `StopReplay` (running or not): `ReplayStopped{None}`.
* Runtime failure (capture closed, encoder, lost audio source, buffer): the
  footage is kept first. If the buffer holds a complete keyframe-aligned span,
  an automatic save of the configured duration is queued exactly like a normal
  save (`ReplaySaving`, later `ReplaySaved`/`ReplaySaveFailed`), then the session is torn
  down and `ReplayStopped{Some(reason)}` is sent. Nothing is saved when the
  buffer is not ready yet. A user `StopReplay` or app shutdown never
  auto-saves.
* `SetReplayDuration`: ignored when idle; live `ReplayBuffer::set_duration`.
* `SaveReplay`: idle -> `ReplaySaveFailed{REPLAY_NOT_RUNNING}`; buffer not
  ready -> "The replay buffer is still filling; try again in a moment.";
  exporter queue full -> "Already saving clips; try again in a moment.";
  accepted -> `ReplaySaving`; the export thread sends `ReplaySaved{path,
  duration}` only after `export_clip` returned `Ok` (file renamed), otherwise
  `ReplaySaveFailed`. Names are `Clipper_Replay_YYYY-MM-DD_HH-MM-SS[_n].mp4`;
  names of queued/running exports are reserved (`ClipNames`).
* `StartRecording` / `StartAudioTest` while replay runs: `RecordingFailed` /
  `AudioTestFinished{error}` with "Stop instant replay first.".

Save jobs: every request gets an id (`save #N` in the logs) and an in-memory
name reservation; the same or an overlapping interval saved twice gives two
files. Lifecycle: `ReplaySaving` is sent before the job is submitted (so it
always precedes the result), then queued -> `export started` -> `ReplaySaved`
(only after the rename) or `ReplaySaveFailed`. Up to 4 jobs (1 running + 3
queued); a further request fails at once with "Already saving clips". Each job
owns its snapshot (`Arc` GOPs / segments), so eviction and `StopReplay` never
take its input away.

Completion delivery: the export thread hands results to a small forwarder
thread (`Completions`, unbounded channel, at most one pending event per
accepted job) that waits for room in the UI queue (`try_send` plus a 10 ms
sleep). A slow or stalled UI therefore never holds up the export thread or the
saves queued behind it.

Stop policy: `StopReplay` rejects nothing that was accepted. It releases
capture, encoder, audio and buffer synchronously (tens of ms), sends
`ReplayStopped{None}`, and lets accepted exports finish from their snapshots
on the export thread; their results arrive later. Stop never waits for disk
I/O.

UI: the recorder event loop runs on the main window. gpui refuses a window
update while that window is mid-update (a nested Win32 message loop can poll
the task then); the loop retries a frame later instead of exiting. Exiting
there used to stop event draining for good: completions piled up, the worker
blocked emitting `ReplayStopped`, so saves stalled and the toggle stayed on
"Stopping…".

Shutdown: the worker first lets already accepted saves finish
(`Exporter::finish_queued`, at most 10 s; the UI keeps draining events
meanwhile), then sets `shutting_down` (pending completion events are given up
if the queue is still full), drops the exporter, which fails whatever is still
queued with `Shutdown`, and then the forwarder. `Recorder::drop` also drains
events while joining.

Logs (stderr, `clipuh replay:` prefix): save accepted/rejected with interval,
export started, saved/failed, stop requested and release time, shutdown drain.

Stress check: `cargo run --release -p clipper-recorder --example
replay_stress` (back-to-back saves, stop right after a save, restart, stop with
queued saves, write failure then recovery).

Hardware check: `cargo run --release -p clipper-recorder --example
replay_smoke -- run=45 duration=30` then `probe_clip` on every clip.
