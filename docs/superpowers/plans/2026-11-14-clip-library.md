# Clip Library Phase 5 Implementation Plan

> **For agentic workers:** Use executing-plans for inline integration; independent storage and platform tasks may use Luna Max subagents as requested by the user.

**Goal:** A real, responsive GPUI clip library sharing its index with Recent Clips.
**Architecture:** One in-memory filesystem index, reconciled off the UI thread on startup, save and native directory changes. Stable file identities and cached metadata feed both views; only a bounded page plus Recent Clips is decoded. User preferences and favorites use AppConfig's existing atomic persistence.
**Tech Stack:** Rust 1.96, GPUI 0.2.2, existing Windows Media Foundation and Shell APIs.
**Spec:** User-supplied Phase 5 handoff (2026-11-14); all 21 acceptance conditions apply.

## Global constraints
- Branch feat/ui-clip-library from origin/main because #17 and #20 are merged; isolated sibling worktree.
- No website changes, Appearance panel, editor redesign, shell duplication or capture/replay changes.
- Preserve old settings; no database; no fake metadata or gameplay.
- File operations off render; always confirm deletion; only update successful operations.
- Draft PR only; do not merge automatically.

## Review focus
- Files replaced or externally renamed while selected must never target another file.
- Failed rename/delete must retain remaining selections and show the error.
- Large libraries must not decode every video or allocate all image textures.
- Old configurations must keep their audio, recording and appearance values.
- Detached asynchronous work must not overwrite newer directory/page requests.

## Approach
Extend existing storage/settings and native media seams. A separate database/indexing service or a second player would duplicate working infrastructure. Use bounded pagination rather than a new virtualization framework; use native directory notifications rather than constant polling. Missing source metadata is shown as unavailable, never inferred as a game name.

### Task 1: Storage model and persistence
- [x] Add tested clip scanning/reconciliation, stable IDs, cached metadata, search/filter/stable sort, selection/range operations and safe rename validation.
- [x] Add backward-compatible LibrarySettings (view, sort, favorites) to AppConfig.
- [x] Tests cover duplicates, changes, missing files, rename conflicts, selection and old settings.

### Task 2: Native file integration
- [x] Extend shell with explicit-confirmation caller contract and recycle-bin deletion.
- [x] Add a cancellable native change watcher for one configured directory, bounded events and no directory creation.
- [x] Tests cover missing-path errors and watcher lifecycle.

### Task 3: Shared background index and thumbnails
- [x] Replace separate Recent Clips scan with a shared index used by Library and dashboard.
- [x] Decode representative frames with existing Media Foundation reader, serialize jobs and cap cached scaled images; invalidate on file fingerprint changes. No disk image cache is required (zero disk usage).
- [x] Refresh on saved/exported clips, navigation, explicit refresh and external directory changes.

### Task 4: GPUI Library
- [x] Theme-based responsive grid/list, real metadata, search/source/favorites filters, persisted sorting and mode.
- [x] Stable-ID single/Ctrl/Shift selection, keyboard controls, focus indicators and bounded pages.
- [x] Real play/editor/folder/favorite/rename actions and explicit delete confirmation with partial-failure feedback.

### Task 5: Validation and delivery
- [x] Run cargo fmt --all -- --check; cargo check --workspace; cargo test --workspace; cargo clippy --workspace --all-targets -- -D warnings.
- [ ] Interactive Windows visual QA: blocked by missing native automation pipe; documented incomplete in docs/clip-library.md.
- [ ] Review diff, Conventional commits, push, Draft PR against main; inspect CI. No merge.

## Execution rulings
- The user explicitly requested autonomous implementation; implementation choices follow the handoff and existing seams without a separate approval menu.
- One shared worker replaced the dashboard scanner; native source tags are unavailable and are not invented.
- Successful explicit deletion removes favorites; scans retain favorite IDs across offline folders and output-directory changes.
- Review identified and regression-tested partial-file exclusion and hard-link deduplication. Stale-path guards cover Library and Recent Clips.
- Duration sorting probes the full metadata index only when the Library is visible; image decoding remains page bounded.
- GUI QA cannot run because both supported desktop tools report the native pipe unavailable. Draft status is mandatory until interactive verification.
