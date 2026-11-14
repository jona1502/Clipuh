# Phase 7 multi-track audio editor

Status: implemented; Draft delivery with visual and acoustic acceptance pending. Phase 6 was merged in PR #25. This branch starts from that implementation and integrates the subsequently merged Phase 9 base without changing another worktree.

## Editing and UI

The existing Editor layout, theme, hardware video preview and authoritative source timeline remain in place. Real stream metadata supplies stable per-file stream indices, stored stream names, original channel counts and sample rates. Missing role metadata gets generic labels; one unknown stream stays one stream and is not classified as game, microphone or Discord audio.

Track headers and the Audio inspector share one EditSession state: enable, mute, solo and linear amplitude gain from 0 to 200 percent. Enabled, unmuted solo tracks exclude non-solo tracks; mute always wins. The sum is saturated to [-1,1], so high combined levels can still distort; reducing source gains avoids that overload. No independent mixing policy is hidden in the exporter.

Audio selection, numeric trim entry, source-time drag handles, splitting and segment deletion are non-destructive. Source audio masks do not shift other tracks. Video cuts apply to every track; preview intersects audio masks with retained video ranges so buffered output cannot leak deleted video-range audio. Undo/Redo includes both video and audio edits, with 100 snapshots maximum. A gain or trim drag is one undoable gesture. Sessions remain in memory, matching Phase 6; there is no project-file persistence.

Real peak/RMS waveforms are drawn from cached summaries with the shared ruler, playhead, zoom and horizontal scroll. Disabled/muted tracks remain visible but visually subdued. Waveform extraction failures are reported instead of drawing invented data. Track rows use the existing density and semantic color tokens.

## Media architecture and limits

Media Foundation decodes real streams to 48 kHz stereo float PCM. Mono, stereo and supported multichannel/rate conversions use the native reader; unsupported layouts fail explicitly. Discovery and edits preserve source identities; source files are never rewritten.

A shared streaming mixer serves preview and export. Audible tracks use isolated readers so a sparse track cannot accumulate another stream's packets inside a shared demux queue. Limits are 32 tracks, one second of pending decoded PCM per reader and 8,192 frames per mixed block. Source timestamps and sample/frame conversion use integer arithmetic.

The existing MediaEngine remains the only playback time authority. A bounded WASAPI worker follows engine play/pause/seek state with 40 ms lookahead; the engine is muted before custom audio can start. Gain-only changes retain the endpoint, decoder and cursor. Commands coalesce into one pending track snapshot. Device notifications trigger controlled reopening; failures are explicit and never fall back to an unmuted source track that would violate edits. Paused playback waits for state changes; suspension drops the playback worker and decoder resources.

Waveform extraction runs asynchronously, supports receiver-drop cancellation and caches downsampled summaries by canonical path, size and modification time. The cache holds at most four files; at most four extraction workers run. Each stream has at most 16,384 peak/RMS buckets, targeting 10 ms resolution on short clips. Longer clips use coarser buckets; zoom never decodes the source again.

The Editor exports one standard stereo 48 kHz AAC mix in H.264 MP4, respecting mute, solo, gain, video edits and audio masks. Separate edited-stream output is not exposed. The existing internal Phase 6 export API that preserves source streams remains compatible. All-muted selections intentionally export silence; video-only clips remain video-only. Export uses bounded blocks, progress/cancellation, no-overwrite publication and the existing asynchronous Library refresh. AAC encoder padding can slightly extend the container duration.

## Validation evidence

- Required fmt/check/workspace tests/Clippy passed, including repository CI `--locked` variants. Final totals are recorded in the PR.
- Model tests cover mute/solo/gain, source masks, trim/split/delete, Undo/Redo, grouped gain gestures and video-history discovery preservation.
- Native tests cover actual stream discovery, missing/corrupt files, mono 44.1 kHz conversion, sparse timestamps, EOF seeks, peak/RMS summaries, cache/fingerprint bounds, gain updates without decoder replacement, silence and mixed-export content.
- The opt-in WASAPI preview smoke passed with rapid gain commands, mute/solo changes, seek, pause/resume and prompt shutdown. It checks device/decode/control operation, not an acoustic listening judgment.
- A real 1080p60 recording discovered three audio streams. Waveforms had 580 buckets per stream, including one genuinely silent stream. Extraction took about 256 ms on this machine.
- The real source played through custom multitrack preview, processed mute/gain changes and supplied the requested paused seek frame. Edited export requested 2.7 s, reopened at 2.70925 s with one stereo AAC stream, decoded 130,048 PCM frames, and played successfully. Source bytes were unchanged.
- That smoke ran alongside native hardware Replay: four of four saves succeeded, zero dropped frames were reported, and one pacer skip occurred. Replay reported roughly 59–62 FPS in this limited run. No general performance guarantee or long-clip benchmark is claimed.

Run a real file with `cargo run -p clipper-media --example multitrack_smoke -- <clip.mp4>`. Run the opt-in device test with `cargo test -p clipper-media player::tests::multitrack_preview_device_smoke --lib -- --ignored --nocapture`.

## Remaining acceptance and scope

Visual check (handoff, Claude): the app was launched on Windows 11 and a real 4-stream replay clip was opened in the Editor through the Library. A maximized screenshot was compared with root `editor.png`. Matching: sidebar, single-row capture bar, clip header with Export, preview with transport row, inspector tabs Trim/Effects/Audio/Text/Export, full-width timeline with ruler/zoom, video thumbnail track, separate audio rows with real waveforms, shared playhead. Adjusted in Phase 7 scope: waveforms are drawn on a -48..0 dBFS scale (real peaks; quiet voice/game tracks were near-flat lines on a linear scale) and each track gets an accent-derived hue (purple/blue/green with the default accent) on a neutral surface. Verified by clicks: Audio tab shows the selected track's identity, source stream, channels, rate, gain, mute, solo and trim; Mute greys the waveform; Play advances the shared playhead to the end.

Remaining differences, left to Phase 6 ownership: transport and timeline toolbars use text buttons instead of icons, no volume/scrub slider in the transport row, no per-track lock/visibility icons, and the paused preview shows black until playback starts. Track names stay generic ("Audio 1..n") because recordings carry no stream names; writing them requires a recorder change. Not verified: acoustic listening (audible mute/solo/gain), physical drag of trim handles, light theme and DPI scaling.

Waveform caches and sessions are in memory. Unknown audio roles remain generic until recording metadata persists them. Separate edited-track export, independent time-shifting and audio effects are not offered. The website, capture implementation, Settings and global shell were not redesigned. Existing Phase 6 playback staging copies and native encoder fallback behavior remain unchanged.
