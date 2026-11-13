//! Replay manager: RAM tier plus asynchronous spill to the SSD tier, and
//! snapshots for `SaveReplay`.
//!
//! OWNER: RAM Buffer agent. Public signatures are the agreed contract used by
//! the recorder and exporter.
//!
//! Lifecycle of a sealed GOP: `RAM` -> (over budget) `spilling` (handed to
//! the spill thread, still referenced in RAM so snapshots see it) -> `Disk`
//! (spill thread reported success, RAM released) -> evicted (retention).
//! `push` never blocks and never touches files: handing off uses `try_send`
//! on a bounded channel and results are drained with `try_recv`.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::disk::{DiskConfig, Segment, SegmentStore};
use crate::error::ReplayError;
use crate::gop::Gop;
use crate::packet::{EncodedPacket, StreamLayout, Ticks};
use crate::ram::{PushOutcome, RamTier};

/// Spill jobs that may wait for the spill thread before `push` falls back to
/// the hard RAM cap.
const SPILL_QUEUE: usize = 4;
/// Spill batches try to hold at least this many GOPs...
const SPILL_MIN_GOPS: usize = 4;
/// ...unless they already reach this many charged bytes.
const SPILL_TARGET_BYTES: usize = 8 << 20;

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

/// Spill backend living on the spill thread.
trait SpillSink: Send {
    /// Writes `gops` (oldest first, contiguous) as one segment.
    fn write(&mut self, gops: &[Arc<Gop>]) -> Result<Arc<Segment>, ReplayError>;
}

struct StoreSink(SegmentStore);

impl SpillSink for StoreSink {
    fn write(&mut self, gops: &[Arc<Gop>]) -> Result<Arc<Segment>, ReplayError> {
        self.0.write_segment(gops)
    }
}

struct GopMeta {
    sequence: u64,
    start_pts: Ticks,
}

/// Spill thread report for one batch.
struct SpillDone {
    /// Same order as the batch = index inside the segment.
    metas: Vec<GopMeta>,
    result: Result<Arc<Segment>, ReplayError>,
}

fn spill_loop(
    mut sink: Box<dyn SpillSink>,
    jobs: Receiver<Vec<Arc<Gop>>>,
    done: SyncSender<SpillDone>,
) {
    // Ends when the buffer drops the job sender, or after the first failure
    // (the buffer then runs RAM only).
    while let Ok(batch) = jobs.recv() {
        let metas = batch
            .iter()
            .map(|gop| GopMeta {
                sequence: gop.sequence,
                start_pts: gop.start_pts,
            })
            .collect();
        let result = sink.write(&batch);
        drop(batch);
        let failed = result.is_err();
        if done.send(SpillDone { metas, result }).is_err() || failed {
            break;
        }
    }
}

struct Worker {
    jobs: SyncSender<Vec<Arc<Gop>>>,
    done: Receiver<SpillDone>,
    handle: JoinHandle<()>,
}

struct DiskEntry {
    segment: Arc<Segment>,
    index: usize,
    start_pts: Ticks,
}

fn ticks(duration: Duration) -> Ticks {
    Ticks::try_from(duration.as_nanos() / 100).unwrap_or(Ticks::MAX)
}

struct Core {
    duration: Duration,
    memory_limit: usize,
    ram: RamTier,
    /// Spilled GOPs, oldest first; all older than every RAM GOP.
    disk: VecDeque<DiskEntry>,
    worker: Option<Worker>,
    disk_failed: bool,
    disk_error: Option<String>,
    dropped_gops: u64,
}

impl Core {
    fn disk_usable(&self) -> bool {
        self.worker.is_some() && !self.disk_failed
    }

    fn fail_disk(&mut self, message: String) {
        self.disk_failed = true;
        self.disk_error = Some(message);
    }

