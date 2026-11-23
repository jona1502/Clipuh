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

Audio stream 0 is the playback mix. With separate tracks enabled, one extra
stream per started source follows, in source order. These are alternatives for
editing: do not sum the mix and its isolated tracks together.

## Sources, routing and overlap

Sources come from `[[audio.sources]]`; only enabled ones reach the recorder
(the settings' `recording_options` filters the rest, so a disabled source is
never opened, metered or tracked). Kinds: Game (the executable behind the
captured window), Application (`Discord.exe`...), Desktop (render endpoint
loopback), Microphone. `clipper_audio::routing::plan` makes tracks disjoint so
the mix never holds a signal twice:

- Application/Game: process loopback INCLUDE of that process tree. The same
  executable twice (any case, Game + Application) is recorded once.
- Desktop with no application source: full endpoint mix, labelled "Desktop
  (full mix)".
- Desktop with exactly one application: process loopback EXCLUDE of that
  application ("Desktop (without X)"). It is system-wide, so the chosen output
  device is ignored for this stream.
- Desktop with two or more applications: Windows process loopback can exclude
  only one tree, so the remainder is composed instead ("Desktop (without X,
  Y)"): one process loopback INCLUDE stream per other process that owns an
  audio session, re-listed once a second and summed in the mixer
  (`clipper_audio::desktop`). Signals are never subtracted. Audio that cannot
  be separated (the Windows system sounds session; a process whose tree
  contains a separated application) is left out of the desktop track and
  listed in the source's status note instead of being recorded twice. See
  `docs/quality/audio-routing-live-sources.md`.
- Microphones: one track per distinct device. A device chosen explicitly that
  is also the Windows default counts as the default when a source follows the
  default, and is reported as a duplicate.
- Known residue: an application started by another selected application (an
  Application source on a launcher that parents the game) is inside both
  trees. While the excluded application is not running, the desktop track is
  the full mix and switches to exclusion within ~250 ms plus activation after
  it starts; its audio can be in both tracks for that moment.

Every requested source gets an `AudioSources` status: `Capturing`,
`Waiting(reason)`, `Starting`, `Failed(message)` or `Skipped(reason)`.
`Skipped` carries the routing warning (unsupported Windows, duplicate, whole
display captured for Game, more than four sources).

While instant replay runs, source changes (add, remove, enable, disable,
another device or application) are applied live through
`RecorderCommand::SetAudioSources`: only the affected capture streams start or
stop; video, encoder and replay buffer continue. The encoder's audio tracks are
fixed per session, so a source added live is recorded in the mix and gets its
own isolated track with the next session. A manual recording keeps its sources
until it ends.

## Capability fallback

Process loopback needs Windows 10 build 20348+ / Windows 11.
`clipper_audio::windows::process_loopback_supported()` (or `capabilities()`)
tells the UI up front. Without it, Game/Application sources are `Skipped`
("Per-app audio needs Windows 11 ...") and Desktop becomes the plain full mix,
so tracks are never silently missing and never collapsed: the encoder's track
count and labels are derived from the planned tracks (`AudioPlan`), and the
capture engine refuses a hand-made plan that needs process loopback.
`clipper_recorder::planned_audio_tracks(&options)` returns the track count a
session is expected to encode on this machine (a Game source is assumed to
resolve), for storage estimates; `RecordingAudioOptions::encoded_track_count()`
stays the platform-neutral upper bound.

## Recording and replay do not overlap

The recorder worker is single-threaded and refuses `StartRecording`,
`StartAudioTest` and `StartReplay` while any of the other two is active
("Stop instant replay first." / "stop the current recording first"), so at most
one `RecordingAudio` (one set of capture threads) exists. The replay buffer's
audio also serves clips saved from it; a manual recording is not possible
while the replay runs.

## Source loss mid-session

- Application exits: the stream's exit handle fires, the thread reopens, finds
  no process and reports `SourceLost`; the source shows `Waiting("X is not
  running")`, its track stays in the file as silence, and capture resumes by
  itself when the executable runs again (it is re-resolved by name). Other
  sources continue. Neither recording nor replay fails or hangs: waits for
  activation are interruptible by stop.
- Desktop remainder (EXCLUDE) of an exiting application: reopens as the full
  mix; a loss there is routine and never fails the session.
- Device unplugged (microphone, explicit desktop device): a manual recording
  fails (strict, no half-recorded file reported as good); a replay keeps
  running with silence and `Waiting`, and the source returns on its own. A
  stream that cannot be opened at all (`Failed`) fails either mode. The check
  is per track, so a running application track cannot mask a lost microphone.
- A device or full-mix track that delivered no sample at all fails a finished
  clip; application tracks and desktop remainders may legitimately be silent.

## Levels, gain and limiter

Meters emit at most 10 times per second and only when a peak changed since the
last one sent. Gain is 0 to 200 %, clamped in the settings, the recorder and
the mixer, ramped over one 480-frame block. Each isolated track and the sum go
through a soft limiter (identity up to 0.9, then approaching full scale) so
gain above 100 % or several loud sources cannot clip hard or wrap.

Process loopback streams are requested at 48 kHz stereo f32 with shared-mode
conversion like every other stream; packets carry QPC positions and use the
same placement, drift and jitter rules as the device streams.

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

The global clipping hotkey is registered and dispatches SaveReplay. No replay
buffer or successful replay-save pipeline is implemented; SaveReplay reports
"Replay buffer is not running." until that engine is available.
Manual-recording audio does not prove Replay audio. The command/event and
bounded audio interfaces prepare integration, but Test E remains unavailable.
Review the remaining hardware acceptance gates before merging.

A short debug-build resource sample of the **entire recorder** at 1280×720,
30 fps, desktop+microphone with three AAC streams measured 0.953125 CPU seconds
in 5.014 seconds (19.0% of one logical core, about 0.79% normalized across the
24 logical processors), 147.9 MiB private bytes and 108.7 MiB working set.
The run used AMD hardware H.264 and completed a 10-second recording with no
reported encoder drops. This is one warm debug sample, not an isolated audio
measurement or a release performance guarantee; GPU memory was not measured.
