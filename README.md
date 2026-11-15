<p align="center">
  <img src="assets/branding/clipuh-icon-128.png" width="96" height="96" alt="clipuh logo">
</p>

<h1 align="center">clipuh</h1>

<p align="center">
  Instant replay and screen recording for Windows, written in Rust.
</p>

clipuh keeps a rolling replay buffer of your screen and saves the last moments as an MP4 clip
when you press a global hotkey. It also records manually, keeps a library of your clips and
includes a non-destructive editor for trimming them and mixing their audio tracks. The UI is
built with [GPUI](https://github.com/zed-industries/zed) (Zed's UI framework); capture and encoding use Windows Graphics Capture and
Media Foundation hardware encoders.

> **Status:** pre-release (version 0.1.0, not yet published). There is no installer or public
> download yet; see [Installation](#installation).

## Contents

- [Features](#features)
- [Screenshots](#screenshots)
- [System requirements](#system-requirements)
- [Installation](#installation)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Configuration and files](#configuration-and-files)
- [Troubleshooting](#troubleshooting)
- [Development](#development)
- [Versioning and releases](#versioning-and-releases)
- [License](#license)

## Features

**Capture**

- **Instant replay:** a hybrid buffer keeps recent footage in RAM and spills older segments
  to a disk cache. Replay length 15, 30, 60, 120 or 300 seconds; the RAM budget and disk
  limit are configurable. Saving writes the buffered stream to an MP4 without re-encoding.
  The buffer can start automatically with the app.
- **Manual recording** of the selected source to an MP4 file.
- **Capture sources:** a monitor or a single window.
- **Video:** H.264 at 1280×720, 1600×900, 1920×1080, 2560×1440 or 3840×2160 and 30, 60 or
  120 fps (combinations the H.264 level allows), bitrate up to 100 Mbps. Encoding is
  **hardware only** (see [System requirements](#system-requirements)).
- **Storage estimates** of the expected clip size and buffer usage for the current settings.

**Audio**

- **Multi-source mixer** with up to 8 sources: desktop audio, microphones and individual
  applications (per-process loopback). Recordings contain one mixed track plus up to 4
  isolated source tracks.
- Per-source **gain** (in dB, up to 200 %), mute, level meters and a per-source
  **noise gate**.

**Library and editor**

- **Clip library** with grid and list views, search, filters, favourites, multi-select,
  rename, delete and "show in folder".
- **Editor** (non-destructive; source files are never modified): timeline with zoom,
  thumbnails and waveforms, trim, split and delete segments, undo/redo, and per-track gain,
  mute and solo for multi-track recordings. Exports an H.264 MP4 with one mixed stereo AAC
  track. The Effects and Text tabs are not implemented yet, and edits are kept only while
  the clip is open (no project files).

**App**

- Global **Save Clip hotkey**, rebindable in the app.
- Clip-saved **notifications** with quick actions and a sound: a bundled default, or your own
  WAV/MP3 file.
- **Appearance:** dark, light or system theme, accent colour presets or a custom colour,
  UI density.
- Tray icon, minimise to tray, optional start with Windows, single instance (starting clipuh
  again brings the running window to the front).
- **Diagnostics:** log files, temporary detailed logging and a debug-log export.

## Screenshots

There are no up-to-date screenshots of the current app yet. The PNG files in the repository
root (`capture.png`, `library.png`, `editor.png`, `settings.png`) are design mock-ups, not
screenshots, and include third-party game artwork; see [docs/licensing.md](docs/licensing.md).

## System requirements

| | |
| --- | --- |
| OS | Windows 10 or Windows 11, 64-bit (x64). Development and testing so far happened on Windows 11. |
| GPU / encoder | A **hardware H.264 encoder** exposed through Media Foundation by the GPU driver (NVIDIA NVENC, AMD AMF, Intel Quick Sync). Direct3D 11. |
| Capture | Windows Graphics Capture (part of Windows 10 and 11) |
| Per-application audio | Windows 11, or Windows 10 build 20348 or later. Desktop and microphone capture work without it. |
| Storage | Space for your clips, plus the replay disk cache (default limit 4 GiB, configurable from 256 MiB). The app shows the expected size per clip. |

clipuh **never falls back to software (CPU) encoding.** If Media Foundation offers no hardware
H.264 encoder, or only a software one, replay and recording do not start and clipuh reports
"no hardware H.264 encoder is available". Systems without a usable GPU encoder (for example
many virtual machines, remote sessions or older GPUs) cannot record.

Hardware validation so far was done on an AMD Radeon RX 9070 XT. NVIDIA and Intel encoders use
the same Media Foundation path but have not been measured; no list of supported GPU
generations is claimed.

macOS and Linux are not supported: there the app builds only as a stub that exits with a
message.

## Installation

There is no installer and no published release yet. To run clipuh today, build it from source
(see [Development](#development)) on a Windows x64 machine with access to this repository.

Packaged builds will be published on this repository's
[Releases page](https://github.com/jona1502/Clipuh/releases) once the release process is in
place. Settings and clips are stored in your user profile, not next to the executable, so
replacing the executable keeps them.

## Keyboard shortcuts

**Global** (also while another app is focused):

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+F10` | Save the replay buffer as a clip (default; change it in Settings → Hotkeys or on the capture bar) |

There is no global shortcut for manual recording; use the Record button.

**In the app:**

| Shortcut | Action |
| --- | --- |
| `Ctrl+1` … `Ctrl+4` | Capture, Library, Editor, Settings |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous page |
| `Ctrl+K` | Command palette |
| `Ctrl+Q` | Quit |

**In the library:**

| Shortcut | Action |
| --- | --- |
| `Ctrl+F` | Search |
| `Enter` | Play the selected clip |
| `Ctrl+E` | Open in the editor |
| `F2` | Rename |
| `Delete` | Delete |
| `Ctrl+A` | Select all |
| `Esc` | Clear the selection or search |

## Configuration and files

| What | Default location |
| --- | --- |
| Settings | `%APPDATA%\Clipper\ClippingExe\config\settings.toml` |
| Saved clips and recordings | `Clipper` in your Videos folder (usually `%USERPROFILE%\Videos\Clipper`) |
| Replay disk cache | `%LOCALAPPDATA%\Clipper\replay-cache` |
| Imported notification sounds | `%LOCALAPPDATA%\Clipper\sounds` |
| Logs | `%LOCALAPPDATA%\clipuh\logs` |

Some folder names still use the project's earlier internal name ("Clipper"); they stay as they
are so existing settings and clips are not moved.

Everything is configured in the app under **Settings**: Video (resolution, frame rate,
bitrate), Audio (sources and track layout), Replay (length, auto-start, RAM budget), Hotkeys,
Storage (clip folder, replay cache location and limit), Appearance, General (start with
Windows, notifications, sound) and Advanced (settings file, engine details, diagnostics). If
`settings.toml` contains an invalid value, clipuh does not start and names the offending
field, rather than silently recording with different settings.

## Troubleshooting

- **"no hardware H.264 encoder is available"** — update the GPU driver and make sure the GPU
  has an H.264 encoder. Remote desktop sessions and many virtual machines do not expose one.
  There is no software fallback.
- **A source shows black or cannot be captured** — Windows can exclude content from capture,
  for example windows that block screen capture such as DRM-protected video. Try capturing
  the monitor instead of the window.
- **A window is missing from the source list** — the program has to be running with a
  visible window.
- **Per-application audio is unavailable** — it needs Windows 11 or Windows 10 build 20348+;
  the mixer says so on the affected source. An application source waits ("… is not
  running") until that program runs.
- **"Another app already uses this shortcut."** — another program has registered the same
  global hotkey. Pick a different one in Settings → Hotkeys.
- **Replay or recording fails** — clipuh shows the error in the app. Check free space on the
  drives with the clip folder and the replay cache, then look at the logs.
- **Clips are missing** — check the clip folder in Settings → Storage. Clips saved before the
  folder was changed stay in the previous folder. Clips are plain MP4 files, so Explorer can
  find them too.
- **Logs** — `%LOCALAPPDATA%\clipuh\logs`. Settings → Advanced can turn on detailed logging
  (it switches itself off after 24 hours) and export the logs as a ZIP for a bug report.

## Development

Requirements:

- Windows 10/11 x64 (on other platforms the app only builds as a stub).
- [rustup](https://rustup.rs/). The toolchain (Rust 1.96.1 with rustfmt and Clippy) is pinned
  in `rust-toolchain.toml` and installed automatically.
- Visual Studio Build Tools with the "Desktop development with C++" workload and a Windows
  SDK (MSVC linker, and `rc.exe` to embed the app icon).

The workspace lives in `crates/`. The application package is `clipper-app`; internal crate
names keep the `clipper-` prefix.

```text
cargo run -p clipper-app                         # debug build, with a console window
cargo build --release --locked -p clipper-app    # optimized build in target\release\
```

Quality gates (the same as CI):

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Tests that need a GPU, a monitor or audio devices are marked `#[ignore]` because hosted CI
runners have none. Run them on real hardware with `cargo test --workspace -- --ignored`.

After changing dependencies, regenerate the third-party notices (see
[docs/licensing.md](docs/licensing.md)):

```text
cargo install --locked cargo-about@0.9.3
scripts/licenses/generate-notices.sh
```

Design and validation notes are in [docs/](docs/).

## Versioning and releases

The version is defined once, as `workspace.package.version` in [Cargo.toml](Cargo.toml), and
every crate inherits it. Changes are recorded in [CHANGELOG.md](CHANGELOG.md); version and
tagging rules are in [docs/versioning.md](docs/versioning.md).

## License

clipuh does not have a license yet. Until the owner chooses one, all rights are reserved and
this repository is not open source. Third-party components and their licenses are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md); see [docs/licensing.md](docs/licensing.md).
