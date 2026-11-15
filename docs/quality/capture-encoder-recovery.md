# Capture and encoder recovery

Status: audit of `main` at `4ab4130` plus the changes in this branch. The
platform-neutral parts are covered by deterministic tests on macOS and the
Windows runners. **No source loss, encoder failure or GPU device loss was
reproduced on real Windows hardware for this report**; see the manual plan.

## Lifecycle (existing)

One recorder worker thread owns every capture, encoder and audio session
(`crates/clipper-recorder/src/windows_worker.rs`). Replay, manual recording and
the audio test are mutually exclusive. The UI only sends commands and renders
events.

```text
StartReplay ─ ReplaySession::start (replay_session.rs:137)
   D3DContext::new (new device per session) ─ CaptureSession::start (WGC item,
   frame pool, Closed handler) ─ PacketEncoder (HW MFT on that device) ─
   RecordingAudio (WASAPI threads) ─ ReplayStarted
running: service_replay (windows_worker.rs:617) every ≤ 30 ms
   pump: frames ─ slots ─ encoder, static-scene repeats, audio, packets ─ buffer
   source Closed flag checked after each pump
failure: fail_replay (windows_worker.rs:652)
   auto-save of this session's buffer ─ stop_replay: drop capture, encoder,
   audio, buffer, device (in that order) ─ ReplayStopped{error}
StopReplay / Shutdown: stop_replay; accepted saves finish from their snapshots
```

Manual recording uses the same capture and device ownership with a Media
Foundation sink writer. Source closed → normal stop (`windows_worker.rs:919`).
Encoder/sink errors → `RecordingFailed`, partial file removed or kept by the
publish rules. Audio/video clock gap → failure (`windows_worker.rs:120`).

### Confirmed properties (kept)

- **No invalid resource is reused.** Every replay start creates its own
  `D3DContext` (`crates/clipper-platform/src/windows/mod.rs:74`), capture item,
  frame pool, encoder and buffer. After any failure the whole session is
  dropped, so a removed device dies with its session.
- **No mixing of encoder sessions in a buffer.** The `ReplayBuffer` belongs to
  one `ReplaySession` and is created from that session's first packet layout.
  A restarted session starts an empty buffer. Saves hold `Arc` snapshots, so a
  teardown never invalidates a running export.
