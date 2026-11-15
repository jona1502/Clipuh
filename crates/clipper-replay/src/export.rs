//! Clip export: snapshot -> keyframe-aligned plan -> MP4 remux (no
//! re-encoding) -> `<name>.mp4.partial` -> flush -> sync -> structural check
//! -> commit under the final name.
//!
//! Only `Ok` from [`export_clip`] means a complete, finalized clip exists.
//! The final path is never overwritten (the caller picks unique names): the
//! commit ([`crate::publish::commit_no_replace`]) fails if the name exists.

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

/// File system operations of one export. Production uses [`StdFs`]; tests
/// inject failures at each stage (create, write, flush, sync, commit).
pub(crate) trait ExportFs {
    type File: Write;
    /// Creates `path`, failing if it exists.
    fn create_new(&self, path: &Path) -> io::Result<Self::File>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// Flushes the contents to the device and closes the file.
    fn sync_and_close(&self, file: Self::File) -> io::Result<()>;
    /// Publishes `partial` as `final_path`, never replacing a file.
    fn commit(&self, partial: &Path, final_path: &Path) -> io::Result<()>;
}

pub(crate) struct StdFs;

impl ExportFs for StdFs {
    type File = File;

    fn create_new(&self, path: &Path) -> io::Result<File> {
        OpenOptions::new().write(true).create_new(true).open(path)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn sync_and_close(&self, file: File) -> io::Result<()> {
        file.sync_all()
    }

    fn commit(&self, partial: &Path, final_path: &Path) -> io::Result<()> {
        crate::publish::commit_no_replace(partial, final_path)
    }
}

/// Removes the partial file unless the clip was committed, also when the
/// export panics (the worker catches the panic), so no `.partial` of this
/// process outlives its job.
struct PartialGuard<'a, F: ExportFs> {
    fs: &'a F,
    path: &'a Path,
    committed: bool,
}

impl<F: ExportFs> Drop for PartialGuard<'_, F> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.fs.remove(self.path);
        }
    }
}

fn create_partial<F: ExportFs>(fs: &F, partial: &Path) -> Result<F::File, ReplayError> {
    match fs.create_new(partial) {
        Ok(file) => Ok(file),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            // Stale partial of a crashed run: unique clip names make this
            // file ours to replace.
            fs.remove(partial)
                .and_then(|()| fs.create_new(partial))
                .map_err(|err| ReplayError::io("creating partial clip", partial, err))
        }
        Err(err) => Err(ReplayError::io("creating partial clip", partial, err)),
    }
}

/// Writes one clip synchronously. Only `Ok` means a complete, finalized,
/// checked and committed file exists under `job.final_path`; on error (or a
/// panic) no partial file is left behind and no existing file is changed.
pub fn export_clip(job: &ExportJob) -> Result<SavedClip, ReplayError> {
    export_clip_with(job, &StdFs)
}

pub(crate) fn export_clip_with<F: ExportFs>(
    job: &ExportJob,
    fs: &F,
) -> Result<SavedClip, ReplayError> {
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
    let file = create_partial(fs, &partial)?;
    let mut guard = PartialGuard {
        fs,
        path: &partial,
        committed: false,
    };
    let bytes = write_and_finalize(fs, file, &partial, final_path, snapshot, &plan)?;
    guard.committed = true;
    Ok(SavedClip {
        path: final_path.clone(),
        duration: ticks_to_duration(plan.duration),
        requested: snapshot.requested,
        start_pts: plan.start_pts,
        video_frames: plan.video.samples.len() as u64,
        audio_tracks: plan.audio.len(),
        bytes,
    })
}