    fn drain(&mut self) {
        loop {
            let message = match &self.worker {
                Some(worker) => worker.done.try_recv(),
                None => return,
            };
            match message {
                Ok(done) => self.apply(done),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    if !self.disk_failed {
                        self.fail_disk("the spill thread stopped unexpectedly".into());
                    }
                    return;
                }
            }
        }
    }

    fn apply(&mut self, done: SpillDone) {
        match done.result {
            Ok(segment) => {
                for (index, meta) in done.metas.into_iter().enumerate() {
                    // Missing = evicted while the write was running.
                    if self.ram.pop_oldest_if(meta.sequence).is_some() {
                        self.disk.push_back(DiskEntry {
                            segment: Arc::clone(&segment),
                            index,
                            start_pts: meta.start_pts,
                        });
                    }
                }
            }
            Err(error) => self.fail_disk(error.to_string()),
        }
    }

    fn push(&mut self, packet: EncodedPacket) -> Result<(), ReplayError> {
        self.drain();
        let outcome = self.ram.push(packet)?;
        if outcome == PushOutcome::Sealed {
            self.enforce_retention();
        }
        if outcome != PushOutcome::Discarded {
            self.enforce_budget();
        }
        Ok(())
    }

    fn sealed_len(&self) -> usize {
        self.disk.len() + self.ram.sealed().len()
    }

    fn sealed_start(&self, index: usize) -> Ticks {
        match self.disk.get(index) {
            Some(entry) => entry.start_pts,
            None => self.ram.sealed()[index - self.disk.len()].start_pts,
        }
    }

    /// Index (into disk ++ ram sealed GOPs) of the first GOP a clip of
    /// `duration` needs: one before the GOP containing `newest_end - duration`.
    fn first_needed(&self, duration: Duration) -> usize {
        let Some(newest) = self.ram.newest_end() else {
            return 0;
        };
        let cutoff = newest.saturating_sub(ticks(duration));
        let sealed = self.sealed_len();
        let open_contains = self.ram.open().is_some_and(|open| open.start_pts <= cutoff);
        let containing = if open_contains {
            sealed
        } else {
            let mut count = 0;
            while count < sealed && self.sealed_start(count) <= cutoff {
                count += 1;
            }
            count.saturating_sub(1)
        };
        containing.saturating_sub(1)
    }

    fn enforce_retention(&mut self) {
        for _ in 0..self.first_needed(self.duration) {
            if self.disk.pop_front().is_none() {
                self.ram.pop_oldest();
            }
        }
    }

    fn evict_ram_over(&mut self, cap: usize) {
        while self.ram.total_bytes() > cap {
            if self.ram.pop_oldest().is_none() {
                break;
            }
            self.dropped_gops += 1;
        }
    }

    fn enforce_budget(&mut self) {
        let limit = self.memory_limit;
        if self.disk_usable() {
            let excess = self
                .ram
                .total_bytes()
                .saturating_sub(self.ram.handed_bytes())
                .saturating_sub(limit);
            if excess > 0 {
                self.try_spill(excess);
            }
            // Spill queue full / disk slow: never exceed twice the budget.
            self.evict_ram_over(limit.saturating_mul(2));
        } else {
            self.evict_ram_over(limit);
        }
        // A runaway open GOP (no keyframes arriving) must not grow forever.
        if self.ram.open_charged() > limit.saturating_mul(2) {
            self.ram.discard_open();
            self.dropped_gops += 1;
        }
    }

    fn try_spill(&mut self, excess: usize) {
        let batch = self
            .ram
            .peek_spill_batch(excess, SPILL_MIN_GOPS, SPILL_TARGET_BYTES);
        if batch.is_empty() {
            return;
        }
        let Some(worker) = &self.worker else { return };
        match worker.jobs.try_send(batch.clone()) {
            Ok(()) => self.ram.mark_handed(&batch),
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {
                self.fail_disk("the spill thread stopped unexpectedly".into());
            }
        }
    }

    fn snapshot(
        &mut self,
        layout: &StreamLayout,
        duration: Duration,
    ) -> Result<ReplaySnapshot, ReplayError> {
        self.drain();
        let (Some(open), Some(end_pts)) = (self.ram.open(), self.ram.newest_end()) else {
            return Err(ReplayError::NotReady);
        };
        let first = self.first_needed(duration);
        let open = Arc::new(open.clone());
        let mut gops = Vec::with_capacity(self.sealed_len().saturating_sub(first) + 1);
        for entry in self.disk.iter().skip(first) {
            gops.push(GopRef::Disk {
                segment: Arc::clone(&entry.segment),
                index: entry.index,
            });
        }
        let ram_skip = first.saturating_sub(self.disk.len());
        for gop in self.ram.sealed().iter().skip(ram_skip) {
            gops.push(GopRef::Ram(Arc::clone(gop)));
        }
        gops.push(GopRef::Ram(open));
        Ok(ReplaySnapshot {
            layout: layout.clone(),
            gops,
            requested: duration,
            end_pts,
        })
    }

    fn stats(&mut self) -> BufferStats {
        self.drain();
        let oldest = self
            .disk
            .front()
            .map(|entry| entry.start_pts)
            .or_else(|| self.ram.sealed().front().map(|gop| gop.start_pts))
            .or_else(|| self.ram.open().map(|gop| gop.start_pts));
        let buffered = match (oldest, self.ram.newest_end()) {
            (Some(start), Some(end)) if end > start => {
                Duration::from_nanos((end - start) as u64 * 100)
            }
            _ => Duration::ZERO,
        };
        let mut disk_bytes = 0;
        let mut previous: Option<&Arc<Segment>> = None;
        for entry in &self.disk {
            if !previous.is_some_and(|segment| Arc::ptr_eq(segment, &entry.segment)) {
                disk_bytes += entry.segment.bytes();
                previous = Some(&entry.segment);
            }
        }
        let tier = if self.disk_failed {
            BufferTier::RamOnlyDegraded
        } else if self.worker.is_some() && !self.disk.is_empty() {
            BufferTier::Hybrid
        } else {
            BufferTier::Ram
        };
        BufferStats {
            buffered,
            ram_bytes: self.ram.total_bytes(),
            disk_bytes,
            ram_gops: self.ram.sealed().len() + usize::from(self.ram.open().is_some()),
            disk_gops: self.disk.len(),
            tier,
            dropped_gops: self.dropped_gops,
            disk_error: self.disk_error.clone(),
        }
    }
}

