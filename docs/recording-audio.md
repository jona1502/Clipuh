# Recording audio integration

## Cause and fix

The existing audio engine was standalone: `clipper-recorder` never started it,
`RecordingOptions` did not contain audio settings, and the MP4 sink writer only
configured a video stream. Saved recordings therefore could not contain audio.

The recorder now starts WASAPI after the expensive GPU/codec initialization,
waits for every requested source to start, and drains its bounded rings when
packets wake the recorder. It mixes the selected render endpoint and microphone
on a shared QPC timeline, then feeds 48 kHz stereo PCM16 to the system AAC-LC
encoder (192 kbps) inside the same Media Foundation MP4 sink writer as video.

Audio stream 0 is the playback mix. With isolated editing tracks enabled,
additional streams contain desktop and microphone individually, in that order
when both are enabled. These are alternatives for editing: do not sum the mix
and its isolated tracks together. Game and Discord are already included once
in endpoint playback; separate process routing is not integrated into recording.
Existing `audio.game` and `audio.discord` preferences do not select processes.
The UI explains this limitation rather than exposing functional process toggles.

WGC can stop delivering frames on a static scene. The worker sends video stream
ticks for those real gaps so synchronous muxer throttling does not block the
continuous audio stream. Video processing is limited to four frames per worker
iteration so audio and commands cannot be starved by continuous WGC delivery.

## Devices, lifecycle and memory

Settings â†’ Audio selects capture and render endpoints independently, including
Windows default. Stable endpoint IDs, enable flags, gains and isolated-track
preferences persist in per-user TOML. Missing explicit devices fail startup;
there is no silent fallback to a different microphone. Device refresh happens
on menu opening or the refresh action, off the UI thread. Defaults follow OS
endpoint changes through the existing WASAPI notification implementation.

Live peaks update at most 10 times per second while recording/testing. Each
source has an explicit 30-second PCM WAV diagnostic with an early stop action.
Microphone capture only runs during an explicitly started test/recording.
Settings changes apply to the next recording and are locked during capture.
Background configuration writes are ordered by revision to prevent an older
snapshot from overwriting a newer selection.

WASAPI remains event-driven. Packet rings default to 500 ms; the mixer allocates
one second of stereo samples and position tags per source (about 768 kB/source),
plus fixed 480-frame working blocks. No buffers grow with recording duration.
These bounds do not include Media Foundation, GPU or capture allocations.
The mixer retains 80 ms of jitter headroom. QPC timestamps place every packet
on the common 48 kHz grid; sub-2-ms jitter preserves sample continuity and larger
offsets correct clock drift by pad/skip. Adaptive fractional drift resampling is
not implemented; long-duration quality/drift testing remains necessary.
WASAPI shared-mode conversion handles differing device sample rates.

An enabled source must deliver actual packets. Audio source loss, ring overflow,
late packets or finalization errors prevent a successful clip result. Silence
reported by a real source is valid; an empty source is not faked with a dummy
track. All streams are drained before finalizing the container.

A real 30-second microphone diagnostic also finalized correctly (30.050 seconds,
observed peak 0.02013). This confirms packet flow, metering and WAV finalization;
it does not establish speech intelligibility.

## Validation and remaining gates

Automated tests cover timestamp placement/gaps, isolated versus mixed sources,
bounded buffering, config round trips, invalid gains/IDs and unavailable devices.
The ignored hardware AAC test writes video plus three AAC tracks and decodes
all tracks to verify sample rate, channels, non-silent samples and monotonic PTS.
Run it explicitly with:

```text
cargo test -p clipper-encoder muxes_real_aac -- --ignored --nocapture
cargo test -p clipper-recorder microphone_test_finalizes -- --ignored --nocapture
cargo run -p clipper-recorder --example record_smoke -- 1280x720 30 balanced 30 1
cargo run -p clipper-encoder --example probe_audio -- <clip.mp4>
```

`probe_audio` uses Media Foundation as the stream-inspection/decoding equivalent
when FFprobe is unavailable. Actual checks on this Windows/AMD system included
one 30-second desktop+microphone recording: 857 video frames, zero video encoder
drops; all three audio tracks decoded as 48 kHz stereo AAC, approximately
30.165 seconds, with nonzero signal. This is packet/container evidence, not a
subjective hearing test or a long-duration synchronization benchmark.

Open gates: listening tests for clarity/dropouts, explicit A/V marker sync tests,
long-duration drift, physical device removal/switching under load, release CPU/
RAM benchmarking, and visual interaction tests for the GPUI controls.

Instant Replay has no implemented buffer/hotkey/save pipeline in this repository.
Manual-recording audio does not prove Replay audio. The command/event and
bounded audio interfaces prepare integration, but Test E remains unavailable.
Keep the PR draft until the remaining hardware acceptance gates are reviewed.

A short debug-build resource sample of the **entire recorder** at 1280×720,
30 fps, desktop+microphone with three AAC streams measured 0.953125 CPU seconds
in 5.014 seconds (19.0% of one logical core, about 0.79% normalized across the
24 logical processors), 147.9 MiB private bytes and 108.7 MiB working set.
The run used AMD hardware H.264 and completed a 10-second recording with no
reported encoder drops. This is one warm debug sample, not an isolated audio
measurement or a release performance guarantee; GPU memory was not measured.
