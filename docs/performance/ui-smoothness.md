# UI smoothness (P1) — measurements

Measured 2026-11-14 on this machine. "Before" = `origin/main` @ `3a28e14`, "after" = branch `perf/gpui-ui-smoothness`. Both are release builds measured alternately with the same user config (replay running: autostart is on). Nothing below is estimated.

## Environment

| Item | Value |
|------|-------|
| GPU / driver | AMD Radeon RX 7700 XT, 32.0.32015.7029 |
| Display | 2560×1440 @ 180 Hz (5.56 ms per refresh), 100 % scale |
| CPU | Ryzen 9 5900X |
| OS | Windows 11 Pro 10.0.26200 |
| UI | GPUI 0.2.2 (DirectX 11 renderer, DwmFlush vsync thread) |
| Window | 1600×1000, positioned by the harness |

## Method

- **Frame timing.** GPUI's own frame timing, `ZED_MEASUREMENTS=1`, prints one `frame duration` per drawn frame. That covers layout, paint and present. The app's stderr is redirected to a file.
- **Input.** `scripts/perf/ui_driver.ps1` drives the real app with synthetic input (`SetCursorPos`, `mouse_event`, `keybd_event`):
  - mouse wheel: 120-unit notches, 16 ms apart, 40 down and 40 up
  - touchpad-style: 20-unit deltas, 8 ms apart, 150 down and 150 up
  - page shortcuts and clicks
- **Scripts.**
  - `ui_bench.ps1`: idle / Settings / Library / navigation scenarios.
  - `editor_bench.ps1`: playhead.
    - Searches the Library for the 26.3 s clip `…00-46-56` and opens it in the Editor, at zoom 100 % or 338 % (3 zoom clicks).
    - Starts playback and samples a 1 px screen strip through the timeline. The sampler runs at about 60 Hz (`CopyFromScreen`) and locates the white playhead (weighted centroid of near-white pixels).
    - Then drags across the video track to scrub.
  - `ui_resources.ps1`: process CPU and GPU engine counters per scenario, via `gpu_sample.ps1`.
- **Discarded runs.** Runs where the harness failed to open the Editor were discarded: the window lost focus, or an early harness version maximized the window. A screenshot of every Editor-open is saved to `%TEMP%`.

### Limitations

- **Frames per second here means drawn frames, not refresh rate.** GPUI only draws when something changed.
- **The playhead sampler cannot see beyond ~60 Hz** and only resolves whole pixels. At 100 % zoom the line moves 38.6 px/s, so whole-pixel steps every ~26 ms are what the sampler can see no matter how smooth the rendering is.
- **The Library has only 8 clips.** The Library page barely scrolls, so Library scrolling was not meaningfully exercised.
- **Inputs were synthetic.** Real precision-touchpad gestures and real wheel hardware were not tested by hand. On Windows both arrive as `WM_MOUSEWHEEL`.
- **Replay was running in every measurement**, because it starts from the user's config. Measuring the "replay off" scenario would have meant editing the user's settings, so it was skipped.
- **CPU figures are process-wide.** They include the replay worker, about 12 % of one core.

## Root causes found

1. **Mouse-wheel scrolling jumps.**
   - GPUI applies a wheel notch as one instant 3-line jump (~60 px). Wheel scrolling therefore shows exactly one new position per input event: ~35 per second while spinning, 60 px apart. That reads as a ~30 fps UI.
   - Frame cost was not the problem. Settings frames take ~8 ms, and none exceeded 16.7 ms.
2. **The Editor playhead advanced only on player events.**
   - The playhead moved only on decoded-frame events (video frame rate) plus a lossy 100 ms position poll.
   - Its motion followed decode and channel jitter: P95 gap between moves 32–34 ms, max 39–56 ms, at a 60 fps clip. For 30 fps clips the steps are 33 ms by construction.
   - Seeks across trimmed segments froze the playhead until the first post-seek frame arrived.
