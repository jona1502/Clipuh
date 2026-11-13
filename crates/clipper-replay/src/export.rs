//! Clip export: snapshot -> keyframe-aligned plan -> MP4 remux (no
//! re-encoding) -> `<name>.mp4.partial` -> flush -> atomic rename.
//!
//! Only `Ok` from [`export_clip`] means a complete, finalized clip exists.
//! The final path is never overwritten (the caller picks unique names); the
//! check happens before writing and again just before the rename, a tiny
//! race window remains because `std::fs::rename` replaces on Windows.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::ReplayError;
use crate::hybrid::ReplaySnapshot;
use crate::mp4;
use crate::packet::{TICKS_PER_SECOND, Ticks};
use crate::plan::plan_clip;

/// Free space that must remain after the clip is written.
const FREE_SPACE_MARGIN: u64 = 16 * 1024 * 1024;

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

fn partial_path(final_path: &Path) -> PathBuf {
    let mut name: OsString = final_path.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

#[cfg(not(test))]
fn free_space(path: &Path) -> Option<u64> {
    crate::disk::available_space(path)
}

#[cfg(test)]
fn free_space(_path: &Path) -> Option<u64> {
    None
}

fn ticks_to_duration(ticks: Ticks) -> Duration {
    let ticks = u64::try_from(ticks).unwrap_or(0);
    Duration::new(
        ticks / TICKS_PER_SECOND as u64,
        ((ticks % TICKS_PER_SECOND as u64) * 100) as u32,
    )
}

fn duration_to_ticks(duration: Duration) -> Ticks {
    Ticks::try_from(duration.as_nanos() / 100).unwrap_or(Ticks::MAX)
}

fn exists_error(path: &Path) -> ReplayError {
    ReplayError::io(
        "clip already exists",
        path,
        io::Error::new(io::ErrorKind::AlreadyExists, "refusing to overwrite a clip"),
    )
}

fn create_partial(partial: &Path) -> Result<File, ReplayError> {
    let open = || {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(partial)
    };
    match open() {
        Ok(file) => Ok(file),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            // Stale partial of a crashed run: unique clip names make this
            // file ours to replace.
            fs::remove_file(partial)
                .and_then(|()| open())
                .map_err(|err| ReplayError::io("creating partial clip", partial, err))
        }
        Err(err) => Err(ReplayError::io("creating partial clip", partial, err)),
    }
}

/// Writes one clip synchronously. Only `Ok` means a complete, finalized,
/// renamed file exists; on error no partial file is left behind.
pub fn export_clip(job: &ExportJob) -> Result<SavedClip, ReplayError> {
    let snapshot = &job.snapshot;
    let gops = snapshot
        .gops
        .iter()
        .map(|gop| gop.load())
        .collect::<Result<Vec<_>, _>>()?;
    let plan = plan_clip(
        &snapshot.layout,
        &gops,
        duration_to_ticks(snapshot.requested),
        snapshot.end_pts,
    )?;
    drop(gops);

    let final_path = &job.final_path;
    if final_path.exists() {
        return Err(exists_error(final_path));
    }
    let dir = match final_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    if let Some(free) = free_space(dir)
        && free
            < plan
                .estimated_file_bytes()
                .saturating_add(FREE_SPACE_MARGIN)
    {
        return Err(ReplayError::DiskFull {
            path: dir.to_path_buf(),
        });
    }

    let partial = partial_path(final_path);
    let file = create_partial(&partial)?;
    let written = write_and_finalize(file, &partial, final_path, snapshot, &plan);
    match written {
        Ok(bytes) => Ok(SavedClip {
            path: final_path.clone(),
            duration: ticks_to_duration(plan.duration),
            requested: snapshot.requested,
            start_pts: plan.start_pts,
            video_frames: plan.video.samples.len() as u64,
            audio_tracks: plan.audio.len(),
            bytes,
        }),
        Err(err) => {
            let _ = fs::remove_file(&partial);
            Err(err)
        }
    }
}

