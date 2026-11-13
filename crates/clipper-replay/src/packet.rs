//! Encoded packet contract between encoder, buffer and exporter.
//!
//! Time base: every timestamp is in 100 ns ticks ([`TICKS_PER_SECOND`]) on
//! one monotonic session timeline shared by all tracks (QPC based, zero at
//! the replay session origin, the same origin the audio mixer uses). Values
//! are never negative.

use std::sync::Arc;

pub type Ticks = i64;
pub const TICKS_PER_SECOND: Ticks = 10_000_000;

/// Fixed per-packet bookkeeping cost counted against the RAM budget in
/// addition to the payload, so budgets stay honest with tiny audio packets.
pub const PACKET_OVERHEAD: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecConfig {
    /// H.264. Packet payloads are AVCC: NAL units each prefixed with a
    /// 4-byte big-endian length (no Annex B start codes). `sps`/`pps` are raw
    /// NAL units without length prefix or start code.
    H264 {
        width: u32,
        height: u32,
        sps: Vec<u8>,
        pps: Vec<u8>,
    },
    /// AAC-LC raw access units (no ADTS headers), one access unit (1024
    /// samples per channel) per packet.
    Aac {
        sample_rate: u32,
        channels: u16,
        /// MPEG-4 AudioSpecificConfig (2 bytes for AAC-LC).
        audio_specific_config: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInfo {
    pub codec: CodecConfig,
    /// Human readable track name, e.g. "Video", "Mix", "Desktop", "Microphone".
    pub label: String,
}

/// Track list of one replay session. Index = `EncodedPacket::track`.
/// Track 0 is always the H.264 video track; later tracks are AAC audio,
/// track 1 being the primary mix. The layout never changes during a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamLayout {
    pub tracks: Vec<TrackInfo>,
}

impl StreamLayout {
    pub const VIDEO_TRACK: u8 = 0;

    pub fn video(&self) -> Option<&TrackInfo> {
        self.tracks.first()
    }

    pub fn audio_tracks(&self) -> usize {
        self.tracks.len().saturating_sub(1)
    }
}

/// One encoded access unit. Cheap to clone: the payload is shared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedPacket {
    pub track: u8,
    /// Presentation time.
    pub pts: Ticks,
    /// Decode time. Equal to `pts` without B-frames and for audio;
    /// `dts <= pts` and strictly increasing per track.
    pub dts: Ticks,
    /// Presentation duration (> 0).
    pub duration: Ticks,
    /// Video: IDR access unit, decodable without earlier packets.
    /// Audio: always true.
    pub keyframe: bool,
    pub data: Arc<[u8]>,
}

impl EncodedPacket {
    /// Bytes charged against the RAM budget.
    pub fn charged_size(&self) -> usize {
        self.data.len() + PACKET_OVERHEAD
    }

    pub fn is_video(&self) -> bool {
        self.track == StreamLayout::VIDEO_TRACK
    }

    pub fn end(&self) -> Ticks {
        self.pts.saturating_add(self.duration)
    }
}
