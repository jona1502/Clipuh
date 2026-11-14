# Instant Replay GPU load (AMD) — investigation and fix

Follow-up to Phase 10A (`replay-baseline.md`, `replay-optimization.md`).
Hardware: AMD Radeon RX 9070 XT (VCN 5.0), Windows 11, "AMDh264Encoder"
Media Foundation MFT. Release builds. Measured 2026-11-14.

## Summary

- **Root cause:** the `Quality` profile's encoder settings, not the capture
  pipeline. Quality (QvS 100, 2 B-frames, 1.5× bitrate) costs **2.26×** the
  encoder time per 1080p frame of Balanced (4.04 ms vs 1.79 ms on the AMD
  Video Codec engine at full clock). The preset (QvS 100) and the B-frames
  are each about half of the extra; bitrate costs nothing.
- **Fix:** on AMD only, Quality now spends **bits instead of encoder time**:
  QvS 50, no B-frames, 1.25× the Quality bitrate. On identical real game
  frames this scores **higher** SSIM/PSNR than the old Quality (+0.21 dB) for
  **55 % less encoder time**. Cost: ~+20–30 % file size on hard content.
- **Live replay, Quality 1080p60:** process Video Codec utilisation
  **21.3 % → 11.4 %** (−47 %), 0 drops, 60.0 fps, all saves OK.
- Other vendors (NVIDIA, Intel) and other profiles are unchanged; they were
  not measured.

## Pipeline and avoidable work

Capture (WGC, BGRA) → one GPU copy into the converter's staging texture →
D3D11 video processor (BGRA → NV12, BT.709 limited, scaling if needed) → NV12
pool (4 textures) → hardware H.264 MFT (async) → AVCC packets → RAM/SSD
replay buffer. `crates/clipper-encoder/src/backends/packet_encoder.rs`
(`PacketEncoder::submit`, `Converter`, `VideoStage::configure`),
`crates/clipper-recorder/src/replay_session.rs` (pacing, repeats).

Phase 10A already removed the duplicate full-size copy and the duplicate
frame submissions (repeats 7 % → 0.1 %). Re-checked here: one capture
session, one encoder, 60.0 encoded fps, process 3D ≈ 0.5–0.6 %, Copy engine
0 %. The video processor and copy run on the 3D engine and are below 1 %.
There is no remaining avoidable work of a size that matters; the encoder
settings were the avoidable cost.

## Method

`crates/clipper-encoder/examples/encoder_ab.rs` feeds the **same frames**
through the production `PacketEncoder` path (frames pre-uploaded to GPU
textures, so each submit costs one copy + conversion like a capture frame),
with one H.264 setting changed at a time. `scripts/perf/encoder_ab.py`
extracts frames from a clip, samples the GPU engine counters
(`\GPU Engine(*)\Utilization Percentage`, summed per engine type for the
process, via `typeperf`) and scores quality.

- **Sources:** 240 frames each, 1920×1080, from real War Thunder clips:
  `game` (in battle: snow, dense foliage, HUD) and `menu` (hangar + UI text).
- **Throughput mode** (`paced=0`): encodes as fast as the engine accepts.
  The engine runs at full clock, so ms/frame is the encoder's real cost and
  does not depend on power-state ramping. 3 interleaved runs, very stable
  (±1 %).
- **Paced mode**: real-time 60 fps, utilisation sampled once per second for
  15 s after a 3 s warm-up.
- **Quality**: luma (BT.709) SSIM at full resolution and luma PSNR of every
  3rd of the 240 decoded frames against the source frames (OpenCV decode,
  scikit-image SSIM). Both sides go through the same decoder, so the
  comparison between variants is fair. **VMAF was not available** (no
  ffmpeg/libvmaf on this machine).

Caveat on utilisation %: it is busy time at the engine's *current* clock.
When other processes use the engine (a browser playing video used ~22 % of it
during later runs) the clock rises and every percentage drops. Compare only
within one interleaved batch; ms/frame (throughput mode) is the robust
number.

## Encoder parameter isolation (1080p60, game frames)