/// Write -> flush -> sync -> check -> commit. Each step's failure is final for
/// this job (the caller's guard removes the partial).
fn write_and_finalize<F: ExportFs>(
    fs: &F,
    file: F::File,
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
    fs.sync_and_close(file)
        .map_err(|err| ReplayError::io("syncing clip", partial, err))?;
    // What reached the disk must be a finalized MP4 (cheap: box headers only).
    match crate::publish::check_mp4(partial) {
        Ok(Ok(())) => {}
        Ok(Err(incomplete)) => return Err(ReplayError::Incomplete(incomplete.0)),
        Err(err) => return Err(ReplayError::io("checking clip", partial, err)),
    }
    fs.commit(partial, final_path).map_err(|err| {
        if err.kind() == io::ErrorKind::AlreadyExists {
            exists_error(final_path)
        } else {
            ReplayError::io("finalizing clip", final_path, err)
        }
    })?;
    Ok(bytes)
}

pub type ExportDone = Box<dyn FnOnce(Result<SavedClip, ReplayError>) + Send + 'static>;

/// Runs on the export thread right before the clip is written: creates the
/// folder, picks a unique file name (may rewrite `final_path`).
pub type ExportPrepare = Box<dyn FnOnce(&mut ExportJob) -> Result<(), ReplayError> + Send>;

/// Housekeeping that must not run on the caller's thread.
pub type ExportTask = Box<dyn FnOnce() + Send + 'static>;

