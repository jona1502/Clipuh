//! Encoder A/B benchmark on real footage: the same frames, the production
//! `PacketEncoder` path (BGRA texture -> video processor -> NV12 -> hardware
//! H.264 MFT), one H.264 setting changed at a time.
//!
//! Frames come from a raw file written by `scripts/perf/encoder_ab.py
//! extract` (u32 width, u32 height, u32 count, then count BGRA frames). They
//! are uploaded to GPU textures once, so each submitted frame costs what a
//! live capture frame costs (one copy + conversion), never an upload.
//!
//! ```text
//! cargo run --release -p clipper-encoder --example encoder_ab -- \
//!   frames=game.bgra seconds=20 profile=quality [qvs=100] [bframes=2] \
//!   [bitrate=16] [peak=2.0] [rc=vbr|cbr] [out=quality.h264]
//! ```
//! `bitrate` is the base Mbps the profile scales (Quality = 1.5x); `peak` is
//! the VBR peak as a multiple of the mean. Prints drops, bitrate and
//! submit->packet latency; `out` gets an Annex B stream of the run.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clipper_core::recording::QualityProfile;
    use clipper_encoder::backends::media_foundation::{
        EncoderConfig, MediaFoundation, SubmitOutcome,
    };
    use clipper_encoder::backends::packet_encoder::PacketEncoder;
    use clipper_encoder::codecs::{H264Tuning, RateControl};
    use clipper_platform::windows::{ComApartment, D3DContext};
    use clipper_replay::{CodecConfig, EncodedPacket, TICKS_PER_SECOND};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

    let mut frames_path = String::new();
    let mut seconds = 20u32;
    let mut fps = 60u32;
    let mut profile = QualityProfile::Quality;
    let mut base_mbps = 16u32;
    let mut qvs: Option<u32> = None;
    let mut bframes: Option<u32> = None;
    let mut peak: Option<f64> = None;
    let mut rc: Option<RateControl> = None;
    let mut out: Option<String> = None;
    let mut paced = true;
    for arg in std::env::args().skip(1) {
        let (key, value) = arg.split_once('=').ok_or("arguments are key=value")?;
        match key {
            "frames" => frames_path = value.to_owned(),
            "seconds" => seconds = value.parse()?,
            "fps" => fps = value.parse()?,
            "profile" => {
                profile = match value {
                    "performance" => QualityProfile::Performance,
                    "balanced" => QualityProfile::Balanced,
                    "quality" => QualityProfile::Quality,
                    _ => return Err("profile=performance|balanced|quality".into()),
                }
            }
            "bitrate" => base_mbps = value.parse()?,
            "qvs" => qvs = Some(value.parse()?),
            "bframes" => bframes = Some(value.parse()?),
            "peak" => peak = Some(value.parse()?),
            "rc" => {
                rc = Some(match value {
                    "cbr" => RateControl::Cbr,
                    "vbr" => RateControl::PeakConstrainedVbr,
                    _ => return Err("rc=cbr|vbr".into()),
                })
            }
            "out" => out = Some(value.to_owned()),
            // paced=0: submit as fast as the encoder accepts (frames are
            // retried, never dropped); out_fps then measures encoder time per
            // frame at full clock, independent of power-state ramping.
            "paced" => paced = value != "0",
            _ => return Err(format!("unknown argument {key}").into()),
        }
    }

    // Source frames.
    let mut file = std::fs::File::open(&frames_path)?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header)?;
    let word = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
    let (width, height, count) = (word(0), word(1), word(2) as usize);
    let frame_bytes = (width * height * 4) as usize;

    let _com = ComApartment::init_multithreaded()?;
    let mf = MediaFoundation::start()?;
    let d3d = D3DContext::new()?;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut textures = Vec::with_capacity(count);
    let mut pixels = vec![0u8; frame_bytes];
    for _ in 0..count {
        file.read_exact(&mut pixels)?;
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        // SAFETY: valid device, descriptor, initial data and out-pointer.
        unsafe {
            d3d.device()
                .CreateTexture2D(&desc, Some(&data), Some(&mut texture))?;
        }
        textures.push(texture.ok_or("texture")?);
    }
    drop(pixels);

    let config = EncoderConfig {
        input_width: width,
        input_height: height,
        output_width: width,
        output_height: height,
        fps,
        bitrate_mbps: base_mbps,
        profile,
    };
    let mut tuning = H264Tuning::new(profile, base_mbps, fps);
    if let Some(value) = qvs {
        tuning.quality_vs_speed = value;
    }
    if let Some(value) = bframes {
        tuning.b_frames = value;
    }
    if let Some(value) = rc {
        tuning.rate_control = value;
    }
    if let Some(value) = peak {
        tuning.peak_bitrate_bps = (f64::from(tuning.bitrate_bps) * value) as u32;
    }
    if tuning.rate_control == RateControl::Cbr {
        tuning.peak_bitrate_bps = tuning.bitrate_bps;
    }
    let mut encoder = PacketEncoder::create_with_tuning(&mf, &d3d, &config, &[], tuning)?;
    println!(
        "encoder {} | {count} frames {width}x{height} looped @{fps} for {seconds}s | {tuning:?}",
        encoder.name()
    );
    // Let the run start clean: the caller samples GPU counters meanwhile.
    std::thread::sleep(Duration::from_millis(500));
    println!("encoding started");

    let frame_ticks = TICKS_PER_SECOND / i64::from(fps);
    let mut packets: Vec<EncodedPacket> = Vec::new();
    let mut submitted_at: HashMap<i64, Instant> = HashMap::new();
    let mut latency_ms: Vec<f64> = Vec::new();
    let mut dropped = 0u32;
    let start = Instant::now();
    let total = seconds * fps;
    let mut collect = |encoder: &mut PacketEncoder,
                       packets: &mut Vec<EncodedPacket>,
                       submitted_at: &mut HashMap<i64, Instant>|
     -> Result<(), Box<dyn std::error::Error>> {
        let before = packets.len();
        encoder.poll(packets)?;
        let now = Instant::now();
        for packet in &packets[before..] {
            if packet.track == 0
                && let Some(at) = submitted_at.remove(&packet.pts)
            {
                latency_ms.push(now.duration_since(at).as_secs_f64() * 1000.0);
            }
        }
        Ok(())
    };
    for index in 0..total {
        let time = i64::from(index) * frame_ticks;
        let texture = &textures[index as usize % count];
        loop {
            match encoder.submit(texture, (width, height), time, frame_ticks)? {
                SubmitOutcome::Submitted => {
                    submitted_at.insert(time, Instant::now());
                    break;
                }
                SubmitOutcome::Dropped if paced => {
                    dropped += 1;
                    break;
                }
                SubmitOutcome::Dropped => {
                    collect(&mut encoder, &mut packets, &mut submitted_at)?;
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        }
        collect(&mut encoder, &mut packets, &mut submitted_at)?;
        if !paced {
            continue;
        }
        // Real-time pacing like a live capture; poll while waiting so
        // latency is measured at ~1 ms resolution.
        let due =
            start + Duration::from_nanos(((u64::from(index) + 1) * 1_000_000_000) / u64::from(fps));
        while Instant::now() < due {
            std::thread::sleep(Duration::from_millis(1));
            collect(&mut encoder, &mut packets, &mut submitted_at)?;
        }
    }
    println!("encoding finished");
    let layout = encoder.layout().ok_or("layout never became known")?;
    encoder.finish(&mut packets)?;
    let wall = start.elapsed().as_secs_f64();

    let video: Vec<&EncodedPacket> = packets.iter().filter(|p| p.track == 0).collect();
    let bytes: usize = video.iter().map(|p| p.data.len()).sum();
    latency_ms.sort_by(f64::total_cmp);
    let pct = |p: f64| {
        latency_ms
            .get(((latency_ms.len() as f64 - 1.0) * p) as usize)
            .copied()
            .unwrap_or(f64::NAN)
    };
    println!(
        "RESULT frames={} dropped={dropped} out_fps={:.2} bitrate_mbps={:.2} latency_ms p50={:.1} p95={:.1} p99={:.1}",
        video.len(),
        video.len() as f64 / wall,
        bytes as f64 * 8.0 / f64::from(seconds) / 1e6,
        pct(0.5),
        pct(0.95),
        pct(0.99),
    );

    if let Some(path) = out {
        let Some(CodecConfig::H264 { sps, pps, .. }) = layout.video().map(|t| t.codec.clone())
        else {
            return Err("no video track".into());
        };
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        let start_code = [0u8, 0, 0, 1];
        for packet in &video {
            if packet.keyframe {
                for set in [&sps, &pps] {
                    file.write_all(&start_code)?;
                    file.write_all(set)?;
                }
            }
            // AVCC (4-byte lengths) -> Annex B.
            let mut data = &packet.data[..];
            while data.len() >= 4 {
                let len = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
                let nal = data.get(4..4 + len).ok_or("truncated NAL")?;
                file.write_all(&start_code)?;
                file.write_all(nal)?;
                data = &data[4 + len..];
            }
        }
        file.flush()?;
        println!("wrote {path}");
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("encoder_ab needs Windows");
}
