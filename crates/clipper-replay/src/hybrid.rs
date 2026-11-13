//! Replay manager: RAM tier plus asynchronous spill to the SSD tier, and
//! snapshots for `SaveReplay`.
//!
//! OWNER: RAM Buffer agent. Public signatures are the agreed contract used by
//! the recorder and exporter; bodies are stubs until implemented.

use std::sync::Arc;
use std::time::Duration;

use crate::disk::{DiskConfig, Segment};
use crate::error::ReplayError;
use crate::gop::Gop;
use crate::packet::{EncodedPacket, StreamLayout, Ticks};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayConfig {
    /// Replay length the user asked for (15/30/60/120/300 s).
    pub duration: Duration,
    /// RAM budget for encoded packets (default 64 MiB). Not the process size.
    pub memory_limit_bytes: usize,
    /// SSD tier. `None` = RAM only (replay shortens when the budget is hit).
    pub disk: Option<DiskConfig>,
}

/// Where the buffered footage currently lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BufferTier {
    #[default]
    Ram,
    /// Older footage spilled to SSD.
    Hybrid,
    /// SSD unavailable or failing: RAM only, possibly shorter than requested.
    RamOnlyDegraded,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BufferStats {
    /// Footage covered from the oldest buffered keyframe to the newest packet.
    pub buffered: Duration,
    pub ram_bytes: usize,
    pub disk_bytes: u64,
    pub ram_gops: usize,
    pub disk_gops: usize,
    pub tier: BufferTier,
    /// GOPs discarded without reaching the SSD (budget or disk failure).
    pub dropped_gops: u64,
    /// Last SSD error, if the tier is degraded.
    pub disk_error: Option<String>,
}

/// A GOP referenced by a snapshot. Holding it keeps the data alive (RAM) or
/// keeps the segment file from being deleted (disk).
#[derive(Clone, Debug)]
pub enum GopRef {
    Ram(Arc<Gop>),
    Disk { segment: Arc<Segment>, index: usize },
}

impl GopRef {
    pub fn sequence(&self) -> u64 {
        match self {
            Self::Ram(gop) => gop.sequence,
            Self::Disk { segment, index } => segment.gops()[*index].sequence,
        }
    }

    pub fn start_pts(&self) -> Ticks {
        match self {
            Self::Ram(gop) => gop.start_pts,
            Self::Disk { segment, index } => segment.gops()[*index].start_pts,
        }
    }

    /// Loads the packets (reads and validates the segment for disk GOPs).
    pub fn load(&self) -> Result<Arc<Gop>, ReplayError> {
        match self {
            Self::Ram(gop) => Ok(Arc::clone(gop)),
            Self::Disk { segment, index } => segment.read_gop(*index).map(Arc::new),
        }
    }
}

/// Immutable view of the footage needed for one clip, taken in O(GOPs)
/// without copying payloads. Recording continues while it is exported.
#[derive(Clone, Debug)]
pub struct ReplaySnapshot {
    pub layout: StreamLayout,
    /// Oldest first, contiguous sequences. Starts one GOP before the GOP that
    /// contains `end_pts - requested` (audio lookback / keyframe reserve).
    pub gops: Vec<GopRef>,
    pub requested: Duration,
    /// Newest presentation time covered by the snapshot (exclusive end).
    pub end_pts: Ticks,
}

pub struct ReplayBuffer {
    _private: (),
}

impl ReplayBuffer {
    /// Starts the spill thread when `config.disk` is set. Disk setup failure
    /// is not fatal: the buffer runs RAM only and reports the error in stats.
    pub fn new(config: ReplayConfig, layout: StreamLayout) -> Result<Self, ReplayError> {
        let _ = (config, layout);
        todo!()
    }

    /// Adds one packet. Never blocks and never performs file I/O. Packets
    /// before the first video keyframe are discarded.
    pub fn push(&mut self, packet: EncodedPacket) -> Result<(), ReplayError> {
        let _ = packet;
        todo!()
    }

    /// Changes retention live. Growing keeps everything; shrinking evicts.
    pub fn set_duration(&mut self, duration: Duration) {
        let _ = duration;
        todo!()
    }

    /// Changes the RAM budget live; excess GOPs spill or are evicted.
    pub fn set_memory_limit(&mut self, bytes: usize) {
        let _ = bytes;
        todo!()
    }

    /// References the last `duration` of footage (plus keyframe reserve).
    /// `NotReady` when no complete keyframe-aligned footage exists yet.
    pub fn snapshot(&mut self, duration: Duration) -> Result<ReplaySnapshot, ReplayError> {
        let _ = duration;
        todo!()
    }

    pub fn stats(&self) -> BufferStats {
        todo!()
    }

    pub fn layout(&self) -> &StreamLayout {
        todo!()
    }
}
