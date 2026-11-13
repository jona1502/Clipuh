//! Encodes synthetic video + a 440 Hz sine into replay packets and prints
//! statistics.
//!
//! Usage:
//!   cargo run -p clipper-encoder --example replay_packets_smoke --release -- \
//!     [seconds=5] [fps=60] [in=1920x1080] [out=1920x1080] [profile=balanced] \
//!     [realtime=0] [dump=<path>]
//!
//! `realtime=1` paces submissions like a live capture (frames that meet
//! backpressure are dropped); otherwise frames are retried until accepted so
//! the encode throughput can be measured.
//!
//! Dump file format (little endian):
//!   header: magic "CLPK" u32 version=1, u32 width, u32 height,
//!           u32 sps_len, sps, u32 pps_len, pps,
//!           u8 audio_tracks, per audio track: u32 asc_len, asc
//!   then packets until EOF: u8 track, u8 keyframe, i64 pts, i64 dts,
//!           i64 duration, u32 len, data
//!   (video data is AVCC, audio data raw AAC access units)
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clipper_core::recording::QualityProfile;
    use clipper_encoder::backends::media_foundation::{
        EncoderConfig, MediaFoundation, SubmitOutcome,
    };
    use clipper_encoder::backends::packet_encoder::PacketEncoder;
    use clipper_platform::windows::{ComApartment, D3DContext};
    use clipper_replay::{CodecConfig, EncodedPacket, TICKS_PER_SECOND};
    use std::io::Write;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::core::Interface;

    let mut seconds = 5u32;
    let mut fps = 60u32;
    let mut input = (1920u32, 1080u32);
    let mut output = (1920u32, 1080u32);
    let mut profile = QualityProfile::Balanced;
    let mut realtime = false;
    let mut dump: Option<String> = None;
    let parse_size = |value: &str| -> Option<(u32, u32)> {
        let (w, h) = value.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    };
    for arg in std::env::args().skip(1) {
        let (key, value) = arg.split_once('=').ok_or("arguments are key=value")?;
        match key {
            "seconds" => seconds = value.parse()?,
            "fps" => fps = value.parse()?,
            "in" => input = parse_size(value).ok_or("in=WxH")?,
            "out" => output = parse_size(value).ok_or("out=WxH")?,
            "profile" => {
                profile = match value {
                    "performance" => QualityProfile::Performance,
                    "balanced" => QualityProfile::Balanced,
                    "quality" => QualityProfile::Quality,
                    _ => return Err("profile=performance|balanced|quality".into()),
                }
            }
            "realtime" => realtime = value != "0",
            "dump" => dump = Some(value.to_owned()),
            _ => return Err(format!("unknown argument {key}").into()),
        }
    }

    let _com = ComApartment::init_multithreaded()?;
    let mf = MediaFoundation::start()?;
    let d3d = D3DContext::new()?;
    let config = EncoderConfig {
        input_width: input.0,
        input_height: input.1,
        output_width: output.0,
        output_height: output.1,
        fps,
        bitrate_mbps: 16,
        profile,
    };
    let mut encoder = PacketEncoder::create(&mf, &d3d, &config, 1)?;
    println!(
        "encoder: {}  {}x{} -> {}x{} @{fps} {profile:?}",
        encoder.name(),
        input.0,
        input.1,
        output.0,
        output.1
    );

    let desc = D3D11_TEXTURE2D_DESC {
        Width: input.0,
        Height: input.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    let mut view = None;
    // SAFETY: valid device, descriptor and out-pointers.
    unsafe {
        d3d.device()
            .CreateTexture2D(&desc, None, Some(&mut texture))?;
        d3d.device().CreateRenderTargetView(
            texture.as_ref().ok_or("texture")?,
            None,
            Some(&mut view),
        )?;
    }
    let texture = texture.ok_or("texture")?;
    let view: ID3D11RenderTargetView = view.ok_or("view")?;
    let context1: Option<ID3D11DeviceContext1> = d3d.context().cast().ok();

    let frame_ticks = TICKS_PER_SECOND / i64::from(fps);
    let mut packets: Vec<EncodedPacket> = Vec::new();
    let mut dropped = 0u32;
    let mut retries = 0u64;
    let mut audio_time = 0i64;
    let start = Instant::now();
    for index in 0..seconds * fps {
        let t = (index % 90) as f32 / 90.0;
        // SAFETY: valid view on the shared device; rects lie inside the target.
        unsafe {
            d3d.context()
                .ClearRenderTargetView(&view, &[t, 0.2, 1.0 - t, 1.0]);
            if let Some(context) = &context1 {
                for block in 0..12u32 {
                    let x = ((index * (3 + block * 2) + block * 97) % (input.0 - 120)) as i32;
                    let y = ((index * (2 + block) + block * 61) % (input.1 - 120)) as i32;
                    let rect = RECT {
                        left: x,
                        top: y,
                        right: x + 120,
                        bottom: y + 120,
                    };
                    let color = [(block as f32) / 12.0, 1.0 - t, t, 1.0];
                    context.ClearView(&view, &color, Some(&[rect]));
                }
            }
        }
        let time = i64::from(index) * frame_ticks;
        while audio_time <= time {
            let n0 = (audio_time * 48_000 / TICKS_PER_SECOND) as usize;
            let chunk: Vec<f32> = (0..480)
                .flat_map(|i| {
                    let v = (2.0 * std::f32::consts::PI * 440.0 * (n0 + i) as f32 / 48_000.0).sin()
                        * 0.5;
                    [v, v]
                })
                .collect();
            encoder.submit_audio(0, &chunk, audio_time)?;
            audio_time += 100_000;
        }
        loop {
            match encoder.submit(&texture, input, time, frame_ticks)? {
                SubmitOutcome::Submitted => break,
                SubmitOutcome::Dropped if realtime => {
                    dropped += 1;
                    break;
                }
                SubmitOutcome::Dropped => {
                    retries += 1;
                    encoder.poll(&mut packets)?;
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        }
        encoder.poll(&mut packets)?;
        if realtime {
            let due =
                start + Duration::from_nanos(((index as u64 + 1) * 1_000_000_000) / u64::from(fps));
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
    }
    let layout = encoder.layout().ok_or("layout never became known")?;
    encoder.finish(&mut packets)?;
    let elapsed = start.elapsed().as_secs_f64();

    let video: Vec<&EncodedPacket> = packets.iter().filter(|p| p.track == 0).collect();
    let audio: Vec<&EncodedPacket> = packets.iter().filter(|p| p.track == 1).collect();
    let video_bytes: usize = video.iter().map(|p| p.data.len()).sum();
    let audio_bytes: usize = audio.iter().map(|p| p.data.len()).sum();
    let keys: Vec<i64> = video.iter().filter(|p| p.keyframe).map(|p| p.pts).collect();
    let key_gaps: Vec<f64> = keys
        .windows(2)
        .map(|w| (w[1] - w[0]) as f64 / TICKS_PER_SECOND as f64)
        .collect();
    let reordered = video.windows(2).any(|w| w[1].pts < w[0].pts);
    println!("video packets {} bytes {video_bytes}", video.len());
    println!("audio packets {} bytes {audio_bytes}", audio.len());
    println!("keyframes {} gaps(s) {key_gaps:.3?}", keys.len());
    println!(
        "avg video bitrate {:.2} Mbit/s over {seconds} s of content",
        video_bytes as f64 * 8.0 / f64::from(seconds) / 1e6
    );
    println!("B-frame reordering seen: {reordered}");
    println!(
        "wall {elapsed:.2} s -> {:.1} frames/s, dropped {dropped}, backpressure retries {retries}",
        f64::from(seconds * fps) / elapsed
    );

    if let Some(path) = dump {
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        let Some(CodecConfig::H264 {
            width,
            height,
            sps,
            pps,
        }) = layout.video().map(|t| t.codec.clone())
        else {
            return Err("no video track".into());
        };
        file.write_all(b"CLPK")?;
        file.write_all(&1u32.to_le_bytes())?;
        file.write_all(&width.to_le_bytes())?;
        file.write_all(&height.to_le_bytes())?;
        file.write_all(&(sps.len() as u32).to_le_bytes())?;
        file.write_all(&sps)?;
        file.write_all(&(pps.len() as u32).to_le_bytes())?;
        file.write_all(&pps)?;
        file.write_all(&[layout.audio_tracks() as u8])?;
        for track in layout.tracks.iter().skip(1) {
            if let CodecConfig::Aac {
                audio_specific_config,
                ..
            } = &track.codec
            {
                file.write_all(&(audio_specific_config.len() as u32).to_le_bytes())?;
                file.write_all(audio_specific_config)?;
            }
        }
        for packet in &packets {
            file.write_all(&[packet.track, u8::from(packet.keyframe)])?;
            file.write_all(&packet.pts.to_le_bytes())?;
            file.write_all(&packet.dts.to_le_bytes())?;
            file.write_all(&packet.duration.to_le_bytes())?;
            file.write_all(&(packet.data.len() as u32).to_le_bytes())?;
            file.write_all(&packet.data)?;
        }
        file.flush()?;
        println!("dumped {} packets to {path}", packets.len());
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}
