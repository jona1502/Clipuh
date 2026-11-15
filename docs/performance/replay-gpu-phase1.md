# Replay GPU — Phase 1: AMD H.264 encoder efficiency beyond PR #50

Measured 2026-11-15 on current `main` (PR #50 merged). Historical numbers
from PR #50 stay in `replay-gpu-load.md`; everything below is new.

## Result

**No further quality-equivalent saving exists in the Media Foundation
encoder's settings on this hardware.** The merged AMD Quality configuration
(QvS 50, no B-frames, 1.25× bitrate) already runs at the encoder's floor,
about **1.58 ms per 1080p frame** at full engine clock. Of every supported
knob, only CAVLC was measurably faster (−3.4 to −4.1 %), and it lost quality
and grew files, so it was rejected. **Production encoder settings are
unchanged.** This PR adds the measurement tooling and this report.

The 30–40 % seen in Task Manager is explained below: it is the system-wide
Video Codec engine including other processes (Discord ~7 % here), and a
percentage of the engine's *current* clock, which power management keeps low
at this load.

## Hardware and encoder

- AMD Radeon RX 9070 XT, driver 32.0.31041.1004 (2026-08-17), Windows 11.
- Media Foundation hardware MFT "AMDh264Encoder", vendor `VEN_1002` (listed
  twice by `MFTEnumEx`, identical capabilities).
- Background engine users during the runs: Discord (two processes, ~4.4 % +
  2.8 % of the Video Codec engine), DWM 3D ~0.3 %. Nothing was closed;
  runs were interleaved so both sides saw the same background.

## What the encoder reports (`examples/codec_caps.rs`)

`ICodecAPI` reports every property below as **supported but not
modifiable**, and its ranges do not match its behaviour (QvS "50..50", rate
control "3..3"), so the reported ranges cannot be trusted. Each knob was
therefore checked in the real output.

| Property | Reported | Verified in the output |
|---|---|---|
| QualityVsSpeed | supported | 0, 25 and 50 give **byte-identical** streams; 100 is the slow preset (PR #50) |
| MaxNumRefFrame (default 4) | 1..16 | SPS `max_num_ref_frames` follows (1, 2) |
| H264CABACEnable | supported | PPS `entropy_coding_mode_flag` follows (CAVLC/CABAC) |
| SliceControlMode/Size | 0..2 | 2 and 4 slices per frame |
| BPictureCount | 0..16 | real B-frames (PR #50) |
| RateControlMode | supported | CBR vs peak-constrained VBR both work |
| MPVProfile (Main/High) | supported | follows |
| Temporal layers, LTR, QP limits, Usage | supported | not tested: no efficiency hypothesis |
| SceneDetection, GOPOpen, NumWorkerThreads, MaxKeyframeDistance, ContentType, RealTime, MultipassMode | **unsupported** | — |

Not tested on purpose:
- **Low-latency mode**: starves the encoder's input pool on this MFT
  (documented in `H264Tuning::low_latency`).
- **Quality/CQP rate control**: no bitrate bound, which the replay buffer
  and storage estimate need.
- **Longer GOP**: replay clips start at keyframes; 2 s is the clip
  precision.

## Method

Same as PR #50 (`encoder_ab.rs` + `scripts/perf/encoder_ab.py`), extended:

- **Knobs**: `encoder_ab` takes `refs=`, `cabac=`, `slice_rows=`, `gop=`,
  `h264=` besides `qvs=`, `bframes=`, `bitrate=`, `peak=`, `rc=`. The
  matching optional fields on `H264Tuning` are `None` in every profile, so the
  app sends exactly the properties it sent before (unit test
  `optional_knobs_are_off_unless_set`).
- **Bitstream check**: `encoder_ab.py bitstream` parses SPS/PPS/slice headers
  (reference count, entropy coder, slices per frame, slice types, IDR
  spacing).
- **Encoder time**: `paced=0` (as fast as the encoder accepts, engine at
  full clock), 600 frames, 3 interleaved runs, median.
- **Quality**: luma SSIM and PSNR of every 3rd of 240 frames against the
  source frames; mean, **5th percentile** and minimum, so bad frames count.
  **VMAF not available** (no libvmaf).
- **Footage**, 1920×1080, 240 frames each, from local War Thunder clips:
  - `game`: in battle, foliage and snow, camera motion;
  - `game2`: a later segment of the same battle;
  - `menu`: hangar and UI text;
  - `dark`: `game` darkened to 25 %, a synthetic stand-in for dark scenes.

  The sources are 30 fps clips fed at 60 fps, so motion per frame is doubled;
  there is **no native 60 fps or particle-heavy footage** on this machine.

**Baseline** = merged AMD Quality: QvS 50, B 0, base 16 → 30 Mbit/s mean,
60 Mbit/s peak.

## Stage 1 — encoder time (game footage, median of 3)

| Candidate | ms/frame | vs baseline | Mbit/s |
|---|---:|---:|---:|
| **Baseline** | 1.575–1.584 | — | 32.84 |
| QvS 0 | 1.583 | 0.0 % | 32.84 (identical stream) |
| QvS 25 | 1.591 | +0.4 % | 32.84 (identical stream) |
| 1 reference frame | 1.569 | −1.0 % | 32.84 |
| 2 reference frames | 1.590 | +0.4 % | 32.84 |
| 2 slices | 1.585 | +0.1 % | 32.86 |
| 4 slices | 1.595 | +0.7 % | 32.80 |
| H.264 Main | 1.590 | +1.0 % | 32.86 |
| CBR | 1.574 | −0.1 % | 30.41 |
| **CAVLC** | 1.518–1.521 | **−3.4 to −4.1 %** | 32.84 |
| Main + CAVLC | 1.524 | −3.3 % | 32.84 |

Run-to-run spread is about ±2 %. Only CAVLC is outside it.

## Stage 3 — quality of the candidates that differ

| Scene | Variant | Mbit/s | SSIM mean | SSIM p5 | PSNR mean | PSNR p5 | PSNR min |
|---|---|---:|---:|---:|---:|---:|---:|
| game | baseline | 33.41 | 0.99167 | 0.98930 | 42.33 | 41.52 | 40.07 |
| game | CAVLC | 33.71 | 0.98905 | 0.98159 | 41.71 | 40.03 | 39.57 |
| game | 1 ref | 33.41 | 0.99167 | 0.98930 | 42.33 | 41.52 | 40.07 |
| game2 | baseline | 23.41 | 0.99298 | 0.98947 | 42.31 | 41.20 | 40.74 |
| game2 | CAVLC | 24.44 | 0.99208 | 0.98730 | 42.03 | 40.62 | 39.02 |
| game2 | 1 ref | 23.41 | 0.99298 | 0.98947 | 42.31 | 41.20 | 40.74 |
| menu | baseline | 16.50 | 0.99224 | 0.99160 | 42.67 | 42.34 | 40.65 |
| menu | CAVLC | 17.98 | 0.99223 | 0.99161 | 42.67 | 42.35 | 40.63 |
| menu | 1 ref | 16.50 | 0.99224 | 0.99160 | 42.67 | 42.34 | 40.65 |
| dark | baseline | 17.07 | 0.99301 | 0.99213 | 45.97 | 45.76 | 43.49 |
| dark | CAVLC | 19.51 | 0.99287 | 0.99184 | 45.95 | 45.62 | 43.39 |
| dark | 1 ref | 17.07 | 0.99301 | 0.99213 | 45.97 | 45.76 | 43.49 |

### Rejected

- **CAVLC**: −4 % encoder time, but −0.6 dB mean and −1.5 dB on the worst 5 %
  of gameplay frames, and +9 % (menu) to +14 % (dark) bitrate. Matching
  quality would need more bitrate, i.e. larger files, for a 4 % gain.
- **1 reference frame**: identical quality, no reliable speed gain (−1 % is
  inside the noise). The stream differs from the baseline, yet every metric is
  the same to 5 decimals: the encoder appears to use one reference anyway.
  Nothing to gain.
- **QvS < 50**: identical output; there is no faster preset.
- **Slices, Main profile, CBR**: no speed gain.

## Comparison table

| Metric | Current Quality | Best candidate (CAVLC, rejected) | Difference |
|--------|-----------------|-----------|------------|
| Encoder time/frame | 1.575 ms | 1.521 ms | −3.4 % |
| Max encode FPS | 635 | 657 | +3.5 % |
| Video Codec utilisation (live) | 21.8–22.4 % (process) | not run (rejected) | N/A |
| Output bitrate (game / menu / dark) | 33.4 / 16.5 / 17.1 Mbit/s | 33.7 / 18.0 / 19.5 | +1 / +9 / +14 % |
| File size/min (game) | ~250 MB | ~253 MB | +1 % |
| SSIM (game, mean / p5) | 0.99167 / 0.98930 | 0.98905 / 0.98159 | worse |
| PSNR (game, mean / p5) | 42.33 / 41.52 dB | 41.71 / 40.03 dB | −0.62 / −1.49 dB |
| VMAF | N/A | N/A | not installed |
| Frame drops (bench) | 0 | 0 | — |
| Output validity | verified (bitstream, decode) | verified | — |
| Gaming FPS | N/A | N/A | not measured |

## Live replay on current main (Quality, 1080p60)

`replay_smoke run=32 duration=30 fps=60 desktop=1 mic=0 profile=quality`,
3 runs, capturing the 2560×1440 desktop scaled to 1080p.

| Run | Video Codec (process) | Video Codec (system) | 3D (process) | Encoded fps | Drops | Saves |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 22.41 % | 30.28 % | 0.62 % | 60.0 | 0 | 4/4 |
| 2 | 21.87 % | 29.16 % | 0.50 % | 57.7* | 0 | 4/4 |
| 3 | 21.84 % | 30.08 % | 0.54 % | 62.0* | 0 | 4/4 |

\* the status line's 1-second rate; the encoded frame counts (1869–1928
frames over ~31 s) are 60 fps.

### Why ~22 % for ~1.6 ms of work per frame

At full clock, 60 frames × 1.58 ms = 9.5 % of the engine. Utilisation is
busy time at the engine's **current** clock. At this light, steady load the
GPU's power management runs the video engine slowly, so the same work fills
~22 % of the time. The PR #50 live runs read 11.4 % while a browser was
decoding video, because the extra load clocked the engine up. The work per
frame is the same in both cases; only the clock differs.

Task Manager's "Video Codec" graph is the whole engine for all processes
(30 % here, with Discord), and the process "GPU" column shows the busiest
engine.

## Gaming impact

Not measured: no repeatable game benchmark was available. The procedure is
in `replay-gpu-load.md` (game only, replay off, replay on; avg fps, 1 % low,
P99 frametime with PresentMon/CapFrameX).

## Recommended next steps (outside Media Foundation tuning)

1. **Native AMF backend.** AMF exposes presets, pre-analysis and VBAQ that
   Media Foundation does not. It could lower encoder time or improve quality
   per bit, but it is a new backend.
2. **HEVC or AV1 on VCN.** Better quality per bit; encoder cost has to be
   measured. It changes the codec, so it is a product decision.
3. **Size estimate fix** for AMD Quality (still open from PR #50).
4. **Native 60 fps, dark and particle footage** for future quality tests.

## Reproduce

```text
cargo run --release -p clipper-encoder --example codec_caps
python scripts/perf/encoder_ab.py extract <clip.mp4> 300 240 game.bgra [0.25]
encoder_ab.exe frames=game.bgra profile=quality qvs=50 bframes=0 bitrate=20 peak=2.0 cabac=0 seconds=10 paced=0
encoder_ab.exe ... seconds=4 paced=0 out=cavlc.h264
python scripts/perf/encoder_ab.py bitstream cavlc.h264
python scripts/perf/encoder_ab.py quality game.bgra cavlc.h264 240 3
python scripts/perf/encoder_ab.py live replay_smoke.exe label run=32 duration=30 fps=60 desktop=1 mic=0 profile=quality
```