3. **Waveforms were rescanned on every paint.** Each waveform paint folded all audio buckets for up to 2048 columns per track. That is about 1 ms of a 4.3 ms Editor frame (measured by disabling waveforms in an experiment build).
4. **Ruled out.**
   - No I/O, decoding or entity creation in render paths.
   - The Library is already its own view, with pre-decoded thumbnails and paging (24 cards).
   - Audio meters are throttled (10 Hz, Dashboard only).
   - No idle redraw loops: about 1 frame/s idle, which is the 1 Hz replay status.
   - Minimized windows draw 0 frames.

## Changes

- **`components::smooth_scroll`**
  - Wheel notches (≥ 1 line) ease to the same target over the next refreshes: exponential ease with τ = 45 ms, ~95 % after 135 ms.
  - Pixel deltas and sub-line deltas (precision touchpads, free-spinning wheels) pass through untouched, so they gain no latency.
  - Programmatic scrolling cancels the animation. This covers the Settings category anchors.
  - No animation runs when the target cannot move.
  - GPUI's own scroll handling (bounds, nesting) is unchanged. The animation state lives in `use_keyed_state`, so no new fields were added to the root view.
  - Applied to Settings and the Library.
- **`editor::playhead::PlayheadClock`**
  - The player stays the only clock. Every frame or position report re-anchors the model.
  - Between reports the playhead advances with `Instant`.
  - Small errors are slewed out (15 % per report), never moving backwards. Errors over 150 ms snap. Extrapolation is capped at 250 ms past the last report.
  - The playhead is clamped to the end of the playing segment.
  - Seeks and segment jumps move it immediately.
  - Paused, ended and error states show the authoritative position.
  - The time readout and I/O marks keep using the authoritative position.
- **Display-paced playhead redraw.** While playing, a per-refresh `on_next_frame` check redraws only when the line moved ≥ 0.5 px. At 100 % zoom most refreshes therefore cost nothing; zoomed in, it redraws every refresh. Nothing runs while paused, and GPUI skips minimized windows.
- **`editor::audio::PeakCache`** keeps column peaks per track and layout (16 entries, FIFO). Painting reads cached peaks instead of rescanning buckets.

## Results

### Scrolling (frame log; before 2 runs, after 3 runs)

| Metric | Before | After |
|--------|-------:|------:|
| Settings mouse wheel: drawn frames/s | 35.2 / 35.7 | 69.6 / 71.3 / 67.1 |
| Settings mouse wheel: frames per notch | 1 | ~2 at this input rate; 10–60 for an isolated notch (unit test at 180 Hz) |
| Settings wheel: median / P95 / P99 frame time (ms) | 8.2 / 9.9 / 13.2 — 8.0 / 9.5 / 10.1 | 7.3 / 8.7 / 10.2 — 7.1 / 8.6 / 9.0 — 7.5 / 10.3 / 12.2 |
| Settings touchpad-style: frames/s | 60.6 / 60.8 | 60.6 / 60.7 / 61.0 (unchanged pass-through) |
| Settings touchpad-style: median frame (ms) | 7.9 / 8.0 | 7.9 / 7.8 / 7.6 |
| Frames over 16.7 ms (any scenario) | 0 | 0 |
| Idle drawn frames/s | 1.0 | 1.0–1.4 |
| Navigation: median frame (ms) | 5.2 / 4.4 | 5.4 / 5.1 / 4.7 |

At ~7.5 ms per Settings frame, a 180 Hz display gets a new frame every second refresh (~90 Hz) during an animation. Lower Settings frame cost is listed under remaining work.

### Editor playhead (60 fps clip, playback)

