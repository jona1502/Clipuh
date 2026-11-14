# Custom notification sounds implementation plan

Goal: let users retain the default notification sound or import and preview their own clip-saved notification sound in native Settings.

Architecture: extend the existing notification settings with an optional managed sound path. Validate and copy imports off the UI thread. Reuse current event policy and dispatch native Windows playback through one bounded worker. Preserve silent progress events and prevent duplicate balloon audio.

Scope: the supplied request ends at the Project heading. An optional clarification was sent; pending further requirements, implement default/custom WAV selection, import, preview and persistence. WAV PCM 8/16-bit mono/stereo, 8–192 kHz, at most 10 seconds and 5 MiB is an implementation choice for lightweight reliable playback. No new audio framework or capture changes.

## Tasks

- Backend: persisted optional path and legacy defaults; bounded RIFF validation; managed copy; asynchronous playback and default fallback. Test imports, malformed files, persistence and dispatch policy.
- UI: native file picker and background import; default/custom/preview/reset controls; errors retain prior selection. Integrate saved/error sounds once across foreground/background routes. Test event eligibility and settings behavior.
- Verification: full workspace formatting/check/tests/clippy, independent review, real Windows playback when available. Record UI and listening limitations. Commit, push and ready PR directly targeting main.

## Review focus

Imported original moved/deleted; missing managed file; invalid chunks/overflow; disabled notification/sound switches; bursts and saving/failure events; cancelled or failed picker; app exit while playback runs.

## Progress

Branch created from origin/main 02f24b2; independent of mixer PR #42. Existing implementation uses MessageBeep and notification sound boolean. Backend and UI ownership delegated separately.

Implementation and independent review completed. Final workspace fmt/check/test/clippy passed. Native playback smoke passed; UI interaction and listening limitations documented in docs/notification-sounds.md. Publish this isolated branch as a regular PR directly targeting main; retain the worktree for review.
