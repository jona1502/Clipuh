# Audio routing: desktop audio with separated apps, live source changes

Two reported bugs, fixed together because both live in the routing and the
capture lifecycle of the audio pipeline.

- **Bug 1**: Desktop Audio became silent when Spotify and Discord were
  recorded as separate sources.
- **Bug 2**: an audio source added while Instant Replay runs was not
  recorded until replay was restarted.

"Confirmed" below means verified in the code or by a test. "Assumption"
means not verifiable without real Windows hardware.

## 1. Existing architecture (before this change)

```
settings [[audio.sources]]
  -> clipper_storage AudioSettings::recording_options      (enabled sources)
  -> clipper_recorder AudioPlan::build                      (recording_audio.rs)
       resolve_selection -> alias_default_devices -> routing::plan
  -> clipper_audio AudioCapture::start_plan                (windows/engine.rs)
       one StreamThread per planned track (windows/stream.rs)
       endpoint loopback / process loopback INCLUDE|EXCLUDE / microphone
       -> bounded SPSC ring per track (ring.rs)
  -> RecordingAudio::ingest -> TimelineMixer::push          (mixer.rs, QPC timeline)
  -> TimelineMixer::render_until -> AudioSink::submit_audio
       stream 0 = mix, streams 1.. = isolated tracks (when separate tracks on)
  -> PacketEncoder (replay) / HardwareEncoder (manual recording)
```

- Source identity: the settings id (`u32`) of each mixer row. Applications
  are identified by executable name (`ProcessTarget::Executable`), re-resolved
  to a root pid after restarts (`process::find_process`).
- The encoder's audio track count is decided before the encoder is created
  (`AudioPlan::encoder_tracks`, `PacketEncoder::create_with_labels`).
- Statuses travel to the UI as `RecorderEvent::AudioSources`.

## 2. Root cause of Bug 1 (confirmed)

`clipper_audio::routing::plan` (crates/clipper-audio/src/routing.rs) planned
the desktop source as

- no app separated: endpoint loopback (full mix),
- one app separated: process loopback `EXCLUDE_TARGET_PROCESS_TREE` of it,
- **two or more apps separated: no track at all**, warning
  `DesktopRemainderUnavailable` ("Desktop audio is off ...").

So with Desktop + Spotify + Discord, the desktop source was never captured
and everything else (the game) was lost. The UI said so in small print
("Skipped while 2+ apps are recorded").

The underlying limit is real: `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS` has a
single `TargetProcessId`, so one process loopback client can exclude exactly
one process tree. There is no Windows API to exclude an arbitrary list of
processes from a loopback stream without a driver or virtual device.

## 3. Root cause of Bug 2 (confirmed)

- The recorder had no command for source changes. `RecorderCommand` had
  `SetAudioGain` and `SetAudioNoiseGate` only.
- `RecordingAudio` started a fixed set of capture threads and a fixed mixer
  (`TimelineMixer::new(origin, gains, separate)`), indexed by position.
- The UI (crates/clipper-ui/src/views/audio.rs) persisted the change and
  showed "Applies to the next recording or replay." (`topology_pending`).
- The module doc of recording_audio.rs stated it: "Enabling, disabling,
  adding or removing sources therefore applies to the NEXT session".

## 4. Supported exclusion semantics after the fix

| Selection | Desktop track | Content |
| --- | --- | --- |
| Desktop only | endpoint loopback of the chosen device | full mix |
| Desktop + 1 app (or Game) | process loopback EXCLUDE of that tree (system-wide) | everything except that app |
| Desktop + 2+ apps | **composed**: one process loopback INCLUDE stream per other process with an audio session (system-wide), summed | everything except those apps, minus unseparable audio (below) |
| No process loopback (older Windows) | full mix, apps skipped with a warning | unchanged |

Composed desktop (`StreamPlan::DesktopComposite`, crates/clipper-audio/src/desktop.rs,
crates/clipper-audio/src/windows/sessions.rs, `CompositeThread` in windows/engine.rs):

- Once a second the composite thread lists the audio sessions of every active
  render endpoint (`IAudioSessionManager2::GetSessionEnumerator`,
  `IAudioSessionControl2::GetProcessId`) and the process table (Toolhelp).
- `desktop::compose` picks the processes to include:
  - skipped: any process inside an excluded tree (itself or an ancestor
    matches an excluded executable, case-insensitive, or pid);
  - **unseparable**: a process whose own tree contains an excluded process
    (including it would record the excluded app twice), and the Windows
    system sounds session (pid 0, no capturable process);
  - a process under another included process is covered by that tree;
  - at most `MAX_PARTS` (16) streams.
- Each included process gets its own capture thread and ring; the mixer sums
  a source's inputs before its gate and gain (`TimelineMixer::push_input`).
  Nothing is subtracted.
- Unseparable applications are shown on the Desktop row
  ("Not in desktop audio (would duplicate a separated app): ...") and logged.

Why not the alternatives:

- Exclude one app and keep the other: the other app is then recorded in two
  tracks and twice in the mix. Rejected (silent duplication).