- **No orphaned audio.** `RecordingAudio` is a field of the session; dropping
  the session joins the WASAPI threads (`AudioCapture`'s `Drop`). The origin of
  a new session is taken from its own audio start.
- **Teardown is best effort and does not depend on a healthy GPU.** Encoder
  `Drop` sends flush/end-streaming and ignores errors; WGC `Drop` removes
  handlers and closes the session/pool.
- **The UI cannot stay "running" after a failure.** Every failure path sends
  `ReplayStopped{Some(error)}`; the UI resets on it.

## Defects and gaps found

| # | Finding | Effect | Change | Test |
| --- | --- | --- | --- | --- |
| 1 | An encoder that silently stops producing output (no error, inputs only dropped) was detected only indirectly, by the audio/video clock-gap check, and **not at all when no audio source is enabled**. | Replay showed "active" while the buffer stopped growing; a save produced old footage. | `OutputWatchdog`: after video started, video output must follow within 5 s, else `EncoderFailure`. | `logic::tests::watchdog_detects_a_silent_encoder` (fails when the watchdog never fires) |
| 2 | All failures were plain strings: a GPU reset showed a raw HRESULT text ("encoder failed: …"), and nothing distinguished recoverable from permanent failures. | No basis for recovery; unclear messages. | `Failure { kind, detail, code }` with `classify_hresult` (DXGI device removed/hung/reset/driver error, D3DDDI device removed → `DeviceLost`; RO_E_CLOSED, invalid window → `SourceLost`; unsupported formats → `Configuration`). UI text explains GPU and encoder failures and keeps the code; the log keeps kind, detail and code. | `logic::tests::hresults_are_classified_by_meaning_not_by_failure` |
| 3 | A replay that failed because of a driver reset (game crash, driver update) stayed off until the user noticed. | Lost replay coverage. | Bounded automatic restart for `DeviceLost` (3 attempts after 2/5/10 s) and `EncoderFailure` (1 attempt after 2 s). Never for source loss, configuration, audio or other failures. | `logic::tests::device_loss_is_restarted_once_after_a_delay`, `retries_are_bounded`, `source_loss_and_configuration_errors_need_the_user` |

## Failure taxonomy

| Kind | Typical cause | Detected by | Automatic restart |
| --- | --- | --- | --- |
| `SourceLost` | Window closed, app crashed, monitor unplugged | WGC `Closed` event (`capture/windows/mod.rs:365`), `SourceUnavailable`, RO_E_CLOSED / invalid window handle | no; the user picks a source (never switched silently) |
| `DeviceLost` | Driver reset (TDR), driver update, GPU removed | DXGI_ERROR_DEVICE_REMOVED/HUNG/RESET/DRIVER_INTERNAL_ERROR, D3DDDIERR_DEVICEREMOVED from capture, encoder or device creation | yes, up to 3 attempts, each a complete new session |
| `EncoderFailure` | MFT processing error, no output for 5 s | Other Windows errors from the encoder, watchdog | yes, 1 attempt |
| `Configuration` | No hardware encoder, unsupported format/resolution, WGC unsupported | `NoHardwareEncoder`, `UnsupportedFormat`, `Unsupported`, DXGI_ERROR_UNSUPPORTED, MF_E_INVALIDMEDIATYPE | no |
| `Other` | Audio device lost, buffer/disk error, clock gap, no frames for 30 s | everything else | no |
| User stop / shutdown | — | commands | never; cancels any pending restart |

Unknown HRESULTs (E_FAIL, E_OUTOFMEMORY, …) are never treated as transient:
from capture they are `Other`, from the encoder `EncoderFailure` (one retry).

## Recovery policy (implemented, replay only)

`Recovery` (`crates/clipper-recorder/src/logic.rs:291`) is a small state machine
driven by the worker thread, so there are no concurrent attempts:

```text
Idle ──failure (retryable, budget left)──▶ Waiting{due} ──due──▶ Attempting
  ▲                                           │                    │
  │◀── user command / shutdown (cancel) ──────┴────────────────────┤
  │◀──────────── success (session installed) ──────────────────────┤
  │◀──── failure, budget exhausted: GiveUp ── or ── Retry ─▶ Waiting
```

- **One pending or running attempt at most.** Duplicate failure reports are
  `Ignored`.
- **Generations.** Each scheduled attempt gets a new generation; `cancel` and
  `shutdown` bump it, and reports for an older generation are ignored (a
  stale success must not install its session).
- **Bounded.** The budget is per failure episode. A restarted session that
  fails again within 60 s counts against the same budget, so a flapping
  device cannot restart forever. After 60 s of stable running, a new failure
  starts a fresh budget.
- **Cancelled** by `StartReplay`, `StopReplay`, `StartRecording`,
  `StartAudioTest` and `Shutdown`. After shutdown nothing is scheduled again.
- **Restart = the user's last `StartReplay` request** (same source and
  settings) on a brand-new device, capture, encoder, audio and buffer. If the
  source is gone, the attempt fails as `SourceLost` and recovery ends.
- **Footage:** the failed session's buffer is saved automatically first (as
  before), so a restart never claims or mixes old footage; the new buffer
  starts empty.
- **Idle wake-up:** while an attempt is pending the idle worker waits on its
  command channel with a timeout until the attempt is due. No polling thread.

UI (unchanged components): the failure arrives as `ReplayStopped` with, for
example, "the GPU device was lost (graphics driver reset or GPU removed)
(0x887A0005); restarting automatically in 2 s". A successful restart sends
`ReplayStarted`, which the UI already accepts. When the budget is exhausted,
one final `ReplayStopped` says "…; it could not be restarted automatically".
Intermediate failed attempts are only logged, not toasted.

**Manual recording is not restarted automatically.** A file cannot continue
across a new encoder session without a gap; a restart would claim continuity
the file does not have. A GPU reset now gets the same explanatory message.

## Diagnostics