fn write_and_finalize(
    file: File,
    partial: &Path,
    final_path: &Path,
    snapshot: &ReplaySnapshot,
    plan: &crate::plan::ClipPlan,
) -> Result<u64, ReplayError> {
    let mut out = BufWriter::with_capacity(1 << 20, file);
    let bytes = mp4::write_mp4(&mut out, partial, &snapshot.layout, plan)?;
    out.flush()
        .map_err(|err| ReplayError::io("flushing clip", partial, err))?;
    let file = out
        .into_inner()
        .map_err(|err| ReplayError::io("flushing clip", partial, err.into_error()))?;
    file.sync_all()
        .map_err(|err| ReplayError::io("syncing clip", partial, err))?;
    drop(file);
    if final_path.exists() {
        return Err(exists_error(final_path));
    }
    fs::rename(partial, final_path)
        .map_err(|err| ReplayError::io("finalizing clip", final_path, err))?;
    Ok(bytes)
}

pub type ExportDone = Box<dyn FnOnce(Result<SavedClip, ReplayError>) + Send + 'static>;

struct Queued {
    job: ExportJob,
    done: ExportDone,
}

/// One background export thread with a bounded job queue.
pub struct Exporter {
    sender: Option<SyncSender<Queued>>,
    in_flight: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Exporter {
    /// `queue_capacity` jobs may wait behind the running one.
    pub fn spawn(queue_capacity: usize) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = {
            let in_flight = Arc::clone(&in_flight);
            let stopping = Arc::clone(&stopping);
            std::thread::Builder::new()
                .name("clipper-replay-export".into())
                .spawn(move || worker(receiver, in_flight, stopping))?
        };
        Ok(Self {
            sender: Some(sender),
            in_flight,
            stopping,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Never blocks. `Busy` when the queue is full. `done` runs on the export
    /// thread exactly once per accepted job (also on shutdown, with
    /// `Shutdown`).
    pub fn try_submit(&self, job: ExportJob, done: ExportDone) -> Result<(), ReplayError> {
        let Some(sender) = &self.sender else {
            return Err(ReplayError::Shutdown);
        };
        if self.stopping.load(Ordering::SeqCst) {
            return Err(ReplayError::Shutdown);
        }
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        match sender.try_send(Queued { job, done }) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                Err(match err {
                    TrySendError::Full(_) => ReplayError::Busy,
                    TrySendError::Disconnected(_) => ReplayError::Shutdown,
                })
            }
        }
    }

    /// Jobs accepted and not yet completed.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }
}

impl Drop for Exporter {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.sender = None;
        let handle = self.thread.get_mut().ok().and_then(Option::take);
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

fn worker(receiver: Receiver<Queued>, in_flight: Arc<AtomicUsize>, stopping: Arc<AtomicBool>) {
    while let Ok(Queued { job, done }) = receiver.recv() {
        let result = if stopping.load(Ordering::SeqCst) {
            Err(ReplayError::Shutdown)
        } else {
            catch_unwind(AssertUnwindSafe(|| export_clip(&job)))
                .unwrap_or_else(|_| Err(ReplayError::Invalid("export panicked".into())))
        };
        let _ = catch_unwind(AssertUnwindSafe(move || done(result)));
        in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hybrid::GopRef;
    use crate::plan::test_support::*;
    use std::sync::mpsc::channel;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clipper-export-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn job(final_path: PathBuf) -> ExportJob {
        ExportJob {
            snapshot: ReplaySnapshot {
                layout: layout(1),
                gops: simple_gops(1).into_iter().map(GopRef::Ram).collect(),
                requested: Duration::from_secs(2),
                end_pts: ms(6000),
            },
            final_path,
        }
    }

    fn no_partials(dir: &Path) -> bool {
        fs::read_dir(dir).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".partial")
        })
    }

