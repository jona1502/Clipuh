# Phase 5 Clip Library

## Scope

The GPUI Library replaces the session-only table with a shared, real-file index.
The existing shell, single-row capture bar, theme tokens, media player/editor,
text input, dropdown, buttons, panels and native shell helpers are reused.
There is no Appearance panel, second player, database or website change.

## Index and background work

`clipper-storage::library` scans direct, completed MP4 children of the configured
recording directory. Partial recordings, symlinks and nested directories are
excluded. Windows volume/file identities provide stable selection and favorites;
hard-linked paths are deduplicated deterministically. A marked metadata/path
fallback is used when a native identity cannot be obtained; external rename
continuity of favorites is only guaranteed for native IDs.

One `LibraryWorker` serializes scanning, metadata probes and thumbnail decoding.
Requests coalesce through a one-slot pending request and wake channel; output is
bounded to four messages. Generations reject stale results. Index acknowledgements
use the same generation so preview requests cannot create a rescan loop.

Refreshes run on startup, saved recordings/replays, Library navigation, explicit
Refresh and native directory changes. The nonrecursive native watcher is idle
without filesystem activity. It coalesces changes after 250 ms quiet, with a
maximum 1 s notification interval during continuous writes; stop events take
priority. It can therefore still trigger bounded reconciliation while files are
being written. A missing/unwatchable folder displays a warning and supports
manual Refresh; saving a clip retries watcher setup.

Basic metadata is retained in RAM and reused by file identity plus size/mtime.
Search and filtering never scan the filesystem. Queries sort matching indices
with stable ID ties. Recent Clips reads the same index; it has no separate scanner.
Duration sorting explicitly reads all duration metadata on the background worker,
then shows the sorted collection. It does not decode every clip's video frames.
Unreadable durations sort last. Metadata work can be interrupted between files.

## Images and rendering

Media Foundation extracts a representative frame using the existing thumbnail
backend. Scaled images are capped at 320 pixels per dimension; the worker LRU
holds at most 40 results, including cached failures. File changes invalidate the
cache. No thumbnail files are written, so thumbnail disk-cache usage is zero.
The Library requests at most 24 page previews plus three recent previews; when
hidden it requests only the three recent clips. UI-held images outside the wanted
set are dropped from GPUI's image atlas. Native decoding still temporarily needs
a source frame; these bounds are not a total-process RAM guarantee.

Grid and list share one filtered index and stable-ID selection. Pagination bounds
rendered cards to 24. Wide windows use up to four columns; narrower widths reduce
the count. Keyboard selection scrolls the selected row into view. Search, filters
and pages retain selection IDs until explicit clear, successful deletion or a
confirmed index removal.

## Preferences and actions

AppConfig adds a defaulted LibrarySettings section for grid/list mode, sorting and
favorite IDs, using the existing ordered, atomic settings writer. Old config
files remain loadable. Scans do not erase favorites from offline volumes or other
configured folders; successful explicit deletion removes its favorite ID.

Click selects; Ctrl-click toggles; Shift-click extends a range. Arrows navigate,
Ctrl+A selects filtered clips, Escape clears, Enter plays, Ctrl+E edits and F2
renames while the clip grid has focus. Action controls support Tab and Enter/Space.
Right-click opens clip actions. Favorite toggles never modify media contents.

Play opens the existing associated Windows player; Edit opens the existing GPUI
editor and preserves the Library state. Open Folder reveals the indexed file.
Both Library and Recent Clips verify identity/fingerprint before opening paths.
Rename validates Windows names, missing/replaced files and conflicts, and uses an
atomic no-replace move. A configurable text-input limit preserves file stems up to
255 characters instead of silently truncating to the shared default of 64.

Delete always requires explicit confirmation. A dedicated STA performs native
IFileOperation recycling with no permanent-delete fallback. Only successful
operations clear selection/favorite IDs. Partial failures retain remaining IDs
and show a bounded error summary. Settings failures also surface in the Library.

## Verification status

Storage tests cover search, source/favorite filtering, every sort order, selection,
old settings, scanning, metadata retention, identity changes, duplicate hard links,
partial files, rename conflicts and stale-path verification. Worker tests cover
coalescing, generation acknowledgement, cache invalidation/limits, bounded image
sizes and the real scan/preview/removal channel flow. Native watcher tests cover
create/rename/delete, bounded burst notifications and prompt shutdown. A native
Recycle Bin smoke test uses only a newly created disposable temporary MP4.

Local validation (Windows, 2026-11-14): `cargo fmt --all -- --check`,
`cargo check --workspace`, `cargo test --workspace` and
`cargo clippy --workspace --all-targets -- -D warnings` passed. The full suite
reported 387 passed, 0 failed and 18 ignored; the explicit disposable-file
Recycle Bin smoke test also passed. The compound search/filter/sort test covers
10,000 synthetic index entries without filesystem access. These are correctness
checks, not a scrolling or resource benchmark. CI links are recorded in the Draft PR. Hardware-only
capture/audio tests remain explicitly ignored by their existing test annotations.
No performance benchmark or measured scrolling FPS is claimed.

## Visual QA and remaining work

`docs/design/references/` is absent. Existing GPUI components supplied the design
reference; no comparison against missing images is claimed.

Windows UI automation was attempted through both the Computer Use skill and CUA.
Both report `Computer Use native pipe is unavailable` (Windows error 2). Therefore
Library launch/interactions, card alignment at multiple widths, physical DPI,
light/dark/custom accents, associated-player launch, Explorer selection and
large-library scroll performance remain visually unverified. Compilation and
unit tests do not substitute for these checks. Keep the PR Draft until this QA
is performed on an interactive Windows desktop.

File source/game labels are shown only when indexed metadata provides them. The
existing recordings do not supply persisted game tags, so source controls remain
disabled rather than invent categories. The existing simple text input still
uses append/backspace/paste editing and has no full cursor selection or IME.
Linux remains planned. Phase 6 can redesign the Editor around the existing
Library-to-editor navigation; no Editor redesign is included here.
