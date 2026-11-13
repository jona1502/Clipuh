//! Clip export: snapshot -> keyframe-aligned plan -> MP4 remux (no
//! re-encoding) -> `<name>.mp4.partial` -> flush -> atomic rename.
//!
//! OWNER: Clip Export agent. Public signatures are the agreed contract used
//! by the recorder; bodies are stubs until implemented.

use std::path::PathBuf;
use std::time::Duration;

use crate::error::ReplayError;
use crate::hybrid::ReplaySnapshot;
use crate::packet::Ticks;

#[derive(Clone, Debug)]
pub struct ExportJob {
    pub snapshot: ReplaySnapshot,
    /// Final clip path (`.mp4`). The exporter writes `<path>.partial` first.
    pub final_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedClip {
    pub path: PathBuf,
    /// Actual video duration of the written clip.
    pub duration: Duration,
    pub requested: Duration,
    /// Session PTS of the first (key)frame in the clip.
    pub start_pts: Ticks,
    pub video_frames: u64,
    pub audio_tracks: usize,
    pub bytes: u64,
}

/// Writes one clip synchronously. Only `Ok` means a complete, finalized,
/// renamed file exists; on error no partial file is left behind.
pub fn export_clip(job: &ExportJob) -> Result<SavedClip, ReplayError> {
    let _ = job;
    todo!()
}

pub type ExportDone = Box<dyn FnOnce(Result<SavedClip, ReplayError>) + Send + 'static>;

/// One background export thread with a bounded job queue.
pub struct Exporter {
    _private: (),
}

impl Exporter {
    /// `queue_capacity` jobs may wait behind the running one.
    pub fn spawn(queue_capacity: usize) -> std::io::Result<Self> {
        let _ = queue_capacity;
        todo!()
    }

    /// Never blocks. `Busy` when the queue is full. `done` runs on the export
    /// thread exactly once per accepted job (also on shutdown, with
    /// `Shutdown`).
    pub fn try_submit(&self, job: ExportJob, done: ExportDone) -> Result<(), ReplayError> {
        let _ = (job, done);
        todo!()
    }

    /// Jobs accepted and not yet completed.
    pub fn in_flight(&self) -> usize {
        todo!()
    }
}