- Subtract the app streams from the full mix: independent loopback copies are
  not sample-aligned, Windows applies per-stream processing and the apps may
  render to other endpoints; residue or doubled audio is unavoidable. Rejected.
- Driver / virtual device: out of scope by requirement.

### Remaining limitations (honest)

- Audio that is not in any separable process tree is missing from the
  composed desktop track (system sounds; a launcher that parented Spotify or
  Discord, often `explorer.exe` when the app was started from the shell and
  explorer itself plays sounds). It is reported, not recorded twice.
- A process that opens its first audio session joins the desktop track
  within about 1 s plus activation time (polling interval of the session
  list; `COMPOSE_INTERVAL_MS`). Sessions are listed even while silent, so an
  app that already has a session is captured before it plays.
- Assumption to verify on hardware: Windows resolves "process tree" for
  process loopback from the same parent pids Toolhelp reports. If Windows
  treated a process differently, a part could contain an excluded app.
- Process loopback is system-wide: the composed (and the single-exclusion)
  desktop track ignores the chosen output device. Unchanged behavior for the
  single-exclusion case.
- Costs: one extra WASAPI process-loopback client and thread per included
  process (typically a handful), 768 KiB mixer ring + 500 ms capture ring per
  part, and one session/process enumeration per second. Only paid when two or
  more apps are separated; the other routing shapes are unchanged.

## 5. Live audio source changes (Bug 2)

Flow:

```
UI row change (toggle, add, remove, device/application choice)
  -> persist settings + RecorderCommand::SetAudioSources(recording_options)
  -> worker: only a running replay applies it (ReplaySession::reconfigure_audio)
  -> RecordingAudio::reconfigure
       AudioPlan::replan (same captured window, same separate-tracks setting)
       LiveRouting::apply  (crates/clipper-audio/src/live.rs)
         routing::diff(running, next): unchanged tracks keep running
         removed tracks: AudioCapture::remove_track (signal only) + mixer retire
         new tracks: device check, AudioCapture::add_track, mixer add_source
  -> statuses (Starting -> Capturing / Waiting / Failed) via AudioSources
```

- Desired configuration is the persisted settings; the applied state is the
  per-source status the recorder reports. A source that cannot start is
  `Failed(reason)`, never shown as capturing (TEST J).
- Video capture, the hardware encoder and the replay buffer are never
  touched; the replay keeps its history.
- A failure of a live-added device marks only that source `Failed`
  (`TrackRt::live_added`); devices present at session start keep their
  existing policy.
- Manual recordings and the audio test keep their sources until they end
  (the UI keeps showing "Applies to the next recording or replay." there).
- A replay that started with no audio source has no audio tracks; its
  sources report "This session started without audio; restart it to record
  audio." instead of pretending to capture.

### Worker lifecycle and concurrency

- Commands are processed in order on the single recorder thread, so the last
  `SetAudioSources` always wins; there is no pending asynchronous update that
  could overwrite a newer one (TEST K).
- `AudioCapture::add_track` only spawns a thread; WASAPI activation happens
  on that thread. `remove_track` only signals stop; the thread is joined once
  it has exited (`maintain`, `StreamThread::is_finished`), so the recorder
  never waits on a WASAPI shutdown and holds no lock while streams start.
- Track indexes are unique per capture and never reused; events and packets
  of removed tracks are ignored (generation-safe).
- After `stop`/`stop_and_drain`, `add_track` refuses (TEST L/U). Dropping the
  capture joins every worker, including retiring ones and composite parts.
- All channels are bounded (part hand-over channel `2 * MAX_PARTS`, events
  `event_capacity`, rings `ring_ms`).

### Mixer, replay continuity and timestamps

- The mixer is addressed by QPC position against the session origin. A new
  source contributes from its first packet's QPC position; nothing earlier is
  synthesized; the mix timeline is not reset or shifted (TEST P/Q/T).
- A removed source's already captured tail is rendered, then the source is
  dropped (`retire_source`); a finished composite part likewise
  (`retire_input`).
- Gain ramps, noise gates, dB mapping, the soft limiter and the Combined/Mix
  stream are unchanged; `TimelineMixer::new` keeps its exact behavior (all
  previous mixer and A/V-sync tests pass unchanged).
- Encoder topology is fixed per session (an MP4/replay packet stream cannot
  gain tracks midway). With separate tracks, each isolated output stays owned
  by the source it was created for: a removed source leaves it silent, the
  same source re-enabled gets it back, and a newly added source is recorded
  in the mix only, with the status note "Recorded in the mix; its own track
  starts with the next session."
- Replacing one source's stream (other device, other app, desktop exclusion
  changed) stops the old stream before the new one starts: a short gap on
  that source only, never doubled audio.

## 6. Tests added

macOS-runnable (`cargo test -p clipper-audio -p clipper-recorder -p clipper-core`):