| Metric | Before | After |
|--------|-------:|------:|
| Zoom 338 %: playhead moves seen by the ~60 Hz sampler, per second | 54.5 / 54.9 / 50.1 | 66.6 (moves on every sample) |
| Zoom 338 %: gap between moves, median / P95 / max (ms) | 16.6 / 32.5 / 39.1 — 16.8 / 32.3 / 45.6 — 17.5 / 33.7 / 43.5 | 15.6 / 18.8 / 33.2 |
| Zoom 338 %: drawn frames/s during playback | 65.7 / 64.2 / 63.7 | 143.7 |
| Zoom 100 %: gap median / P95 / max (ms) | 31.0 / 49.2 / 55.5 | 27.9 / 37.6 / 50.1 |
| Zoom 100 %: drawn frames/s | 63.5 | 77.8 |
| **Speed: measured px/s** (clip-time accuracy) | 134.7 / 134.7 / 135.2 and 38.8 | 135.1 and 38.6 |
| Backward steps | 0 | 0 |
| Playback frame time, median (ms) | 5.0–5.2 | 4.2–4.9 |
| Scrub (drag) drawn frames/s | 83–89 | 84–86 (unchanged; follows input) |

**Accuracy.** The playhead speed is identical before and after, so there is no drift against the clip.

**Smoothness.** Zoomed in, the P95 gap fell from ~33 ms to 19 ms, and the line now moves on every sample. That is the sampler's limit; the frame log shows ~144 draws/s.

### Resources (replay running; process totals)

| Scenario | CPU (% of one core) before → after | GPU 3D (process) before → after | dwm 3D before → after |
|----------|-----------------------------------:|--------------------------------:|----------------------:|
| Idle, Settings page | 17.3 → 11.9 | 1.19 → 1.16 % | 4.59 → 4.42 % |
| Continuous Settings wheel scrolling | 32.5 → 80.7 | 2.55 → 2.26 % | 5.87 → 5.56 % |
| Editor just opened (one-time thumbnails + waveform analysis) | 144 → 126 | 1.39 → 1.17 % | 5.03 → 4.50 % |
| Editor paused, settled | 12.1 → 14.9 | 1.16 → 1.15 % | 3.77 → 4.46 % |
| Editor playback, zoom 100 % | 59.5 → 71.5 | 4.41 → 4.68 % | 5.21 → 5.64 % |

- Video codec engine stayed at ~35 % in all scenarios: the replay encoder, unchanged.
- GPU 3D did not change meaningfully.
- **CPU rises only while something animates**, because more frames are drawn:
  - Wheel scrolling Settings costs ~48 percentage points of one core more. Measured during continuous scrolling.
  - Playback costs ~12 points more at 100 % zoom. That is after the ≥ 0.5 px redraw rule; drawing on every refresh at 100 % zoom cost 110 % of one core in an intermediate build.
  - Idle and paused cost the same as before.

## GUI checks performed (synthetic input, screenshots inspected)

- **Settings.**
  - Wheel scrolling moves the content smoothly and keeps the category sidebar fixed.
  - Clicking "General" after wheel scrolling jumps to its section.
  - No overlap or clipping in the screenshots.
- **Window.** Minimized: 0 frames drawn in 5 s. Restore redraws correctly.
- **Editor.**
  - Opens from the Library and plays.
  - The playhead moves at the clip rate.
  - Scrub follows the pointer.
  - Zoom in works during playback.
- **Not exercised:**
  - trim and split boundaries during playback (covered by unit tests of the clock: segment clamp and jump)
  - favoriting while scrolled
  - theme switching
  - DPI scales other than 100 %
  - a library large enough to scroll
  - a real touchpad

## Remaining work

- **Settings frame cost (~7.5 ms).** Settings is rendered inline in the root view, so every scroll frame rebuilds the sidebar, the capture bar and all eight Settings cards. Moving Settings into its own cached entity (`AnyView::cached`) would let it scroll at the full refresh rate and cut CPU while scrolling. It needs `views/mod.rs` and `state.rs`, which PR #30 is editing, so it is deferred.
- **Root `render` calls `sync_tray()` every frame** (`views/mod.rs`). This is cheap but redundant with the event loop. Also deferred because of PR #30.
- **The editor redraws the whole window per playhead step.** GPUI 0.2.2 re-renders uncached views every frame. A cached timeline entity would make playhead frames cheaper.
- **The video preview** is still decoded with a GPU→CPU readback per frame. That is out of scope here and unchanged.
