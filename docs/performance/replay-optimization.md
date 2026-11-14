# Instant Replay — optimization results

Phase 10A. Setup, methodology and caveats: `replay-baseline.md`. "Before" =
`origin/main` @ `f3642ae`, "after" = branch `perf/replay-gpu-optimization`.
Both binaries were built from the same tree with only these changes and
measured in alternating order. Mean of 2 runs × 20 samples unless noted.

## Changes

1. **Redundant GPU copy removed.**
   - Before: every encoded frame was copied twice at full source resolution (2560×1440 BGRA, about 14.7 MB per copy): once into the converter's staging texture, once into a separate "last frame" texture used for static-scene repeats.
   - Now the staging texture *is* the repeat source: `PacketEncoder::submit_repeat` re-blits it and `PacketEncoder::stage` keeps the pre-origin frame.
   - Saves one copy per frame (~0.9 GB/s of GPU memory traffic at 60 fps) and one full-resolution texture in VRAM.
   - Same inputs reach the video processor, so output is unchanged.
2. **WGC paced to the configured fps.**
   - `CaptureSession::set_target_fps` sets `GraphicsCaptureSession.MinUpdateInterval` to half a frame interval minus 0.5 ms. The call is optional; Windows builds without it keep the default.
   - The WGC default (~60 updates/s) made a 120 fps replay or recording mostly repeated frames. At 60 fps it left the pacer no spare frame when one arrived late.
   - The cap is ~2× the target, so a 180/240 Hz game is still not copied at its full rate.
   - Applied to replay and to manual recording.
3. **Minimum repeat lag.**
   - The static-scene fill used to repeat a slot one frame interval after it ended. At 120 fps that is 8.3 ms, shorter than WGC's delivery latency, so real frames were overtaken by repeats and discarded as late.
   - The lag is now one interval but at least 1/60 s (`repeat_lag_ticks`). 30/60 fps behaviour is unchanged.
4. **Diagnostics.** The number of frames WGC delivered is now counted and logged when a replay stops (`capture delivered N frames`).

Encoder settings, resolution, fps, bitrate, profile, audio and the buffer were **not** changed.

## Before / after — 1080p60 Balanced, refresh workload (headless)

| Metric | Before | After | Change |
|--------|-------:|------:|-------:|
| GPU 3D (process) | 1.15 % | 0.89 % | −0.26 pp (−23 %) |
| GPU Video Codec / encode (process) | 34.83 % | 34.76 % | −0.07 pp (noise) |
| GPU Copy (process) | 0.00 % | 0.00 % | — |
| GPU 3D (dwm.exe) | 4.03 % | 4.20 % | +0.17 pp (within run-to-run spread: 3.82–4.35) |
| CPU (process, 1 core) | 11.0 % | 9.1 % | −1.9 pp |
| RAM (working set, max) | 102.1 MiB | 101.3 MiB | −0.8 MiB |
| Dropped frames | 0 | 0 | — |
| WGC frames delivered/s | ~57 | ~94 | +65 % (more candidates, same encode rate) |
| Repeated (duplicate) frames | 7.4 % / 3.7 % | 0.1 % / 0.1 % | −3.6 to −7.3 pp |
| Encoded fps | 60.0 | 60.0 | — |
| Clip bitrate (31 s clip) | 4.63 / 4.21 Mbit/s | 4.21 / 4.21 Mbit/s | — |

## Other scenarios

| Scenario | Metric | Before | After |
|----------|--------|-------:|------:|
| static, 60 fps | Proc. 3D | 0.92 % | 0.66 % |
| static, 60 fps | Proc. Video Codec | 34.45 % | 34.47 % (1 valid run) |
| real app visible | Proc. 3D / Video Codec | 1.16 % / 32.76 % | 0.84 % / 32.92 % |
| real app minimized | Proc. 3D / Video Codec | 1.09 % / 32.36 % | 0.79 % / 32.17 % |
| real app visible / min. | CPU (1 core) | 15.2 % / 12.9 % | 16.5 % / 13.6 % (1 run each, within noise) |
| 120 fps refresh | Proc. 3D | 2.21 % | 1.71 % |
| 120 fps refresh | Proc. Video Codec | 46.99 % | 45.74 % |
| 120 fps refresh | dwm 3D | 4.33 % | 5.54 % (WGC now delivers ~170/s instead of ~59/s) |
| 120 fps refresh | CPU (1 core) | 12.5 % (1 run) | 14.7 % |
| 120 fps refresh | Repeated frames | **54 % / 53 %** | **14 % / 14 %** |

At 120 fps the after-build spends slightly more DWM/CPU work, because WGC now
really delivers more than 60 frames/s. That is the price of real 120 fps
footage instead of 60 fps footage with every frame shown twice.

## Is the +30 pp resolved?

**No, and it cannot be without lowering quality.** The +30 pp is the AMD
Video Codec engine encoding 1080p60 H.264 (~33–35 %). It is the same before
and after, in the real app, and whether visible or minimized. It is
proportional to fps (30 fps: 17 %) and nearly independent of the
`Performance`/`Balanced` profile on this driver (`Quality` with B-frames:
66 %).

The engine is fixed-function hardware, separate from the shader cores, so it
does not translate one-to-one into lost game fps. The shader-side (3D)
overhead of replay was already small and is now ~23 % lower.

Users who want a lower encoder load can already pick 30 fps in Settings.
That is a quality trade-off, so it was not made silently.

## Quality and regression checks

- **`probe_clip` (full Media Foundation decode)** on before and after clips, 60 and 120 fps:
  - 1920×1080, exact 60.000 / 120.000 fps, intervals 16.66–16.67 / 8.32–8.33 ms.
  - 0 long intervals, 0 decode errors, AAC 48 kHz stereo intact.
  - Container and stream durations match (31.08 s).
- **Saves**: 4/4 per replay run (3-save burst + 1 delayed), RAM tier, no invalid packets, no audio glitch frames.
- **Manual recording, 1080p120 (`record_smoke`)**: 600 frames at 120.000 fps, 3 AAC tracks, decodes cleanly.
- **Visual check**: not done. Colour and orientation were not checked by eye. The colour path (same video processor, same colour spaces, same staging input) is unchanged by construction.
- **Not exercised**:
  - live game switching
  - GPU device loss
  - encoder failure
  - repeated start/stop within one process

  The existing `clipper-replay` soak/memory tests passed. The capture session `Drop` path is unchanged.

## Remaining bottlenecks

- Hardware H.264 encode, ~35 % of the AMD Video Codec engine at 1080p60. Intrinsic to the workload.
- One BGRA GPU copy per frame (WGC texture → staging). Removing it needs per-texture video-processor input views on WGC pool textures and holding pool frames longer. The measured gain (<0.5 % 3D) does not justify that risk now.
- 120 fps on a 180 Hz source still repeats ~14 % of frames. The 180→120 beat pattern plus WGC jitter leaves some slots without a frame inside the pacer tolerance.
