# Phase 6 video editor

Status: implemented on `feat/editor-video-redesign`; Draft PR, visual acceptance pending. Phase 5 was merged as PR #22, so the base is `main` at `ebd9cc3`.

## Behavior

The editor uses the existing sidebar, capture bar, semantic theme, hardware Media Engine player, audio output and seeking. The workspace now contains a clip header, large aspect-preserving preview, compact inspector and full-width source timeline. Fullscreen preview shares the same player and exits with Escape.

Source-time segments reference the original media. Numeric timestamps and drag handles trim a selected segment; Split, Delete, Undo and Redo update the same edit session. History is capped at 100 operations. Empty edits disable playback/export and remain undoable. Preview advances across retained ranges using player timestamps; it has no independent clock.

The source timeline includes a ruler, playhead, click/drag seek, zoom, horizontal scrolling, real thumbnail strip, segment borders and separate rows for each audio stream found by Media Foundation. Audio roles are not inferred: files currently expose stream counts rather than durable game/microphone/application labels. No waveform or track control is fabricated.

Thumbnail extraction uses one decoder worker, a queue of two frames and at most 12 retained images, each at most 160 by 160 pixels. Switching clips or suspending the editor drops its worker receiver, decoder and GPU images. Paused playback has no editor polling loop.

Export snapshots retained source ranges, asks for an MP4 destination and runs on a background worker with progress, cancellation and completion/error reporting. The new file is published with no-overwrite `MoveFileW`, including destination races. Original media is read only. Successful export requests an asynchronous Library refresh.

## Exact-range export

Arbitrary cuts use decoded NV12 video and PCM audio followed by native H.264/AAC encoding. Each stream shares the segment origin; video presentation durations are clipped at cuts, PCM is sliced at audio sample boundaries, and retained ranges are concatenated without the deleted spans. The editor uses the smaller of the player and container durations. Invalid ranges fail instead of snapping to earlier keyframes.

Hardware transforms are enabled; Windows may select a software codec. Hardware-only encoding and stream-copy optimization are not promised for this export path. The existing playback staging copy is retained; this phase does not claim zero-copy GPUI presentation.

Decoder changes from 1080 visible lines to 1088 storage lines are accepted only after validating the display aperture, subtype, frame rate and aspect/interlace metadata. Actual video resolution or PCM layout changes fail explicitly. Output is restricted to supported H.264/AAC MP4 with even video dimensions; unsupported source streams/codecs report errors.

AAC packet/encoder padding can make the container duration slightly longer than the requested content. The real three-track smoke requested 2.675 s and reopened at 2.6879791 s, about 13 ms longer. No earlier keyframe content is added. The inspector discloses encoder padding.

## Validation

- Required formatting, workspace check and Clippy with `-D warnings` passed. Workspace tests: 408 passed, zero failed, 19 ignored (hardware/manual or opt-in tests). The opt-in real-file export regression also passed when explicitly invoked.
- Model tests cover trim validity, split boundaries, deletion, bounded Undo/Redo, invalid redo transitions, timestamp parsing and time/pixel conversions.
- Editor tests cover source playback transitions through removed ranges, edited completion and scrolled timeline mapping.
- Native media tests decode all streams after arbitrary non-keyframe multi-segment export; check monotonic timestamps, synchronization, contiguous joins, first-frame content, original bytes, destination races and cancellation cleanup.
- Real 1080p60 recordings with zero and three audio streams passed playback, pause, seek, exact segment export and exported playback/reopen. Source bytes remained identical. The three-track clip retained all three streams.
- An existing native Replay smoke ran concurrently with the real playback/export smoke. Four of four replay saves completed; reported dropped frames were zero, with one pacer skip. This is a limited smoke, not a general performance guarantee.
- `cargo test -p clipper-media real_clip_exact_segment_export -- --ignored` can exercise another real MP4 after setting `CLIPUH_EDITOR_TEST_CLIP`.

## Visual QA and remaining acceptance

The approved root `editor.png` was inspected; the expected `docs/design/references/editor.png` was absent. Layout follows that image and the supplied specification. Both available Windows automation interfaces failed with an unavailable native pipe. No editor screenshot, multiple-size/DPI inspection, theme switching, fullscreen interaction or complete Library-to-Editor mouse workflow could be executed through those interfaces. Build/media success does not establish visual fidelity. Keep the PR in Draft until this runtime visual and interaction QA passes; Phase 6 is not declared fully accepted.

Edit sessions are in memory and reset when another clip replaces them; no project-file persistence or saved-state claim is introduced. Preview audio follows the existing Media Engine stream selection; independent track mixing is not implemented. Effects, Text and detailed audio editing remain unavailable.

## Phase 7 prerequisites

Persist recording stream identities/roles before showing category-specific labels. Build waveform generation and bounded caches on the existing aligned track rows and source-time segment model. Add track-specific edit state, gain, mute/solo, independent trim, synchronization and an explicit preview/export mixing policy. Do not infer stream roles from the current application's audio settings.