    #[test]
    fn exports_and_reports_real_values() {
        let dir = temp_dir("ok");
        let path = dir.join("clip.mp4");
        let saved = export_clip(&job(path.clone())).unwrap();
        assert_eq!(saved.duration, Duration::from_secs(2));
        assert_eq!(saved.video_frames, 20);
        assert_eq!(saved.audio_tracks, 1);
        assert_eq!(saved.start_pts, ms(4000));
        assert_eq!(saved.bytes, fs::metadata(&path).unwrap().len());
        assert!(no_partials(&dir));
        let data = fs::read(&path).unwrap();
        assert_eq!(&data[4..8], b"ftyp");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_directory_fails_without_partial() {
        let dir = temp_dir("missing");
        let path = dir.join("nope").join("clip.mp4");
        assert!(export_clip(&job(path)).is_err());
        assert!(no_partials(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_overwrite_existing_clip() {
        let dir = temp_dir("exists");
        let path = dir.join("clip.mp4");
        fs::write(&path, b"precious").unwrap();
        assert!(export_clip(&job(path.clone())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"precious");
        assert!(no_partials(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_ready_leaves_nothing() {
        let dir = temp_dir("notready");
        let mut j = job(dir.join("clip.mp4"));
        j.snapshot.gops.clear();
        assert!(matches!(export_clip(&j), Err(ReplayError::NotReady)));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exporter_runs_jobs_and_calls_done_once() {
        let dir = temp_dir("exporter");
        let exporter = Exporter::spawn(4).unwrap();
        let (tx, rx) = channel();
        for i in 0..3 {
            let tx = tx.clone();
            exporter
                .try_submit(
                    job(dir.join(format!("c{i}.mp4"))),
                    Box::new(move |r| tx.send((i, r)).unwrap()),
                )
                .unwrap();
        }
        let mut seen = Vec::new();
        for _ in 0..3 {
            let (i, r) = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            assert!(r.is_ok());
            seen.push(i);
        }
        seen.sort();
        assert_eq!(seen, [0, 1, 2]);
        drop(exporter);
        assert!(rx.try_recv().is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Blocks the worker inside `done` of the first job.
    fn blocking_exporter(
        capacity: usize,
        dir: &Path,
    ) -> (Exporter, mpsc::Sender<()>, Receiver<()>) {
        let exporter = Exporter::spawn(capacity).unwrap();
        let (release_tx, release_rx) = channel::<()>();
        let (started_tx, started_rx) = channel::<()>();
        exporter
            .try_submit(
                job(dir.join("first.mp4")),
                Box::new(move |_| {
                    started_tx.send(()).unwrap();
                    let _ = release_rx.recv();
                }),
            )
            .unwrap();
        (exporter, release_tx, started_rx)
    }

    #[test]
    fn busy_when_queue_full() {
        let dir = temp_dir("busy");
        let (exporter, release, started) = blocking_exporter(1, &dir);
        started.recv_timeout(Duration::from_secs(20)).unwrap();
        let noop = || -> ExportDone { Box::new(|_| {}) };
        exporter.try_submit(job(dir.join("q.mp4")), noop()).unwrap();
        assert_eq!(exporter.in_flight(), 2);
        let err = exporter.try_submit(job(dir.join("r.mp4")), noop());
        assert!(matches!(err, Err(ReplayError::Busy)));
        assert_eq!(exporter.in_flight(), 2);
        release.send(()).unwrap();
        drop(exporter);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn queued_jobs_get_shutdown_on_drop() {
        let dir = temp_dir("shutdown");
        let (exporter, release, started) = blocking_exporter(3, &dir);
        started.recv_timeout(Duration::from_secs(20)).unwrap();
        let (tx, rx) = channel();
        for i in 0..2 {
            let tx = tx.clone();
            exporter
                .try_submit(
                    job(dir.join(format!("q{i}.mp4"))),
                    Box::new(move |r| tx.send(r).unwrap()),
                )
                .unwrap();
        }
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            release.send(()).unwrap();
        });
        drop(exporter);
        releaser.join().unwrap();
        for _ in 0..2 {
            let result = rx.try_recv().unwrap();
            assert!(matches!(result, Err(ReplayError::Shutdown)));
        }
        assert!(rx.try_recv().is_err());
        assert!(!dir.join("q0.mp4").exists() && !dir.join("q1.mp4").exists());
        assert!(no_partials(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_submissions_complete_exactly_once() {
        let dir = temp_dir("concurrent");
        let exporter = Arc::new(Exporter::spawn(64).unwrap());
        let completed = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let exporter = Arc::clone(&exporter);
                let completed = Arc::clone(&completed);
                let accepted = Arc::clone(&accepted);
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..4 {
                        let completed = Arc::clone(&completed);
                        let result = exporter.try_submit(
                            job(dir.join(format!("t{t}-{i}.mp4"))),
                            Box::new(move |r| {
                                assert!(r.is_ok());
                                completed.fetch_add(1, Ordering::SeqCst);
                            }),
                        );
                        if result.is_ok() {
                            accepted.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let exporter = Arc::try_unwrap(exporter).ok().unwrap();
        let start = std::time::Instant::now();
        while exporter.in_flight() > 0 && start.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(exporter);
        assert_eq!(accepted.load(Ordering::SeqCst), 16);
        assert_eq!(completed.load(Ordering::SeqCst), 16);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 16);
        let _ = fs::remove_dir_all(&dir);
    }
}