| Test | Scenario |
| --- | --- |
| `routing::desktop_and_two_apps_keeps_desktop_as_composite` | Bug 1 regression, TEST C/D |
| `routing::unseparated_apps_stay_in_the_desktop_remainder` | Desktop + 2 apps + other sources |
| `routing::source_ids_never_collide_across_many_sources`, `every_combination_is_disjoint` | TEST O, no duplicate routing |
| `routing::diff_*` (3) | minimal live diff, TEST H/I |
| `desktop::game_stays_in_desktop_while_two_apps_are_separated` | Bug 1: game kept, Spotify/Discord excluded |
| `desktop::*` (10 more) | children, nesting, unseparable ancestors, system sounds, pid targets, restart/pid reuse (TEST N), cycles, part bound, no sessions |
| `live::add_source_during_session_starts_only_that_track` | Bug 2 regression, TEST E |
| `live::remove_source_during_session_stops_only_that_track` | TEST F |
| `live::disable_and_re_enable_never_duplicates` | TEST G |
| `live::changed_process_selection_replaces_the_track` | TEST H |
| `live::changed_exclusion_reconfigures_only_the_desktop` | TEST I |
| `live::activation_failure_is_reported_and_isolated` | TEST J, V |
| `live::rapid_updates_converge_on_the_latest` | TEST K |
| `live::nothing_starts_after_shutdown` | TEST L, U |
| `live::many_sources_have_distinct_indexes` | TEST O |
| `live::added_source_joins_the_timeline_without_shifting_it` | TEST P, Q, R, T |
| `live::gains_survive_live_changes` | TEST S |
| `live::isolated_outputs_stay_owned_by_their_sources` | separate-track topology |
| `live::composed_desktop_keeps_rendering_next_to_two_apps` | TEST A-D at mixer level |

Windows-only unit tests (run by CI on `windows-latest`, no audio hardware):

| Test | Scenario |
| --- | --- |
| `recording_audio::every_source_has_a_track_or_a_reason` (updated) | Bug 1 regression in the session plan |
| `recording_audio::states_report_outputs_refusals_and_notes` | applied-state reporting, TEST J |
| `recording_audio::failed_live_added_device_is_failed_not_fatal` | TEST J, M |
| `recording_audio::worker_loss_after_start_leaves_capturing` | TEST M |
| `views::audio::desktop_note_names_every_separated_app`, `running_note_is_shown_only_while_capturing` | UI text |

Test A (desktop only) and B (desktop + one app) are covered by the existing
routing tests (`desktop_alone_is_an_honestly_labelled_full_mix`,
`desktop_and_one_app_use_exclusion`), which still pass.

## 7. Results

- macOS: `cargo fmt --all -- --check` clean; `cargo test -p clipper-audio
  -p clipper-recorder -p clipper-core` all passing (clipper-audio 86 unit
  tests).
- macOS cross-check: `cargo clippy -p clipper-audio -p clipper-recorder
  --target x86_64-pc-windows-msvc --all-targets -- -D warnings` clean
  (type-checks the WASAPI code; nothing is executed). `clipper-ui` cannot be
  cross-checked on macOS (a C dependency of GPUI needs the Windows SDK).
- Windows CI: see the pull request checks.
- Nothing here proves real capture of Spotify, Discord or a game. The plan
  below is required on Windows hardware.

## 8. Manual Windows validation plan (not yet run)

Record for each: expected routing, actual routing, recorded tracks (open the
saved clip in the editor), replay restarted (should be no), replay history
kept, A/V sync, `clipuh` log lines (`audio sources changed live`, `audio
source N: Not in desktop audio ...`), pass/fail.

| # | Scenario | Expected |
| --- | --- | --- |
| 1 | Desktop only | full mix |
| 2 | Desktop + Spotify | Desktop (without Spotify.exe) + Spotify |
| 3 | Desktop + Discord | Desktop (without Discord.exe) + Discord, incl. voice |
| 4 | Desktop + Spotify + Discord | Desktop (without Spotify.exe, Discord.exe) + both apps |
| 5 | as 4, both apps playing | no Spotify/Discord in the Desktop track |
| 6 | as 4 with a game playing | game audible in the Desktop track |
| 7 | add Spotify while replay runs | Spotify `Capturing` within ~1 s, no restart |
| 8 | add Discord while replay runs | as 7; desktop switches to composed |
| 9 | remove Spotify while replay runs | Spotify gone, desktop keeps game |
| 10 | disable and re-enable Desktop during replay | gap only while off |
| 11 | change Desktop exclusion during replay | only desktop restarts |
| 12 | close and restart Spotify | Spotify `Waiting` then `Capturing`; never in Desktop |
| 13 | close and restart Discord | as 12 |
| 14 | stop replay right after adding a source | clean stop, no orphan threads |
| 15 | save replay before and after live changes | both valid, history kept |
| 16 | levels, gain, mute, solo, gate after live changes | unchanged behavior |
| 17 | separate tracks on, add source live | in Mix, note says own track next session |
| 18 | no duplicate or missing game audio | game exactly once in the mix |
| 19 | Spotify started from Explorer, Explorer plays a sound | Desktop note lists explorer.exe |
| 20 | Windows notification sound with 2 apps separated | missing from Desktop, note lists system sounds |