| Variant | QvS | B | Mean target | Encoder ms/frame | Paced util. (quiet) | Latency p50 |
|---|---:|---:|---:|---:|---:|---:|
| Balanced | 50 | 0 | 16 Mbit/s | **1.79** | 22.8 % | 4.7 ms |
| Quality, QvS 50, B 0 | 50 | 0 | 24 | 1.82 | — | 4.4 ms |
| Balanced + B 2 | 50 | 2 | 16 | 2.97 | — | 25 ms |
| Quality, QvS 50 | 50 | 2 | 24 | 3.00 | 31.1 % | 29 ms |
| Balanced, QvS 100 | 100 | 0 | 16 | 3.44 | 32.6 % | 7.2 ms |
| Quality, B 0 | 100 | 0 | 24 | 3.45 | 32.6 % | 6.9 ms |
| **Quality (old)** | 100 | 2 | 24 | **4.04** | 31.7 % | 29 ms |

- Bitrate: no encoder cost (1.79 vs 1.82 ms at 16 vs 24 Mbit/s).
- QvS 50 → 100: +1.65 ms/frame. B-frames: +0.6–1.2 ms/frame and +20 ms
  latency. Together 2.26×.
- The B-frames are real: the old Quality stream has I 10 / P 400 / B 790
  slices (parsed from the bitstream). The VBR peak (1.5× vs 2×) changed
  nothing (31.8 % vs 31.7 %).
- In paced mode, Quality saturates differently than throughput suggests
  (31 % vs 22.8 %) because the engine clocks up under the heavier load; the
  throughput numbers are the cost.

## Quality: same frames, every variant

| Variant | game SSIM | game PSNR | game Mbit/s | menu SSIM | menu PSNR | menu Mbit/s | ms/frame |
|---|---:|---:|---:|---:|---:|---:|---:|
| **Quality (old)** | 0.99100 | 42.12 | 25.8 | 0.99204 | 42.58 | 14.0 | 4.04 |
| QvS 100, B 0 | 0.98957 | 41.76 | 27.1 | 0.99217 | 42.62 | 15.0 | 3.45 |
| QvS 50, B 2 | 0.99042 | 41.97 | 25.8 | 0.99195 | 42.55 | 14.3 | 3.00 |
| QvS 50, B 0 | 0.98879 | 41.59 | 27.2 | 0.99206 | 42.59 | 15.2 | 1.82 |
| **QvS 50, B 0, 1.25× (new)** | **0.99167** | **42.33** | 33.4 | **0.99224** | **42.67** | 16.5 | **1.83** |
| QvS 50, B 0, 1.5× | 0.99283 | 42.64 | 39.6 | 0.99242 | 42.72 | 18.5 | 1.92 |
| Balanced | 0.98265 | 40.32 | 18.4 | 0.98958 | 41.76 | 8.6 | 1.79 |

