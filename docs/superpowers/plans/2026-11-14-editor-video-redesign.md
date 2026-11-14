# Video Editor Redesign Implementation Plan

> **For agentic workers:** Execute independent media/model tasks with explicit file ownership; integrate and review in the main session.

**Goal:** Implement the approved Phase 6 editor with real playback, segment editing and exact MP4 export.

**Architecture:** Reuse the hardware Media Engine player. Keep source-time segments in a pure Rust edit session with bounded history. Decode and encode exact selected ranges on a background Media Foundation worker, preserving all audio streams. Generate a bounded strip of real thumbnails asynchronously.

**Tech Stack:** Rust, GPUI, Windows Media Foundation, existing theme tokens.

**Spec:** User-supplied Phase 6 specification and root `editor.png`; user approved recommended design and autonomous implementation.

## Global Constraints
- Never modify website or unrelated capture/replay infrastructure.
- Never overwrite source media. Export MP4 H.264/AAC only.
- No Phase 7 gain, mute, solo or waveform engine.
- Maintain persistent existing sidebar and capture bar.

## Review Focus
- Empty/deleted segment lists must disable export and remain undoable.
- Missing/corrupt media must report errors without UI blocking.
- Non-keyframe cuts must not include earlier video/audio.
- Multiple audio streams must retain shared timestamps across segment joins.
- Clip switch or editor suspension must release decoder and thumbnail resources.

## Tasks
- [x] Pure Rust edit session: test trim/split/delete/history/time conversion and timestamp parsing before implementation; run `cargo test -p clipper-editor`.
- [x] Exact export: test synthetic non-keyframe multi-segment video and multiple audio streams, cancellation and destination safety; implement native decoded NV12/PCM to H.264/AAC export; verify by reopening media.
- [x] Thumbnail strip: bounded background worker, requested timestamps, receiver-drop cancellation; test missing file and real frames.
- [x] GPUI integration: compact header, upper preview/inspector, tabs, numeric trim entries, drag handles, zoom/scroll, ruler, segment selection and actual audio rows; wire history and export file picker.
- [ ] Run required fmt/check/test/clippy commands; launch Windows GUI, inspect screenshots at multiple sizes and themes, exercise real media workflow; fix findings.
- [ ] Review final diff, document limitations and Phase 7 prerequisites, Conventional Commit, push feature branch, create Draft PR against main. Never merge.

Ruling: Native automation reports an unavailable pipe. Media and automated validation proceed; visual acceptance stays pending in the Draft PR. See `docs/editor-phase6.md` for evidence and limitations.
