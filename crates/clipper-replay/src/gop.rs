//! Group of pictures: the unit the buffer evicts, spills and exports.

use crate::packet::{EncodedPacket, Ticks};

/// All packets (every track) that arrived from one video IDR keyframe up to,
/// but excluding, the next one, in arrival order.
///
/// Audio is grouped by arrival, not by timestamp: audio for the first
/// ~100 ms of a GOP may sit in the previous GOP. Retention and export
/// therefore always keep one extra GOP before the first exported keyframe
/// (see `docs/replay-architecture.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gop {
    /// Monotonic per session, starting at 0. Never reused.
    pub sequence: u64,
    /// PTS of the opening video keyframe.
    pub start_pts: Ticks,
    /// Largest `pts + duration` of any packet in the group.
    pub end_pts: Ticks,
    /// First packet is the video keyframe.
    pub packets: Vec<EncodedPacket>,
    /// Sum of `EncodedPacket::charged_size`.
    pub charged_bytes: usize,
}

impl Gop {
    /// Starts a group with its opening keyframe.
    pub fn new(sequence: u64, keyframe: EncodedPacket) -> Self {
        let mut gop = Self {
            sequence,
            start_pts: keyframe.pts,
            end_pts: keyframe.end(),
            packets: Vec::new(),
            charged_bytes: 0,
        };
        gop.push(keyframe);
        gop
    }

    pub fn push(&mut self, packet: EncodedPacket) {
        self.end_pts = self.end_pts.max(packet.end());
        self.charged_bytes += packet.charged_size();
        self.packets.push(packet);
    }

    /// Payload bytes only (what a disk segment stores).
    pub fn payload_bytes(&self) -> usize {
        self.packets.iter().map(|packet| packet.data.len()).sum()
    }
}
