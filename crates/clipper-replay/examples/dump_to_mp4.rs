//! Feeds a packet dump (from `clipper-encoder`'s `replay_packets_smoke
//! dump=<path>`) through the real `ReplayBuffer` (optionally with the SSD
//! tier) and exports the last `seconds` as MP4.
//!
//! Usage: cargo run -p clipper-replay --example dump_to_mp4 -- <dump.bin> <out.mp4>
//!        [seconds=30] [ram_mb=64] [disk_root=<dir>]

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clipper_replay::disk::DiskConfig;
use clipper_replay::export::{ExportJob, export_clip};
use clipper_replay::hybrid::{ReplayBuffer, ReplayConfig};
use clipper_replay::{CodecConfig, EncodedPacket, StreamLayout, TrackInfo};

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if self.0.len() < n {
            return Err("truncated dump".into());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let len = self.u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(input), Some(output)) = (args.first(), args.get(1)) else {
        return Err(
            "usage: dump_to_mp4 <dump.bin> <out.mp4> [seconds=30] [ram_mb=64] [disk_root=<dir>]"
                .into(),
        );
    };
    let mut seconds = 30u64;
    let mut ram_mb = 64usize;
    let mut disk_root = None;
    for arg in &args[2..] {
        match arg.split_once('=') {
            Some(("seconds", v)) => seconds = v.parse()?,
            Some(("ram_mb", v)) => ram_mb = v.parse()?,
            Some(("disk_root", v)) => disk_root = Some(std::path::PathBuf::from(v)),
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }

    let mut raw = Vec::new();
    std::fs::File::open(input)?.read_to_end(&mut raw)?;
    let mut cur = Cursor(&raw);
    if cur.take(4)? != b"CLPK" || cur.u32()? != 1 {
        return Err("not a CLPK v1 dump".into());
    }
    let width = cur.u32()?;
    let height = cur.u32()?;
    let sps = cur.bytes()?;
    let pps = cur.bytes()?;
    let mut tracks = vec![TrackInfo {
        codec: CodecConfig::H264 {
            width,
            height,
            sps,
            pps,
        },
        label: "Video".into(),
    }];
    for index in 0..cur.u8()? {
        tracks.push(TrackInfo {
            codec: CodecConfig::Aac {
                sample_rate: 48_000,
                channels: 2,
                audio_specific_config: cur.bytes()?,
            },
            label: format!("Audio {}", index + 1),
        });
    }
    let layout = StreamLayout { tracks };

    let mut buffer = ReplayBuffer::new(
        ReplayConfig {
            duration: Duration::from_secs(seconds),
            memory_limit_bytes: ram_mb << 20,
            disk: disk_root.map(|root| DiskConfig {
                root,
                max_bytes: 8 << 30,
            }),
        },
        layout,
    )?;
    let mut packets = 0u64;
    while !cur.0.is_empty() {
        let track = cur.u8()?;
        let keyframe = cur.u8()? != 0;
        let pts = cur.i64()?;
        let dts = cur.i64()?;
        let duration = cur.i64()?;
        let data = cur.bytes()?;
        buffer.push(EncodedPacket {
            track,
            pts,
            dts,
            duration,
            keyframe,
            data: Arc::from(data),
        })?;
        packets += 1;
    }
    // Let the spill thread land pending segments before reporting.
    std::thread::sleep(Duration::from_millis(200));
    println!("pushed {packets} packets; buffer {:?}", buffer.stats());

    let snapshot = buffer.snapshot(Duration::from_secs(seconds))?;
    let started = Instant::now();
    let saved = export_clip(&ExportJob {
        snapshot,
        final_path: output.into(),
    })?;
    println!(
        "exported {} in {:?}: duration={:?} requested={:?} frames={} audio_tracks={} bytes={}",
        saved.path.display(),
        started.elapsed(),
        saved.duration,
        saved.requested,
        saved.video_frames,
        saved.audio_tracks,
        saved.bytes
    );
    Ok(())
}
