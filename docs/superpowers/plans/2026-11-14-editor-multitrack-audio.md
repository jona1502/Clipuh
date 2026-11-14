# Phase 7 implementation plan

**Goal:** Real audio stream discovery, bounded waveform extraction, shared preview/export mixing and undoable source-time audio edits.

**Architecture:** Extend Phase 6 EditSession snapshots with stable source stream IDs and audio masks/settings. A shared Media Foundation PCM decoder and bounded mixer feed both waveform summaries and exact export/preview; the existing video engine remains the authoritative clock. Preserve existing UI structure/theme and all source files.

**Spec:** User-supplied Phase 7 specification; direct implementation explicitly approved; root editor.png inspected. Phase 6 merged in PR #25; new isolated worktree, base main.

## Ownership and tasks
- [x] Root: model audio state/history tests and implementation; UI controls/Audio inspector/waveform rendering; Cargo/module glue.
- [x] Audio worker: media/audio.rs actual discovery,48kHzstereo PCM decoding, bounded shared mixer and fingerprinted waveform cache; synthetic tests.
- [x] Preview worker: media/player.rs and audio_preview.rs; existing engine-clock integration, suppress duplicate engine audio, bounded WASAPI rendering and errors.
- [x] Export worker: media/exact.rs and focused smoke example; mixed single-stereoAAC output, exact video ranges and audio masks/settings; content/timestamp/source-preservation tests.
- [x] Run fmt/check/test/clippy, real multitrack decode/preview/export/reopen and replay smoke where available; independent review.
- [x] Visual QA through native automation (handoff run): screenshot compared with editor.png; unexecuted interactions documented.
- [x] Conventional Commits, push feat/editor-multitrack-audio, regular (non-draft) PR against main. Never merge.

## Decisions and review focus
- Audio trim/split masks source time without moving other streams; video retained ranges apply to all tracks.
- Gain uses integer linear amplitude percent0..200; sum in float, clamp final signal to[-1,1]. Enabled unmuted solo tracks gate non-solo tracks; mute always wins.
- Mixed standard output is supported; original independent-stream export remains available internally but UI never promises edited separate-stream output.
- Validate variable stream counts, timestamp gaps, rates/channels, empty/allmuted selections, seek/pause, cancellation, bounded memory and device errors.
- Missing source-role labels remain generic; never infer them from current capture settings.

Ruling: native automation pipe is unavailable; visual and physical listening acceptance remain pending. See docs/editor-phase7.md. Current main (Phase 9) integrated only into this branch; no source changes were made in other worktrees.