pub struct ReplayBuffer {
    layout: StreamLayout,
    core: Mutex<Core>,
}

impl ReplayBuffer {
    /// Starts the spill thread when `config.disk` is set. Disk setup failure
    /// is not fatal: the buffer runs RAM only and reports the error in stats.
    pub fn new(config: ReplayConfig, layout: StreamLayout) -> Result<Self, ReplayError> {
        let sink: Result<Option<Box<dyn SpillSink>>, ReplayError> = match &config.disk {
            Some(disk) => {
                SegmentStore::open(disk).map(|store| Some(Box::new(StoreSink(store)) as _))
            }
            None => Ok(None),
        };
        match sink {
            Ok(sink) => Self::build(config, layout, sink, None),
            Err(error) => Self::build(config, layout, None, Some(error.to_string())),
        }
    }

    /// Test constructor with a custom spill backend.
    #[cfg(test)]
    fn with_sink(
        config: ReplayConfig,
        layout: StreamLayout,
        sink: Box<dyn SpillSink>,
    ) -> Result<Self, ReplayError> {
        Self::build(config, layout, Some(sink), None)
    }

    fn build(
        config: ReplayConfig,
        layout: StreamLayout,
        sink: Option<Box<dyn SpillSink>>,
        setup_error: Option<String>,
    ) -> Result<Self, ReplayError> {
        if layout.tracks.is_empty() {
            return Err(ReplayError::Invalid("layout has no tracks".into()));
        }
        let mut disk_failed = setup_error.is_some();
        let mut disk_error = setup_error;
        let mut worker = None;
        if let Some(sink) = sink {
            let (jobs, job_rx) = mpsc::sync_channel(SPILL_QUEUE);
            // Room for every queued job plus the one being written.
            let (done_tx, done) = mpsc::sync_channel(SPILL_QUEUE + 2);
            match std::thread::Builder::new()
                .name("clipper-replay-spill".into())
                .spawn(move || spill_loop(sink, job_rx, done_tx))
            {
                Ok(handle) => worker = Some(Worker { jobs, done, handle }),
                Err(error) => {
                    disk_failed = true;
                    disk_error = Some(format!("cannot start the spill thread: {error}"));
                }
            }
        }
        let ram = RamTier::new(layout.tracks.len());
        Ok(Self {
            layout,
            core: Mutex::new(Core {
                duration: config.duration,
                memory_limit: config.memory_limit_bytes,
                ram,
                disk: VecDeque::new(),
                worker,
                disk_failed,
                disk_error,
                dropped_gops: 0,
            }),
        })
    }

