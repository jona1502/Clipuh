# Capture mixer gain and noise gate implementation plan

Goal: implement the supplied Capture Audio Mixer specification without changing capture topology or timestamps.

Architecture: retain persisted integer percent gain as the sole authority. Convert validated dB edits at commit. Apply independent linked-channel gates before gain in the existing timestamp mixer shared by recording, replay and WAV tests.

Tech stack: Rust, GPUI, existing Windows capture and configuration.

Spec: user task dated 2026-11-14 and capture.png.

## Constraints

No extra capture streams, buffering, sample allocations or blocking. Disabled gate preserves the signal. Keep existing controls and compact layout. No acoustic claims without listening. PR targets main.

## Tasks

- [x] Engine: bounded serialized settings, default disabled, migration/persistence, live per-source recorder/replay command, gate before gain, reset handling. Test threshold, attack/hold/release, hysteresis, bypass, isolation, transitions, timestamps and configuration.
- [x] UI: native child input, double left click, focus/select, Enter/Escape/blur, strict comma/point dB parsing into supported percent. Share slider mutation and persistence. Compact gate control and bounded settings. Test parsing and synchronization.
- [x] Integration: full diff review; fmt, workspace check/tests/clippy; resolve failures; independent review; document hardware and performance limitations.

Publication: commit and push this isolated branch, then create a regular PR targeting main. Retain the worktree for review.

## Review focus

Source removal during editing; settings changed during recording/replay; zero gain; finite validation; source loss and timing discontinuity.

## Progress

Worktree created from origin/main d843823. Initial checkout clean. Existing open PRs unrelated. Screenshot inspected. Engine and UI delegated to separate ownership scopes. Baseline workspace build started.