On the hard gameplay scene the expensive tools are worth about +0.5 dB at a
fixed bitrate; 25 % more bits on the fast path are worth +0.74 dB. On the UI
scene every Quality-class variant is within 0.15 dB. The new mapping is at or
above the old Quality on both scenes; its per-frame minima are equal (the
minimum is the stream's first frame in every variant).

Scenes covered: fast motion (foliage, camera pan at 2× speed because the
30 fps sources were fed at 60 fps), high detail, HUD and small text, a
low-motion menu. **Not covered**: dark scenes and particle-heavy effects (no
such footage on this machine). Metrics are not a substitute for watching;
no side-by-side visual review was done.

## Live replay before/after (Quality, 1080p60)

`replay_smoke run=32 duration=30 fps=60 desktop=1 mic=0 profile=quality`,
capturing the 2560×1440 desktop (scaled to 1080p) while a browser video
played. `origin/main` vs this branch, alternating, 2 runs each.

| Metric | Old Quality | New Quality | Difference |
|--------|------------:|------------:|-----------:|
| Video Codec GPU (process) | 21.36 / 21.26 % | 11.38 / 11.37 % | **−9.9 pp (−47 %)** |
| Video Codec GPU (system) | 43.9 / 43.6 % | 34.1 / 34.0 % | −9.7 pp |
| GPU 3D (process) | 0.56 % | 0.60 % | noise |
| GPU Copy (process) | 0 | 0 | — |
| CPU (process, s over ~45 s) | 2.05 / 1.64 | 2.09 / 2.03 | noise |
| RAM working set / private at end (MiB) | 59.8 / 94.9 | 50.0 / 81.8 | −10 / −13 |
| Encoded fps / dropped | 60.4 / 0 | 60.0 / 0 | — |
| Encoder latency p50 (bench) | 29 ms | 4.4 ms | −25 ms |
| Saves (burst of 3 + 1) | 4/4 | 4/4 | — |
| Clip (30 s) | 1801 frames, 60.000 fps, 0 errors, 9.6 MB | 1801 frames, 60.000 fps, 0 errors, 9.5 MB | — |
| Clip timestamps (probe) | −33…+67 ms, 615 irregular | 16.67 ms, 0 irregular | B-frame reordering gone |
| SSIM / PSNR (bench, game) | 0.99100 / 42.12 | 0.99167 / 42.33 | +0.0007 / +0.21 dB |
| Gaming FPS / 1 % low | N/A | N/A | not measured |

The live percentages are lower than in the quiet bench because the browser's
decode raised the engine clock; the relative drop is what matters.

## A / B / C

- **A. Avoidable work** (removed): the AMD Quality preset + B-frames, ~2.2 ms
  of encoder time per frame (≈13 % of the engine at full clock at 60 fps).
- **B. Necessary work**: ~1.8 ms per 1080p frame on this VCN, i.e. ~11 % of
  the engine at full clock at 60 fps; it shows as 11–23 % depending on the
  engine's clock state. It scales with fps (30 fps halves it) and resolution.
  This is the floor for hardware H.264 at 1080p60 here.
- **C. Reporting vs gaming cost**: the Video Codec engine is a fixed-function
  block separate from the shader cores (3D engine use by replay stays
  ~0.5 %). A high percentage there does not map 1:1 to game fps. It still
  shares memory bandwidth and the board power budget, so it is not free.
  **Gaming fps was not measured** (no repeatable game benchmark available in
  this session).

## The ~66 % from 10A

Not reproduced. With the same settings, the quiet bench measured 31.7 %
(1080p source) and the live replay 21.3 % under contention. Utilisation % is
clock-relative, so the earlier number most likely came from a lower engine
clock state. The cost ratio (Quality 2.26× Balanced) explains why Quality
looked roughly twice as heavy.

## Implementation

`H264Tuning::for_vendor` (`crates/clipper-encoder/src/codecs/mod.rs`) maps
`Quality` on vendor `VEN_1002` (AMD) to QvS 50, 0 B-frames and 1.25× the
Quality bitrate (peak 2× the mean, capped at 150 Mbit/s). `PacketEncoder`
reads `MFT_ENUM_HARDWARE_VENDOR_ID_Attribute` of each candidate encoder and
applies it per candidate, so a non-AMD encoder in the same system keeps the
profile. `PacketEncoder::create_with_tuning` takes an explicit tuning for
benchmarks; the app always goes through the profile.

## Limitations and follow-ups

1. **Manual recordings** (`HardwareEncoder`, sink writer) still use the old
   Quality settings: the sink writer picks its MFT internally, so the vendor
   is only known after the properties are set. Next step: resolve the vendor
   up front from the sorted hardware encoder list and apply `for_vendor`
   there too.
2. **Size estimate**: the storage estimate and the Quality menu's "24 Mbps
   VBR" detail use the profile bitrate, so on AMD they under-state Quality by
   up to 25 %. They should take the vendor tuning into account.
3. NVIDIA/Intel were not measured; their Quality is unchanged on purpose.
4. Not measured: dark/particle-heavy footage, VMAF, gaming fps and 1 % lows,
   app minimized, high-DPI. Gaming procedure: same game benchmark run 3× each
   with (1) no clipuh, (2) replay off, (3) old Quality, (4) new Quality;
   record avg fps, 1 % low and P99 frametime (PresentMon or CapFrameX), plus
   GPU 3D and Video Codec utilisation.
5. Further options (not done, each needs its own quality check): HEVC/AV1 on
   VCN (better quality per bit, different encoder cost); 30 fps for users
   who accept it (a quality trade, already in Settings).

## Reproduce

```text
python scripts/perf/encoder_ab.py extract <clip.mp4> 300 240 game.bgra
cargo build --release -p clipper-encoder --example encoder_ab
target/release/examples/encoder_ab.exe frames=game.bgra profile=quality seconds=10 paced=0
target/release/examples/encoder_ab.exe frames=game.bgra profile=quality qvs=50 bframes=0 bitrate=20 seconds=4 paced=0 out=new.h264
python scripts/perf/encoder_ab.py quality game.bgra new.h264 240 3
python scripts/perf/encoder_ab.py run <encoder_ab.exe> game.bgra quality profile=quality seconds=20
python scripts/perf/encoder_ab.py live <replay_smoke.exe> label run=32 duration=30 fps=60 desktop=1 mic=0 profile=quality
```
