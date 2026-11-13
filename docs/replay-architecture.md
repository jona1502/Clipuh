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
recordings) -> `PacketEncoder::submit`; then, when WGC was silent (static
scene), the last frame is re-submitted at the next pacer time up to real time
minus 1.5 frame intervals (max 16 per iteration, GPU `CopySubresourceRegion`
only, no readback); then `RecordingAudio::pump` through the `AudioSink` trait
(shared with manual recording); then `poll` -> `ReplayBuffer::push`. Track
labels: `Mix`, then `Desktop`/`Microphone` with separate tracks.

File I/O on the worker thread: `ReplayBuffer::new` creates the SSD session
directory once, and a save does `create_dir_all` plus `exists()` checks to
choose the clip name. Spilling runs on the spill thread, export on the export
thread.

### Event flows

* `StartReplay` ok: `ReplayStarted{encoder}`, then `ReplayStatus` every
  second (`try_send`). Failure: `ReplayStopped{error: Some(reason)}`
  (lowercase fragment). Already running: `ReplayStarted` again. While a manual
  recording or audio test runs: `ReplayStopped{"stop the current recording first"}`.
* `StopReplay` (running or not): `ReplayStopped{None}`.
* Runtime failure (capture closed, encoder, audio, buffer): the session is torn
  down, `ReplayStopped{Some(reason)}`.
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

Shutdown: the export thread sends completion events with `send_blocking`
(it may wait for the UI). Before the worker joins the exporter it sets
`shutting_down`, after which completions use `try_send`, so the join cannot hang
on a full queue. `Recorder::drop` also drains events while joining, and the
`RecorderHandle` keeps a receiver alive, so `send_blocking` never fails with a
closed channel while the queue is full without a drain.

Hardware check: `cargo run --release -p clipper-recorder --example
replay_smoke -- run=45 duration=30` then `probe_clip` on every clip.
