//! SSD tier: rotating temporary segment files.
//!
//! OWNER: Disk Buffer agent. The public signatures below are the agreed
//! contract used by `hybrid` and `export`; bodies are stubs until implemented.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::ReplayError;
use crate::gop::Gop;
use crate::packet::Ticks;

/// Where and how much the SSD tier may write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskConfig {
    /// App-owned cache root, e.g. `%LOCALAPPDATA%\Clipper\replay-cache`.
    /// Each session writes into its own locked subdirectory of it.
    pub root: PathBuf,
    /// Upper bound for live segment bytes of this session.
    pub max_bytes: u64,
}

/// Index entry for one GOP stored inside a segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentGop {
    pub sequence: u64,
    pub start_pts: Ticks,
    pub end_pts: Ticks,
    /// Byte range of the GOP record inside the segment file.
    pub offset: u64,
    pub len: u64,
    /// `Gop::charged_bytes` of the original group.
    pub charged_bytes: usize,
}

/// One finished, immutable segment file. Deleting is tied to `Drop`: the
/// file is removed when the buffer evicted it AND no export snapshot still
/// holds an `Arc<Segment>`, so a running export can never lose its input.
#[derive(Debug)]
pub struct Segment {
    _private: (),
}

impl Segment {
    pub fn path(&self) -> &Path {
        todo!()
    }

    /// File size in bytes.
    pub fn bytes(&self) -> u64 {
        todo!()
    }

    pub fn gops(&self) -> &[SegmentGop] {
        todo!()
    }

    /// Reads and validates (length, checksum) GOP `index` of `gops()`.
    pub fn read_gop(&self, index: usize) -> Result<Gop, ReplayError> {
        let _ = index;
        todo!()
    }
}

/// Writer for one replay session's segment directory. Lives on the spill
/// thread; never touched by capture or UI threads.
#[derive(Debug)]
pub struct SegmentStore {
    _private: (),
}

impl SegmentStore {
    /// Creates and locks `<root>/session-<id>/`, removing stale sessions of
    /// crashed processes first (only directories this module created).
    pub fn open(config: &DiskConfig) -> Result<Self, ReplayError> {
        let _ = config;
        todo!()
    }

    /// Writes `gops` (oldest first, contiguous sequences) as one segment:
    /// `*.tmp` then flush and atomic rename. Fails with `DiskFull` before
    /// writing when the session would exceed `max_bytes` or the volume is
    /// short on space; a failed write leaves no file behind.
    pub fn write_segment(&mut self, gops: &[Arc<Gop>]) -> Result<Arc<Segment>, ReplayError> {
        let _ = gops;
        todo!()
    }

    /// Live bytes of this session's segments, including segments only kept
    /// alive by running exports.
    pub fn bytes_used(&self) -> u64 {
        todo!()
    }

    pub fn session_dir(&self) -> &Path {
        todo!()
    }
}

/// Removes session directories under `root` whose owning process is gone
/// (lock not held). Never touches anything outside `root` or files it did
/// not create. Returns the number of removed sessions.
pub fn cleanup_stale_sessions(root: &Path) -> Result<usize, ReplayError> {
    let _ = root;
    todo!()
}

/// Free bytes available to the caller on the volume containing `path`.
pub fn available_space(path: &Path) -> Option<u64> {
    let _ = path;
    todo!()
}