enum Queued {
    Export {
        job: ExportJob,
        prepare: Option<ExportPrepare>,
        done: ExportDone,
    },
    Task(ExportTask),
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
        self.try_submit_with(job, None, done)
    }

    /// Like [`Exporter::try_submit`]; `prepare` runs on the export thread
    /// before writing (file system work such as folder creation and name
    /// selection). Its error is reported through `done`.
    pub fn try_submit_with(
        &self,
        job: ExportJob,
        prepare: Option<ExportPrepare>,
        done: ExportDone,
    ) -> Result<(), ReplayError> {
        let Some(sender) = &self.sender else {
            return Err(ReplayError::Shutdown);
        };
        if self.stopping.load(Ordering::SeqCst) {
            return Err(ReplayError::Shutdown);
        }
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        match sender.try_send(Queued::Export { job, prepare, done }) {
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

    /// Runs `task` on the export thread (best effort: `Busy` when the queue is
    /// full). Not counted in [`Exporter::in_flight`].
    pub fn try_run(&self, task: ExportTask) -> Result<(), ReplayError> {
        let Some(sender) = &self.sender else {
            return Err(ReplayError::Shutdown);
        };
        sender
            .try_send(Queued::Task(task))
            .map_err(|err| match err {
                TrySendError::Full(_) => ReplayError::Busy,
                TrySendError::Disconnected(_) => ReplayError::Shutdown,
            })
    }

    /// Waits until every accepted export finished, at most `deadline`.
    /// Returns `true` when nothing is left. Jobs still queued afterwards get
    /// `Shutdown` when the exporter is dropped.
    pub fn finish_queued(&self, deadline: Duration) -> bool {
        let start = std::time::Instant::now();
        while self.in_flight() > 0 {
            if start.elapsed() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
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
    while let Ok(queued) = receiver.recv() {
        let (mut job, prepare, done) = match queued {
            Queued::Task(task) => {
                if !stopping.load(Ordering::SeqCst) {
                    let _ = catch_unwind(AssertUnwindSafe(task));
                }
                continue;
            }
            Queued::Export { job, prepare, done } => (job, prepare, done),
        };
        let result = if stopping.load(Ordering::SeqCst) {
            Err(ReplayError::Shutdown)
        } else {
            catch_unwind(AssertUnwindSafe(|| {
                if let Some(prepare) = prepare {
                    prepare(&mut job)?;
                }
                export_clip(&job)
            }))
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
    fn finish_queued_lets_accepted_jobs_complete_instead_of_shutdown() {
        let dir = temp_dir("finish");
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
        // Too short while the first job is blocked.
        assert!(!exporter.finish_queued(Duration::from_millis(50)));
        release.send(()).unwrap();
        assert!(exporter.finish_queued(Duration::from_secs(20)));
        drop(exporter);
        for _ in 0..2 {
            assert!(rx.try_recv().unwrap().is_ok());
        }
        assert!(dir.join("q0.mp4").exists() && dir.join("q1.mp4").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_runs_on_the_export_thread_and_can_rewrite_the_path() {
        let dir = temp_dir("prepare");
        let exporter = Exporter::spawn(2).unwrap();
        let caller = std::thread::current().id();
        let (tx, rx) = channel();
        let target = dir.join("made").join("renamed.mp4");
        let expected = target.clone();
        exporter
            .try_submit_with(
                job(dir.join("original.mp4")),
                Some(Box::new(move |job| {
                    assert_ne!(std::thread::current().id(), caller);
                    fs::create_dir_all(target.parent().unwrap()).unwrap();
                    job.final_path = target;
                    Ok(())
                })),
                Box::new(move |r| tx.send(r).unwrap()),
            )
            .unwrap();
        let saved = rx.recv_timeout(Duration::from_secs(20)).unwrap().unwrap();
        assert_eq!(saved.path, expected);
        let (tx, rx) = channel();
        exporter
            .try_run(Box::new(move || {
                tx.send(std::thread::current().id()).unwrap()
            }))
            .unwrap();
        assert_ne!(rx.recv_timeout(Duration::from_secs(20)).unwrap(), caller);
        drop(exporter);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_interval_saved_twice_gives_two_independent_clips() {
        let dir = temp_dir("twice");
        let exporter = Exporter::spawn(4).unwrap();
        let snapshot = job(dir.join("unused.mp4")).snapshot;
        let (tx, rx) = channel();
        for name in ["a.mp4", "b.mp4"] {
            let tx = tx.clone();
            let job = ExportJob {
                snapshot: snapshot.clone(),
                final_path: dir.join(name),
            };
            exporter
                .try_submit(job, Box::new(move |r| tx.send(r).unwrap()))
                .unwrap();
        }
        for _ in 0..2 {
            assert!(rx.recv_timeout(Duration::from_secs(20)).unwrap().is_ok());
        }
        let a = fs::read(dir.join("a.mp4")).unwrap();
        assert!(!a.is_empty());
        assert_eq!(a, fs::read(dir.join("b.mp4")).unwrap());
        drop(exporter);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_save_does_not_hold_up_the_next_one() {
        let dir = temp_dir("fail-then-ok");
        let exporter = Exporter::spawn(4).unwrap();
        let (tx, rx) = channel();
        for path in [dir.join("missing").join("x.mp4"), dir.join("ok.mp4")] {
            let tx = tx.clone();
            exporter
                .try_submit(job(path), Box::new(move |r| tx.send(r).unwrap()))
                .unwrap();
        }
        assert!(rx.recv_timeout(Duration::from_secs(20)).unwrap().is_err());
        assert!(rx.recv_timeout(Duration::from_secs(20)).unwrap().is_ok());
        assert!(exporter.finish_queued(Duration::from_secs(20)));
        assert_eq!(exporter.in_flight(), 0);
        assert!(dir.join("ok.mp4").exists() && no_partials(&dir));
        drop(exporter);
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

    // ------------------------------------------------ fault injection --
    //
    // `FaultFs` runs the production export (`export_clip_with`) on real files
    // in a temporary folder, failing one stage on purpose. Invariant checked
    // everywhere: `Ok` <=> exactly one new, complete clip under the final
    // name; `Err` => no new file at all (no clip, no partial) and every file
    // that existed before is unchanged.

    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Fault {
        None,
        Create(io::ErrorKind),
        /// The file accepts this many bytes, then fails like this.
        Write(u64, WriteFault),
        Flush,
        Sync,
        /// The commit fails this many times with a transient error first.
        CommitTransient(u32),
        Commit(io::ErrorKind),
        /// Another process creates the final name right before the commit.
        NameTaken,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum WriteFault {
        DiskFull,
        Zero,
        Other,
        /// Bytes beyond the budget vanish but writes report success.
        SilentTruncate,
        /// Bytes beyond the budget are written as zeros (same length, damaged
        /// content, as from a faulty filter driver or device).
        Corrupt,
        Panic,
    }

    struct FaultFs {
        fault: Fault,
        commit_failures: Cell<u32>,
    }

    impl FaultFs {
        fn new(fault: Fault) -> Self {
            Self {
                fault,
                commit_failures: Cell::new(0),
            }
        }
    }

    struct FaultFile {
        file: File,
        budget: u64,
        fault: Option<WriteFault>,
        fail_flush: bool,
    }

    impl Write for FaultFile {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let Some(fault) = self.fault else {
                return self.file.write(buf);
            };
            if self.budget == 0 {
                return match fault {
                    WriteFault::DiskFull => Err(io::Error::from(io::ErrorKind::StorageFull)),
                    WriteFault::Zero => Ok(0),
                    WriteFault::Other => Err(io::Error::other("device not ready")),
                    WriteFault::SilentTruncate => Ok(buf.len()),
                    WriteFault::Corrupt => self.file.write(&vec![0; buf.len()]),
                    WriteFault::Panic => panic!("injected panic while writing"),
                };
            }
            let take = buf.len().min(self.budget as usize);
            let written = self.file.write(&buf[..take])?;
            self.budget -= written as u64;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                return Err(io::Error::other("flush failed"));
            }
            self.file.flush()
        }
    }

    impl ExportFs for FaultFs {
        type File = FaultFile;

        fn create_new(&self, path: &Path) -> io::Result<FaultFile> {
            if let Fault::Create(kind) = self.fault {
                return Err(io::Error::from(kind));
            }
            let (budget, fault) = match self.fault {
                Fault::Write(budget, fault) => (budget, Some(fault)),
                _ => (0, None),
            };
            Ok(FaultFile {
                file: StdFs.create_new(path)?,
                budget,
                fault,
                fail_flush: self.fault == Fault::Flush,
            })
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            StdFs.remove(path)
        }

        fn sync_and_close(&self, file: FaultFile) -> io::Result<()> {
            if self.fault == Fault::Sync {
                return Err(io::Error::other("sync failed"));
            }
            StdFs.sync_and_close(file.file)
        }

        fn commit(&self, partial: &Path, final_path: &Path) -> io::Result<()> {
            match self.fault {
                Fault::Commit(kind) => Err(io::Error::from(kind)),
                Fault::NameTaken => {
                    fs::write(final_path, b"another program's file").unwrap();
                    StdFs.commit(partial, final_path)
                }
                Fault::CommitTransient(times) => {
                    // The production retry policy, without its sleeps.
                    crate::publish::retry_transient(
                        || {
                            if self.commit_failures.get() < times {
                                self.commit_failures.set(self.commit_failures.get() + 1);
                                Err(io::Error::from(io::ErrorKind::PermissionDenied))
                            } else {
                                StdFs.commit(partial, final_path)
                            }
                        },
                        |_| {},
                    )
                }
                _ => StdFs.commit(partial, final_path),
            }
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A folder with spaces and non-ASCII characters holding one older clip
    /// that must survive every failure.
    fn fault_dir(name: &str) -> PathBuf {
        let dir = temp_dir(&format!("faults {name} Clips ü"));
        fs::write(dir.join("Clipper_Replay_older.mp4"), b"older clip").unwrap();
        dir
    }

    fn clip_size() -> u64 {
        let dir = temp_dir("size");
        let saved = export_clip(&job(dir.join("clip.mp4"))).unwrap();
        let _ = fs::remove_dir_all(&dir);
        saved.bytes
    }

    /// Runs one export with `fault` and checks the invariant.
    fn run(dir: &Path, name: &str, fault: Fault) -> Result<SavedClip, ReplayError> {
        let before = names(dir);
        let final_path = dir.join(name);
        let result = export_clip_with(&job(final_path.clone()), &FaultFs::new(fault));
        let after = names(dir);
        assert_eq!(
            fs::read(dir.join("Clipper_Replay_older.mp4")).unwrap(),
            b"older clip",
            "{fault:?}: an existing clip was changed"
        );
        assert!(
            no_partials(dir),
            "{fault:?}: a partial file was left: {after:?}"
        );
        match &result {
            Ok(saved) => {
                let mut expected = before.clone();
                expected.push(name.to_owned());
                expected.sort();
                assert_eq!(after, expected, "{fault:?}: exactly one new file");
                assert_eq!(fs::metadata(&final_path).unwrap().len(), saved.bytes);
                assert_eq!(crate::publish::check_mp4(&final_path).unwrap(), Ok(()));
            }
            Err(error) => {
                let taken = fault == Fault::NameTaken;
                if !taken {
                    assert_eq!(after, before, "{fault:?} ({error}): files changed");
                }
                if !taken {
                    assert!(!final_path.exists(), "{fault:?}: published despite {error}");
                }
            }
        }
        result
    }

    /// TEST A: every stage succeeds -> exactly one complete clip.
    #[test]
    fn a_successful_save_publishes_one_checked_clip() {
        let dir = fault_dir("ok");
        let saved = run(&dir, "Clipper_Replay ü.mp4", Fault::None).unwrap();
        assert_eq!(saved.path, dir.join("Clipper_Replay ü.mp4"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// TEST B: the partial cannot be created (no permission).
    #[test]
    fn b_create_failure_reports_access_denied() {
        let dir = fault_dir("create");
        let error = run(
            &dir,
            "c.mp4",
            Fault::Create(io::ErrorKind::PermissionDenied),
        )
        .unwrap_err();
        assert!(matches!(error, ReplayError::AccessDenied { .. }), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// TEST C + D: the device fails or fills up while the clip is written.
    #[test]
    fn c_d_write_failures_publish_nothing() {
        let dir = fault_dir("write");
        let size = clip_size();
        for budget in [0, 100, size / 2] {
            let error = run(&dir, "w.mp4", Fault::Write(budget, WriteFault::DiskFull)).unwrap_err();
            assert!(
                matches!(error, ReplayError::DiskFull { .. }),
                "disk full at {budget}: {error}"
            );
            assert_eq!(
                error.to_string(),
                format!(
                    "not enough disk space at {}",
                    dir.join("w.mp4.partial").display()
                )
            );
            let error = run(&dir, "w.mp4", Fault::Write(budget, WriteFault::Zero)).unwrap_err();
            assert!(
                error.to_string().starts_with("flushing clip"),
                "write zero: {error}"
            );
            let error = run(&dir, "w.mp4", Fault::Write(budget, WriteFault::Other)).unwrap_err();
            assert!(error.to_string().contains("device not ready"), "{error}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// TEST E: the failure hits the container's final index (`moov` is written
    /// last), so the media data is complete but the container is not.
    #[test]
    fn e_failure_while_finalizing_the_container() {
        let dir = fault_dir("finalize");
        let size = clip_size();
        let error = run(&dir, "e.mp4", Fault::Write(size - 40, WriteFault::DiskFull)).unwrap_err();
        assert!(matches!(error, ReplayError::DiskFull { .. }), "{error}");
        // A writer that loses the tail silently is caught by the final check.
        let error = run(
            &dir,
            "e.mp4",
            Fault::Write(size - 40, WriteFault::SilentTruncate),
        )
        .unwrap_err();
        assert!(matches!(error, ReplayError::Incomplete(_)), "{error}");
        assert!(
            error
                .to_string()
                .starts_with("the clip could not be finalized")
        );
        // Damaged content of the right length is caught as well.
        let error = run(&dir, "e.mp4", Fault::Write(0, WriteFault::Corrupt)).unwrap_err();
        assert!(matches!(error, ReplayError::Incomplete(_)), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// TEST F: flush or sync to the device fails.
    #[test]
    fn f_flush_and_sync_failures_are_no_success() {
        let dir = fault_dir("flush");
        let error = run(&dir, "f.mp4", Fault::Flush).unwrap_err();
        assert!(error.to_string().starts_with("flushing clip"), "{error}");
        let error = run(&dir, "f.mp4", Fault::Sync).unwrap_err();
        assert!(error.to_string().starts_with("syncing clip"), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// TEST G: the commit fails for good, or briefly (a scanner holds the file).
    #[test]
    fn g_commit_failures_and_transient_locks() {
        let dir = fault_dir("commit");
        let error = run(&dir, "g.mp4", Fault::Commit(io::ErrorKind::NotFound)).unwrap_err();
        assert!(error.to_string().starts_with("finalizing clip"), "{error}");
        let error = run(&dir, "g.mp4", Fault::CommitTransient(10)).unwrap_err();
        assert!(matches!(error, ReplayError::AccessDenied { .. }), "{error}");
        // Locked twice, then free: the bounded retry publishes the clip.
        run(&dir, "g.mp4", Fault::CommitTransient(2)).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    /// TEST H: another program takes the final name between name selection
    /// and commit. Its file is kept untouched, our clip is not published.
    #[test]
    fn h_name_taken_at_commit_never_overwrites() {
        let dir = fault_dir("taken");
        let error = run(&dir, "h.mp4", Fault::NameTaken).unwrap_err();
        assert!(
            error.to_string().starts_with("clip already exists"),
            "{error}"
        );
        assert_eq!(
            fs::read(dir.join("h.mp4")).unwrap(),
            b"another program's file"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// TEST J: a panic in the middle of writing (a bug, not an I/O error)
    /// leaves no partial behind; the worker turns it into an error.
    #[test]
    fn j_panic_while_writing_removes_the_partial() {
        let dir = fault_dir("panic");
        let path = dir.join("j.mp4");
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            export_clip_with(
                &job(path.clone()),
                &FaultFs::new(Fault::Write(100, WriteFault::Panic)),
            )
        }));
        assert!(outcome.is_err());
        assert!(no_partials(&dir) && !path.exists(), "{:?}", names(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    /// TEST N: the destination is not a folder.
    #[test]
    fn n_invalid_destination_fails_cleanly() {
        let dir = fault_dir("invalid");
        let not_a_dir = dir.join("Clipper_Replay_older.mp4");
        let error = export_clip(&job(not_a_dir.join("n.mp4"))).unwrap_err();
        assert!(
            error.to_string().starts_with("creating partial clip"),
            "{error}"
        );
        assert_eq!(names(&dir), vec!["Clipper_Replay_older.mp4"]);
        assert_eq!(fs::read(&not_a_dir).unwrap(), b"older clip");
        let _ = fs::remove_dir_all(&dir);
    }

    /// TEST O: 600 saves cycling through every fault. Results are independent:
    /// successes accumulate exactly, failures leave nothing.
    #[test]
    fn o_repeated_saves_never_leak_state() {
        let dir = fault_dir("repeat");
        let size = clip_size();
        let faults = [
            Fault::None,
            Fault::Create(io::ErrorKind::PermissionDenied),
            Fault::Write(size / 3, WriteFault::DiskFull),
            Fault::Write(size - 8, WriteFault::SilentTruncate),
            Fault::Flush,
            Fault::Sync,
            Fault::Commit(io::ErrorKind::Other),
            Fault::CommitTransient(1),
            Fault::CommitTransient(9),
        ];
        let mut published = 0;
        for round in 0..600 {
            let fault = faults[round % faults.len()];
            let result = run(&dir, &format!("Clipper_Replay_{round:04}.mp4"), fault);
            let should_succeed = matches!(fault, Fault::None | Fault::CommitTransient(1));
            assert_eq!(
                result.is_ok(),
                should_succeed,
                "round {round}: {fault:?} -> {result:?}"
            );
            published += usize::from(result.is_ok());
        }
        assert_eq!(names(&dir).len(), published + 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
