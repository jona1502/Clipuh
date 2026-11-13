//! Shared synthetic stream generator and helpers for the integration tests
//! and the benchmark. Public API of `clipper-replay` only.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clipper_replay::{
    CodecConfig, EncodedPacket, StreamLayout, TICKS_PER_SECOND, Ticks, TrackInfo,
};

pub const FRAME: Ticks = 166_667;
pub const AAC_FRAME: Ticks = 213_333;
pub const GOP_FRAMES: u64 = 120;
pub const GOP_TICKS: Ticks = FRAME * GOP_FRAMES as Ticks;
pub const MIB: usize = 1 << 20;

pub fn layout(audio_tracks: usize, width: u32, height: u32) -> StreamLayout {
    let mut tracks = vec![TrackInfo {
        codec: CodecConfig::H264 {
            width,
            height,
            sps: vec![0x67, 0x64, 0x00, 0x28, 0xac],
            pps: vec![0x68, 0xee, 0x3c, 0x80],
        },
        label: "Video".into(),
    }];
    for i in 0..audio_tracks {
        tracks.push(TrackInfo {
            codec: CodecConfig::Aac {
                sample_rate: 48_000,
                channels: 2,
                audio_specific_config: vec![0x11, 0x90],
            },
            label: format!("Audio {i}"),
        });
    }
    StreamLayout { tracks }
}

/// AVCC shaped payload: 4 byte length prefix, NAL header, filler.
fn avcc(total: usize, idr: bool) -> Arc<[u8]> {
    let total = total.max(8);
    let mut data = vec![0xA5u8; total];
    data[..4].copy_from_slice(&((total - 4) as u32).to_be_bytes());
    data[4] = if idr { 0x65 } else { 0x41 };
    data.into()
}

/// Deterministic 60 fps video + AAC generator. Packets of all tracks are
/// produced in timestamp order, like a real capture.
pub struct Synth {
    audio_tracks: usize,
    idr: Arc<[u8]>,
    p: Arc<[u8]>,
    audio: Arc<[u8]>,
    next_video: u64,
    next_audio: u64,
    pending_audio: Vec<EncodedPacket>,
    pub packets: u64,
    pub payload_bytes: u64,
}

impl Synth {
    pub fn new(audio_tracks: usize, bitrate_bps: u64) -> Self {
        // One GOP is 2 s; the IDR carries 20 % of it.
        let gop_bytes = (bitrate_bps / 8 * 2) as usize;
        let idr = gop_bytes / 5;
        let p = (gop_bytes - idr) / (GOP_FRAMES as usize - 1);
        Self {
            audio_tracks,
            idr: avcc(idr, true),
            p: avcc(p, false),
            audio: vec![0x21u8; 400].into(),
            next_video: 0,
            next_audio: 0,
            pending_audio: Vec::new(),
            packets: 0,
            payload_bytes: 0,
        }
    }

    /// Session time of the next video frame.
    pub fn video_time(&self) -> Ticks {
        self.next_video as Ticks * FRAME
    }

    pub fn video_seconds(&self) -> f64 {
        self.video_time() as f64 / TICKS_PER_SECOND as f64
    }

    pub fn frames(&self) -> u64 {
        self.next_video
    }

    fn count(&mut self, packet: &EncodedPacket) {
        self.packets += 1;
        self.payload_bytes += packet.data.len() as u64;
    }

    /// Next packet in timestamp order (audio first on ties).
    pub fn next_packet(&mut self) -> EncodedPacket {
        if self.audio_tracks > 0 {
            if self.pending_audio.is_empty() {
                let audio_pts = self.next_audio as Ticks * AAC_FRAME;
                if audio_pts <= self.next_video as Ticks * FRAME {
                    for track in (1..=self.audio_tracks).rev() {
                        self.pending_audio.push(EncodedPacket {
                            track: track as u8,
                            pts: audio_pts,
                            dts: audio_pts,
                            duration: AAC_FRAME,
                            keyframe: true,
                            data: Arc::clone(&self.audio),
                        });
                    }
                    self.next_audio += 1;
                }
            }
            if let Some(packet) = self.pending_audio.pop() {
                self.count(&packet);
                return packet;
            }
        }
        let key = self.next_video.is_multiple_of(GOP_FRAMES);
        let pts = self.next_video as Ticks * FRAME;
        self.next_video += 1;
        let packet = EncodedPacket {
            track: 0,
            pts,
            dts: pts,
            duration: FRAME,
            keyframe: key,
            data: Arc::clone(if key { &self.idr } else { &self.p }),
        };
        self.count(&packet);
        packet
    }

    /// Feeds packets until the video clock reached `frames` total frames
    /// (pending audio of the last frame is flushed first).
    pub fn run_frames<E>(
        &mut self,
        frames: u64,
        mut sink: impl FnMut(EncodedPacket) -> Result<(), E>,
    ) -> Result<(), E> {
        while self.next_video < frames {
            sink(self.next_packet())?;
        }
        Ok(())
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique temp directory, removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "clipper-replay-it-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Files in `dir` whose name ends with `suffix`.
pub fn files_with_suffix(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(suffix))
        })
        .collect()
}

/// Session directories (`session-*`) directly under `root`.
pub fn session_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("session-"))
        })
        .collect()
}

/// Polls `cond` until true or `timeout` passes.
pub fn wait_for(timeout: std::time::Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = std::time::Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