    fn core_mut(&mut self) -> &mut Core {
        self.core.get_mut().unwrap_or_else(PoisonError::into_inner)
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds one packet. Never blocks and never performs file I/O. Packets
    /// before the first video keyframe are discarded.
    pub fn push(&mut self, packet: EncodedPacket) -> Result<(), ReplayError> {
        self.core_mut().push(packet)
    }

    /// Changes retention live. Growing keeps everything; shrinking evicts.
    pub fn set_duration(&mut self, duration: Duration) {
        let core = self.core_mut();
        core.duration = duration;
        core.drain();
        core.enforce_retention();
    }

    /// Changes the RAM budget live; excess GOPs spill or are evicted.
    pub fn set_memory_limit(&mut self, bytes: usize) {
        let core = self.core_mut();
        core.memory_limit = bytes;
        core.drain();
        core.enforce_budget();
    }

    /// References the last `duration` of footage (plus keyframe reserve).
    /// `NotReady` when no complete keyframe-aligned footage exists yet.
    pub fn snapshot(&mut self, duration: Duration) -> Result<ReplaySnapshot, ReplayError> {
        let layout = self.layout.clone();
        self.core_mut().snapshot(&layout, duration)
    }

    pub fn stats(&self) -> BufferStats {
        self.core().stats()
    }

    pub fn layout(&self) -> &StreamLayout {
        &self.layout
    }
}

impl Drop for ReplayBuffer {
    fn drop(&mut self) {
        if let Some(Worker { jobs, done, handle }) = self.core_mut().worker.take() {
            // Closing the job channel ends the thread after its current
            // write; dropping the result receiver unblocks a pending send.
            drop(jobs);
            drop(done);
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, Sender};
    use std::time::Instant;

    use super::*;
    use crate::packet::TICKS_PER_SECOND;
    use crate::ram::testutil::*;

    const MIB: usize = 1 << 20;

    fn config(seconds: u64, limit: usize) -> ReplayConfig {
        ReplayConfig {
            duration: Duration::from_secs(seconds),
            memory_limit_bytes: limit,
            disk: None,
        }
    }

    fn ram_only(seconds: u64, limit: usize) -> ReplayBuffer {
        ReplayBuffer::new(config(seconds, limit), layout()).unwrap()
    }

    struct FailingSink;

    impl SpillSink for FailingSink {
        fn write(&mut self, _: &[Arc<Gop>]) -> Result<Arc<Segment>, ReplayError> {
            Err(ReplayError::DiskFull {
                path: "cache".into(),
            })
        }
    }

    /// Blocks inside `write` until the test drops its `Sender`.
    struct BlockingSink {
        started: Sender<()>,
        release: Receiver<()>,
    }

    impl SpillSink for BlockingSink {
        fn write(&mut self, _: &[Arc<Gop>]) -> Result<Arc<Segment>, ReplayError> {
            let _ = self.started.send(());
            let _ = self.release.recv();
            Err(ReplayError::Shutdown)
        }
    }

    fn wait_for(buf: &ReplayBuffer, what: impl Fn(&BufferStats) -> bool) -> BufferStats {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let stats = buf.stats();
            if what(&stats) {
                return stats;
            }
            assert!(Instant::now() < deadline, "timed out: {stats:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_contiguous(snapshot: &ReplaySnapshot) {
        let mut previous = None;
        for gop in &snapshot.gops {
            let GopRef::Ram(gop) = gop else {
                panic!("unexpected disk gop")
            };
            if let Some(p) = previous {
                assert_eq!(gop.sequence, p + 1);
            }
            previous = Some(gop.sequence);
        }
    }

    #[test]
    fn not_ready_without_keyframe() {
        let mut buf = ram_only(10, 64 * MIB);
        assert!(matches!(
            buf.snapshot(Duration::from_secs(5)),
            Err(ReplayError::NotReady)
        ));
        buf.push(packet(1, 0, AAC, true, 10)).unwrap();
        buf.push(packet(0, 0, FRAME, false, 10)).unwrap();
        assert!(matches!(
            buf.snapshot(Duration::from_secs(5)),
            Err(ReplayError::NotReady)
        ));
        assert_eq!(buf.stats().ram_bytes, 0);
    }

    #[test]
    fn invalid_track_is_rejected() {
        let mut buf = ram_only(10, 64 * MIB);
        assert!(matches!(
            buf.push(packet(2, 0, AAC, true, 10)),
            Err(ReplayError::Invalid(_))
        ));
        buf.push(packet(0, FRAME, FRAME, true, 10)).unwrap();
        assert!(matches!(
            buf.push(packet(0, FRAME, FRAME, false, 10)),
            Err(ReplayError::Invalid(_))
        ));
        assert_eq!(buf.layout().tracks.len(), 2);
    }

    #[test]
    fn evicts_by_duration_keeping_one_reserve_gop() {
        let mut buf = ram_only(6, 1024 * MIB);
        Gen::new().seconds(&mut buf, 40);
        let stats = buf.stats();
        // 6 s = 3 GOPs, +1 reserve, +1 open or containing GOP slack.
        assert!(stats.ram_gops <= 5, "{stats:?}");
        assert!(stats.ram_gops >= 4, "{stats:?}");
        assert_eq!(stats.tier, BufferTier::Ram);
        assert_eq!(stats.dropped_gops, 0);
        assert!(stats.buffered >= Duration::from_secs(6));
        let snap = buf.snapshot(Duration::from_secs(6)).unwrap();
        assert_contiguous(&snap);
        let first = snap.gops[0].start_pts();
        // First GOP starts at or before cutoff minus one GOP's worth.
        assert!(first <= snap.end_pts - 6 * TICKS_PER_SECOND);
        assert!(snap.end_pts - first <= 6 * TICKS_PER_SECOND + 4 * TICKS_PER_SECOND + 1);
    }

    #[test]
    fn snapshot_includes_open_gop() {
        let mut buf = ram_only(30, 1024 * MIB);
        let mut generator = Gen::new();
        generator.seconds(&mut buf, 5);
        let snap = buf.snapshot(Duration::from_secs(30)).unwrap();
        let GopRef::Ram(last) = snap.gops.last().unwrap() else {
            panic!()
        };
        assert_eq!(last.sequence, 2);
        assert_eq!(snap.gops.len(), 3);
        assert_eq!(snap.end_pts, last.end_pts);
        assert!(snap.end_pts >= 5 * TICKS_PER_SECOND);
        // The clone is a copy: later pushes do not change it.
        let before = last.packets.len();
        generator.seconds(&mut buf, 1);
        assert_eq!(last.packets.len(), before);
    }

    #[test]
    fn snapshot_stays_valid_after_eviction() {
        let mut buf = ram_only(4, 1024 * MIB);
        let mut generator = Gen::new();
        generator.seconds(&mut buf, 10);
        let snap = buf.snapshot(Duration::from_secs(4)).unwrap();
        let oldest = snap.gops[0].sequence();
        let total: usize = snap
            .gops
            .iter()
            .map(|gop| gop.load().unwrap().packets.len())
            .sum();
        generator.seconds(&mut buf, 60);
        let still = buf.snapshot(Duration::from_secs(4)).unwrap();
        assert!(still.gops[0].sequence() > oldest);
        let again: usize = snap
            .gops
            .iter()
            .map(|gop| gop.load().unwrap().packets.len())
            .sum();
        assert_eq!(total, again);
        assert!(total > 0);
    }

    #[test]
    fn budget_evicts_without_disk() {
        let limit = 10 * MIB;
        let mut buf = ram_only(300, limit);
        let mut generator = Gen::new();
        generator.run_with(&mut buf, 60 * TICKS_PER_SECOND, |buf| {
            assert!(buf.stats().ram_bytes <= limit);
        });
        let stats = buf.stats();
        assert_eq!(stats.tier, BufferTier::Ram);
        assert!(stats.dropped_gops > 0);
        assert!(stats.ram_bytes <= limit);
        assert!(buf.snapshot(Duration::from_secs(300)).is_ok());
    }

    #[test]
    fn set_duration_grow_keeps_and_shrink_evicts() {
        let mut buf = ram_only(10, 1024 * MIB);
        let mut generator = Gen::new();
        generator.seconds(&mut buf, 30);
        let small = buf.stats().ram_gops;
        buf.set_duration(Duration::from_secs(60));
        generator.seconds(&mut buf, 60);
        let grown = buf.stats().ram_gops;
        assert!(grown > small + 10, "{small} -> {grown}");
        // Growing never evicted anything already retained.
        let before = buf.stats().ram_gops;
        buf.set_duration(Duration::from_secs(120));
        assert_eq!(buf.stats().ram_gops, before);
        buf.set_duration(Duration::from_secs(4));
        let shrunk = buf.stats();
        assert!(shrunk.ram_gops <= 4, "{shrunk:?}");
        assert_eq!(shrunk.dropped_gops, 0);
        assert!(buf.snapshot(Duration::from_secs(4)).is_ok());
    }

    #[test]
    fn set_memory_limit_shrink_evicts() {
        let mut buf = ram_only(300, 1024 * MIB);
        Gen::new().seconds(&mut buf, 60);
        let before = buf.stats();
        assert!(before.ram_bytes > 20 * MIB);
        buf.set_memory_limit(10 * MIB);
        let after = buf.stats();
        assert!(after.ram_bytes <= 10 * MIB, "{after:?}");
        assert!(after.dropped_gops > 0);
        buf.set_memory_limit(1024 * MIB);
        assert!(buf.snapshot(Duration::from_secs(10)).is_ok());
    }

    #[test]
    fn disk_failure_degrades_to_ram_only_and_keeps_working() {
        let mut buf =
            ReplayBuffer::with_sink(config(300, 8 * MIB), layout(), Box::new(FailingSink)).unwrap();
        let mut generator = Gen::new();
        generator.seconds(&mut buf, 12);
        let stats = wait_for(&buf, |s| s.tier == BufferTier::RamOnlyDegraded);
        assert!(stats.disk_error.unwrap().contains("not enough disk space"));
        generator.run_with(&mut buf, 60 * TICKS_PER_SECOND, |buf| {
            assert!(buf.stats().ram_bytes <= 8 * MIB * 2);
        });
        let stats = buf.stats();
        assert_eq!(stats.tier, BufferTier::RamOnlyDegraded);
        assert!(stats.ram_bytes <= 8 * MIB);
        assert!(stats.dropped_gops > 0);
        assert!(buf.snapshot(Duration::from_secs(10)).is_ok());
    }

    #[test]
    fn push_never_blocks_when_spill_is_stuck() {
        let (started_tx, started) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let sink = BlockingSink {
            started: started_tx,
            release,
        };
        let limit = 4 * MIB;
        let mut buf =
            ReplayBuffer::with_sink(config(300, limit), layout(), Box::new(sink)).unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut generator = Gen::new();
            let mut worst = 0;
            generator.run_with(&mut buf, 120 * TICKS_PER_SECOND, |buf| {
                worst = worst.max(buf.stats().ram_bytes);
            });
            let stats = buf.stats();
            tx.send((worst, stats)).unwrap();
            buf
        });
        let (worst, stats) = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("push blocked on a stuck spill thread");
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(worst <= 2 * limit, "{worst}");
        assert!(stats.dropped_gops > 0);
        assert_eq!(stats.tier, BufferTier::Ram);
        // Unblock the sink so Drop can join the thread.
        drop(release_tx);
        let buf = worker.join().unwrap();
        wait_for(&buf, |s| s.tier == BufferTier::RamOnlyDegraded);
        drop(buf);
    }

    #[test]
    fn drop_joins_idle_spill_thread_promptly() {
        let buf =
            ReplayBuffer::with_sink(config(10, MIB), layout(), Box::new(FailingSink)).unwrap();
        let start = Instant::now();
        drop(buf);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn long_session_stays_bounded() {
        let limit = 32 * MIB;
        let mut buf = ram_only(300, limit);
        let mut generator = Gen::new();
        let mut peak = 0;
        let mut peak_gops = 0;
        // 30 minutes, checked once per second of footage.
        let mut next_check = 0;
        generator.run_with(&mut buf, 1800 * TICKS_PER_SECOND, |buf| {
            // Stats are O(1) without disk; sample every ~60th video frame.
            next_check += 1;
            if next_check % 100 == 0 {
                let stats = buf.stats();
                peak = peak.max(stats.ram_bytes);
                peak_gops = peak_gops.max(stats.ram_gops);
            }
        });
        assert!(peak <= 2 * limit, "{peak}");
        assert!(peak_gops <= 12, "{peak_gops}");
        let stats = buf.stats();
        assert!(stats.ram_bytes <= limit);
        assert!(stats.dropped_gops > 100);
        assert!(buf.snapshot(Duration::from_secs(300)).is_ok());
    }
}
