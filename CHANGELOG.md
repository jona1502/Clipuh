# Changelog

All notable changes to clipuh will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Release
process: [docs/versioning.md](docs/versioning.md).

## [Unreleased]

## [0.1.0] - 2026-11-15

First prerelease: a GitHub prerelease in the private repository for tag `v0.1.0` (`a7c45d9`),
with a manually attached `clipuh-v0.1.0-windows-x64.exe`, published before the automated
release workflow existed.

### Added

- Instant replay with a hybrid RAM/disk buffer (15 s to 5 min), saved to MP4 without
  re-encoding through a global, rebindable hotkey (default `Ctrl+Shift+F10`). (#10, #5)
- Manual recording of a monitor or window using Windows Graphics Capture and Media Foundation
  hardware H.264 encoding, without software fallback. (#1, #2)
- Video settings for 720p to 4K, 30/60/120 fps and bitrate, with storage estimates derived
  from the encoder settings. (#8, #9)
- Multi-source audio mixer: desktop, microphones and per-application loopback, up to 8
  sources, with one mixed track plus up to 4 isolated tracks per recording. (#4, #6, #17, #20)
- Per-source gain in dB and noise gates. (#42)
- Clip library with persistent index, grid and list views, search, filters, favourites,
  multi-select, rename, delete and show-in-folder. (#22, #27, #39, #40)
- Editor with video timeline, trim/split/delete, undo/redo and exact segment export. (#11, #25)
- Multi-track audio editing with waveforms, per-track gain, mute and solo, resizable tracks
  and timeline zoom; export with one mixed stereo AAC track. (#28, #41)
- Clip-saved notifications with quick actions, a bundled default sound and custom WAV/MP3
  sounds. (#26, #44, #45)
- App shell with Capture, Library, Editor and Settings pages, command palette and keyboard
  navigation; Capture dashboard cards. (#13, #14, #16, #47)
- Settings with category navigation: dark/light/system theme, accent colours, UI density,
  start with Windows, tray icon and minimise to tray, diagnostics logging with debug-log
  export. (#24, #37)
- clipuh logo and Windows application icon. (#21, #34)
- Single instance: a second start brings the running window to the front. (#53)
- Error dialogs and logged crash reports when clipuh cannot start or stops unexpectedly. (#53)
- CI for formatting, Clippy and tests on Windows. (#7)

### Changed

- Release builds run as a Windows GUI application without a console window. (#53)
- Lower encoder load for the Quality replay profile on AMD GPUs. (#50, #51)
- Smoother scrolling in the library and editor timeline. (#32)

### Fixed

- Consecutive replay saves and reliable shutdown of the replay buffer. (#30)
- Opening files and folders no longer blocks the UI. (#48)
- Timeline scrolling, audio track order and the initial window size. (#52)

[Unreleased]: https://github.com/jona1502/Clipuh/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jona1502/Clipuh/releases/tag/v0.1.0