Logged (existing log, no per-frame output) for every failure: kind, technical
detail, HRESULT and the recovery decision. Also each attempt, its outcome and
time to recover, and cancellations by user commands. The existing replay stop
line still reports counters (pacer skips, repeats, re-anchors, audio loss).

## Tests

| Scenario | Test (`clipper-recorder`, `logic::tests`) |
| --- | --- |
| A, G normal start and successful restart (one new session) | `device_loss_is_restarted_once_after_a_delay` |
| B, D source loss / config failure: no false Running, no retry | `source_loss_and_configuration_errors_need_the_user` |
| C device loss: back-off, source gone during recovery | `device_loss_backs_off_and_stops_when_the_source_is_gone` |
| E encoder stops producing output | `watchdog_detects_a_silent_encoder` |
| H bounded retries; flapping | `retries_are_bounded`, `flapping_sessions_cannot_restart_forever` |
| I user stop during recovery | `user_stop_cancels_and_a_new_start_is_clean`, `stale_attempt_reports_are_ignored` |
| J shutdown during recovery | `shutdown_ends_recovery_for_good` |
| K duplicate failure events | `duplicate_failures_start_one_recovery` |
| L stale completion | `stale_attempt_reports_are_ignored` |
| Q restart after permanent failure | `user_stop_cancels_and_a_new_start_is_clean` |
| R idempotent cleanup (cancel/shutdown twice) | `user_stop_cancels_and_a_new_start_is_clean`, `shutdown_ends_recovery_for_good` |
| classification | `hresults_are_classified_by_meaning_not_by_failure` |

Each property was confirmed by mutation: removing the flapping rule, the
generation check, the duplicate guard or the watchdog makes the corresponding
test fail.

Not covered by automated tests (structural, Windows-only):

- **F, P** (manual recording failure, stop while failing): the existing
  `classify_stop` tests and the stop path.
- **M** (buffer per session): by construction.
- **N** (audio cleanup): the session drop order.
- **O** (save during failure): exports own `Arc` snapshots, plus the existing
  export tests and `examples/replay_stress.rs`.

These need the hardware plan.

## Performance

Normal operation adds two `Instant::now()` calls and two `Option` updates per
service step (≤ 33 per second), plus one scan of the packets drained in that
step for a video packet. No frame-path locks, copies, readbacks, polling
threads or encoder setting changes. Recovery work happens only after a
failure.

## Limitations

- A Media Foundation call that **blocks** instead of failing (a hung driver)
  blocks the worker thread; no watchdog thread was added. The UI stays
  responsive, but recorder commands wait.
- Teardown of an encoder on a removed device is best effort; its behaviour on
  every vendor's driver is unverified.
- The 5 s stall threshold and the retry delays are engineering choices, not
  measurements.
- A replay restarted after a device loss starts with an empty buffer; footage
  from before the failure exists only in the automatic save.

## Manual Windows validation (pending)

Record for each: expected vs observed state, whether replay resumes, saved
clips valid, resources released (Task Manager GPU/handles), UI accurate.

1. Replay on a game window; close the game → "capture source was closed", no
   restart, the automatic save contains the last minute.
2. Kill the game process → same as 1.
3. Replay on a secondary monitor; unplug it → stopped, no switch to another
   monitor; replug and start manually.
4. Change display resolution during replay → keeps running (pool recreate).
5. Toggle the game between fullscreen and windowed → keeps running or a
   classified failure.
6. Start/stop replay 50 times quickly → no leaked GPU memory or threads.
7. 30 min replay, save every 5 min → all clips valid.
8. **Controlled device loss** on a test machine only, with
   `dxcap -forcetdr` (Graphics Tools) → "GPU device was lost …; restarting
   automatically in 2 s", then running again; save works afterwards.
9. Same as 8 while no audio source is enabled.
10. Same as 8 during a manual recording → recording fails with the GPU
    message, the file is kept or removed per the publish rules, and a new
    recording starts.
11. Press Stop during the 2 s restart wait → no restart.
12. Quit clipuh during the restart wait → it exits, nothing restarts.
13. Desktop + microphone, then device loss → audio devices released (the
    microphone privacy indicator goes off) until the restart.
14. Repeat 8 on AMD, NVIDIA and Intel where available.
