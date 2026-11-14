# Instant Replay — performance baseline

Phase 10A. Measured on `origin/main` @ `f3642ae` (before any optimization) on
2026-11-14. All numbers below were measured on this machine. Nothing is
estimated.

## System

| Item | Value |
|------|-------|
| GPU | AMD Radeon RX 7700 XT, driver 32.0.32015.7029 |
| Display | 2560×1440 @ 180 Hz (primary monitor, captured) |
| CPU / RAM | AMD Ryzen 9 5900X (12C/24T) / 32 GB |
| OS | Windows 11 Pro 10.0.26200 |
| Capture backend | Windows Graphics Capture (WGC), monitor item, free-threaded frame pool, BGRA8, 2 buffers, cursor on |
| Encoder backend | Media Foundation hardware H.264 MFT (AMD AMF), D3D11 input, async |
| Replay settings | 1920×1080, 60 fps, 16 Mbit/s, `Balanced` profile, 30 s, 64 MiB RAM + SSD spill. These are the app defaults and match the user's `settings.toml`. |

## Pipeline (as audited)

```text
WGC frame pool (BGRA 2560x1440, GPU)
  -> [pump] newest frame only, older pending frames closed unused
  -> FramePacer (60 fps grid, quarter-interval tolerance)
  -> SlotTimeline (constant frame rate; gaps filled with repeated frames)
  -> CopySubresourceRegion  WGC texture -> converter staging (BGRA, GPU)
  -> VideoProcessorBlt      staging -> NV12 1920x1080 (GPU scale + BT.709 limited)
  -> AMF H.264 MFT          NV12 pool of samples (GPU, zero CPU copy)
  -> AVCC packets (CPU, compressed only) -> hybrid RAM/SSD ring buffer
  -> exporter thread muxes MP4 on save
plus, before the change: CopySubresourceRegion WGC texture -> "last frame"
texture for every submitted frame (repeat source).
```

Questions from the brief:

- **Capture API**: WGC only. One capture session and one encoder per replay. Manual recording is a separate session and never runs at the same time.
- **CPU round trips**: none. Frames stay GPU textures until the encoder. Only compressed packets reach the CPU. Nothing is uploaded back.
- **Color conversion / scaling**: one GPU video-processor pass (BGRA→NV12 plus scale). Nothing on the CPU.
- **Copies per encoded frame**: two full-resolution BGRA GPU copies: WGC→staging, plus WGC→last-frame. The second is redundant (fixed, see `replay-optimization.md`).
- **Live preview**: none consumes capture frames.
- **Stale frames**: superseded WGC frames are closed without processing.
- **Encoder options**: Balanced = peak-constrained VBR, quality-vs-speed 50, GOP 2 s, no B-frames.
- **UI**: replay status is sent at 1 Hz. Audio meters go out at 10 Hz and are redrawn only on the Capture page, only when a change is visible.

## Methodology

- `scripts/perf/replay_bench.ps1` runs the headless `replay_smoke` example (the real `Recorder` worker, `ReplaySession`, `PacketEncoder` and `ReplayBuffer`) for 35 s. It samples 20 s after a 10 s warm-up. The run ends with 4 clip saves.
- `scripts/perf/gpu_sample.ps1` reads the Windows `\GPU Engine(*)\Utilization Percentage` counters once per second. For each engine type it sums the instances of the replay process, of `dwm.exe` and of the whole system.
- Workloads:
  - `refresh`: an Edge `requestAnimationFrame` page in a throwaway profile, animating at the display refresh rate. This approximates a game.
  - `static`: no extra workload.
  - The `off` runs show the same workload without replay.
- The real app (`clipper-app`, replay autostart from the user's config) was also measured, with the window visible and minimized.
- CPU is the process CPU time over the sampling window. "% of one core" = share of a single logical core.

### Measurement caveats

- Brave and Discord were running and decoding video the whole time. That adds about 9 % system-wide on the shared AMD **Video Codec** engine (VCN does both decode and encode). Per-process numbers are not affected; system-wide numbers include it.
- Task Manager's per-process "GPU" column shows the **busiest engine** of a process. For clipuh that is the Video Codec engine. It is a fixed-function block, separate from the 3D shader cores a game uses.
- The `static` workload was never fully static: background apps repaint now and then.
- A sample with a counter wrap (values > 10^12 %) was discarded.
- Scenario D (real game) was not run: no game was available to the agent. The `refresh` workload stands in for it.

## Results (before)

Mean of 2 runs × 20 samples. 1080p60 Balanced unless noted.

| Scenario | Proc. Video Codec | Proc. 3D | dwm 3D | Proc. CPU (1 core) | Working set |
|----------|------------------:|---------:|-------:|-------------------:|------------:|
| A — refresh workload, replay **off** | — | — | 2.98 % | — | — |
| B — refresh, replay on (headless) | **34.83 %** | 1.15 % | 4.03 % | 11.0 % | 102 MiB |
| B — static, replay on (headless) | 34.45 % | 0.92 % | 1.93 % | 8.9 % | 114 MiB |
| B — real app visible, replay on | 32.76 % | 1.16 % | 3.58 % | 15.2 % | 214 MiB |
| C — real app minimized, replay on | 32.36 % | 1.09 % | 2.98 % | 12.9 % | 218 MiB |
| refresh, **120 fps** | 46.99 % | 2.21 % | 4.33 % | 12.5 % (1 run) | 117 MiB |

Encoder cost vs. settings (before, 1 run each, refresh workload):

| Setting | Proc. Video Codec |
|---------|------------------:|
| 30 fps Balanced | 17.40 % |
| 60 fps Balanced | 34.74 % |
| 60 fps Performance | 34.83 % |
| 60 fps Quality (B-frames) | 65.64 % (2 frames dropped) |
| 120 fps Balanced | 45.21 % |

Frame delivery (before):

| Target | WGC frames delivered/s | Encoded/s | Repeated (duplicate) frames |
|--------|-----------------------:|----------:|----------------------------:|
| 60 fps | ~57 | 60 | 7.4 % / 3.7 % (2 runs) |
| 120 fps | ~59 | 120 | **54 % / 53 %** |

No frames were dropped, the replay buffer stayed in RAM (15–32 MB), and every save succeeded (4/4 per run).

## Conclusion

- **Root cause of the reported +30 pp.** Replay keeps the hardware H.264 encoder busy: ~33–35 % of the AMD Video Codec engine at 1080p60. That cost is intrinsic to encoding 60 frames/s. It scales linearly with fps (17 % at 30 fps) and is the same whether the app is visible or minimized. Task Manager's GPU column for the process shows exactly this engine.
- **3D cost is small.** All other GPU work (copies, colour conversion, WGC composition in DWM) adds about 1.2 % 3D in the process plus about 1 % in DWM.
- **Not the cause.** Duplicate sessions, CPU round trips, busy loops, GPUI repaints and excess capture rate (WGC capped itself at ~60/s) were each ruled out by the data above.
- **New defect found.** WGC's default update rate (~60/s) starves 120 fps capture.
