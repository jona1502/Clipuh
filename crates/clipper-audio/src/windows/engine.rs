//! Public capture engine: plans routing, owns rings and one capture thread
//! per track, and hands packets to the consumer without blocking.
//!
//! Tracks can be added and removed while capture runs ([`AudioCapture::add_track`],
//! [`AudioCapture::remove_track`]). Track indexes are never reused within one
//! capture, so a late event or packet of a removed track can never be taken
//! for a newer one. Removing a track only signals its thread; the thread is
//! joined once it has exited, so the caller (the recorder thread) never waits
//! for a WASAPI shutdown.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows::Win32::Foundation::WAIT_TIMEOUT;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::WaitForSingleObject;

use super::stream::{ComGuard, LevelCell, OwnedHandle, StreamSource, StreamThread};
use super::{endpoint, process, sessions};
use crate::desktop;
use crate::ring::{AudioConsumer, RingStats};
use crate::routing::{self, PlannedTrack, RoutingPlan, RoutingWarning, StreamPlan};
use crate::types::{
    AudioConfig, AudioError, AudioEvent, AudioFrameRef, AudioLevels, AudioSelection, ProcessTarget,
    SAMPLE_RATE, TICKS_PER_SECOND, TrackInfo,
};

/// Final counters per track after `stop`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureSummary {
    pub frames: Vec<u64>,
    pub dropped_frames: Vec<u64>,
}

/// Capture that ended after a removal or a part change; the consumer
/// releases the matching mixer state (see [`AudioCapture::take_finished`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finished {
    /// One stream of `track` stopped and every packet of it was read.
    Input { track: usize, input: u64 },
    /// A removed track stopped and every packet of it was read.
    Track(usize),
}

/// Re-enumeration interval of a composed desktop remainder: a process that
/// opens its first audio session joins the desktop track within about this
/// long plus its activation time. Not a hot poll (COM calls once a second).
const COMPOSE_INTERVAL_MS: u32 = 1_000;

/// One capture stream's ring.
struct InputRing {
    key: u64,
    consumer: AudioConsumer,
    stats: Arc<RingStats>,
}

/// Thread(s) behind a track.
enum Worker {
    Stream(StreamThread),
    Composite(CompositeThread),
}

impl Worker {
    fn signal_stop(&self) {
        match self {
            Self::Stream(thread) => thread.signal_stop(),
            Self::Composite(thread) => thread.stop.signal(),
        }
    }

    fn is_finished(&self) -> bool {
        match self {
            Self::Stream(thread) => thread.is_finished(),
            Self::Composite(thread) => thread.thread.as_ref().is_none_or(JoinHandle::is_finished),
        }
    }
}

struct Slot {
    inputs: Vec<InputRing>,
    levels: Arc<LevelCell>,
    /// New part rings of a composed desktop track.
    new_inputs: Option<Receiver<InputRing>>,
    /// Audio a composed desktop track cannot contain (executable names).
    unseparable: Option<Arc<Mutex<Vec<String>>>>,
    /// `false` once removed: kept only until its rings are drained.
    active: bool,
}

/// Running audio capture: one dedicated thread per track.
pub struct AudioCapture {
    /// Indexed by track index; `None` once removed and drained.
    slots: Vec<Option<Slot>>,
    /// Infos of the active tracks, ascending index.
    tracks: Vec<TrackInfo>,
    warnings: Vec<(u32, RoutingWarning)>,
    /// Workers of active tracks, by track index.
    workers: Vec<(usize, Worker)>,
    /// Signalled workers waiting to exit; joined once finished.
    retiring: Vec<Worker>,
    finished: Vec<Finished>,
    events_tx: SyncSender<AudioEvent>,
    events: Receiver<AudioEvent>,
    wake: Option<SyncSender<()>>,
    config: AudioConfig,
    next_key: Arc<AtomicU64>,
    /// Counters of rings that no longer exist (monotonic totals).
    removed_frames: u64,
    removed_dropped: u64,
    /// Set by `stop`/`stop_and_drain`: no track may start afterwards.
    stopped: bool,
    started_at: i64,
}

/// Refuses a hand-made plan that needs process loopback on a Windows build
/// without it, instead of starting a thread that can only fail later. Plans
/// from [`routing::plan`] never trigger this (they drop such sources with a
/// warning).
fn check_supported(
    plan: &RoutingPlan,
    caps: crate::types::AudioCapabilities,
) -> Result<(), AudioError> {
    let needs_process = plan
        .tracks
        .iter()
        .any(|t| t.stream.needs_process_loopback());
    if needs_process && !caps.process_loopback {
        return Err(AudioError::Unsupported(
            RoutingWarning::ProcessLoopbackUnsupported
                .message()
                .to_owned(),
        ));
    }
    Ok(())
}

/// Current QPC time in 100 ns units.
pub fn qpc_now_ticks() -> i64 {
    let mut freq = 0i64;
    let mut counter = 0i64;
    // SAFETY: both pointers reference valid, live i64 locals.
    unsafe {
        let _ = QueryPerformanceFrequency(&mut freq);
        let _ = QueryPerformanceCounter(&mut counter);
    }
    if freq <= 0 {
        return 0;
    }
    (i128::from(counter) * i128::from(TICKS_PER_SECOND) / i128::from(freq)) as i64
}

impl AudioCapture {
    /// Plans routing for `selection` and starts it (no consumer wake-up).
    pub fn start(selection: &AudioSelection, config: AudioConfig) -> Result<Self, AudioError> {
        let plan = routing::plan(selection, process::capabilities());
        Self::start_plan(&plan, config, None)
    }

    /// Starts exactly `plan`, waking the consumer only when packets/events
    /// arrive. The callback must never block or allocate. A process source
    /// whose app is not running does not fail the start: its thread reports
    /// `SourceLost` and keeps retrying.
    pub fn start_plan(
        plan: &RoutingPlan,
        config: AudioConfig,
        wake: Option<SyncSender<()>>,
    ) -> Result<Self, AudioError> {
        if plan.tracks.is_empty() {
            return Err(AudioError::NothingToCapture);
        }
        check_supported(plan, process::capabilities())?;
        // Taken before the threads spawn so no packet predates the origin.
        let started_at = qpc_now_ticks();
        let (events_tx, events) = sync_channel(config.event_capacity.max(1));
        let mut this = Self {
            slots: Vec::new(),
            tracks: Vec::new(),
            warnings: plan.warnings.clone(),
            workers: Vec::new(),
            retiring: Vec::new(),
            finished: Vec::new(),
            events_tx,
            events,
            wake,
            config,
            next_key: Arc::new(AtomicU64::new(0)),
            removed_frames: 0,
            removed_dropped: 0,
            stopped: false,
            started_at,
        };
        for planned in &plan.tracks {
            // On error `this` is dropped, which stops every started thread.
            this.add_track(planned)?;
        }
        Ok(this)
    }

    /// Starts one more track while capture runs and returns its index (new,
    /// never reused). Its packets carry QPC times like every other track, so
    /// it joins the shared timeline from its first packet on. Never blocks on
    /// WASAPI: the stream opens on its own thread and reports
    /// `TrackStarted` / `SourceLost` / `Failed` through [`Self::events`].
    pub fn add_track(&mut self, planned: &PlannedTrack) -> Result<usize, AudioError> {
        if self.stopped {
            return Err(AudioError::SourceUnavailable(
                "audio capture already stopped".to_owned(),
            ));
        }
        if planned.stream.needs_process_loopback() && !process::capabilities().process_loopback {
            return Err(AudioError::Unsupported(
                RoutingWarning::ProcessLoopbackUnsupported
                    .message()
                    .to_owned(),
            ));
        }
        let index = self.slots.len();
        let levels = Arc::new(LevelCell::default());
        let (channels, worker, inputs, new_inputs, unseparable) = match &planned.stream {
            StreamPlan::DesktopComposite(excluded) => {
                let (tx, rx) = sync_channel(desktop::MAX_PARTS * 2);
                let unseparable = Arc::new(Mutex::new(Vec::new()));
                let thread = CompositeThread::spawn(CompositeSpec {
                    track: index,
                    excluded: excluded.clone(),
                    ring_ms: self.config.ring_ms,
                    parts: tx,
                    events: self.events_tx.clone(),
                    wake: self.wake.clone(),
                    keys: Arc::clone(&self.next_key),
                    unseparable: Arc::clone(&unseparable),
                })?;
                (
                    2,
                    Worker::Composite(thread),
                    Vec::new(),
                    Some(rx),
                    Some(unseparable),
                )
            }
            stream => {
                let (channels, source): (u16, Box<dyn StreamSource>) = match stream {
                    StreamPlan::EndpointLoopback(device) => {
                        (2, endpoint::selected_loopback_source(device.as_deref()))
                    }
                    StreamPlan::Microphone(choice) => (
                        endpoint::microphone_channels(choice),
                        endpoint::microphone_source(choice.clone()),
                    ),
                    StreamPlan::ProcessInclude(target) => {
                        (2, process::include_source(target.clone()))
                    }
                    StreamPlan::ProcessExclude(target) => {
                        (2, process::exclude_source(target.clone()))
                    }
                    StreamPlan::DesktopComposite(_) => unreachable!("handled above"),
                };
                let (producer, consumer) = crate::ring::audio_ring(channels, self.config.ring_ms);
                let stats = Arc::clone(consumer.stats());
                let thread = StreamThread::spawn_with_wake(
                    index,
                    channels,
                    source,
                    producer,
                    Arc::clone(&levels),
                    self.events_tx.clone(),
                    self.wake.clone(),
                )?;
                let key = self.next_key.fetch_add(1, Ordering::Relaxed);
                (
                    channels,
                    Worker::Stream(thread),
                    vec![InputRing {
                        key,
                        consumer,
                        stats,
                    }],
                    None,
                    None,
                )
            }
        };
        let info = TrackInfo {
            index,
            source: planned.source,
            content: planned.content.clone(),
            channels,
            sample_rate: SAMPLE_RATE,
        };
        self.slots.push(Some(Slot {
            inputs,
            levels,
            new_inputs,
            unseparable,
            active: true,
        }));
        self.workers.push((index, worker));
        self.tracks.push(info);
        Ok(index)
    }

    /// Stops track `index` without waiting for its thread. Packets it already
    /// captured are still delivered by [`Self::read_frames`]; once drained,
    /// [`Finished::Track`] is reported. Unknown or removed indexes are
    /// ignored. Returns whether a track was removed.
    pub fn remove_track(&mut self, index: usize) -> bool {
        let Some(position) = self.workers.iter().position(|(i, _)| *i == index) else {
            return false;
        };
        let (_, worker) = self.workers.swap_remove(position);
        worker.signal_stop();
        self.retiring.push(worker);
        self.tracks.retain(|t| t.index != index);
        if let Some(Some(slot)) = self.slots.get_mut(index) {
            slot.active = false;
        }
        true
    }

    /// Active tracks, ascending index.
    pub fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    pub fn warnings(&self) -> &[(u32, RoutingWarning)] {
        &self.warnings
    }

    /// Applications a composed desktop track cannot contain without
    /// duplicating a separated application (empty for other tracks).
    pub fn unseparable(&self, track: usize) -> Vec<String> {
        self.slot(track)
            .and_then(|slot| slot.unseparable.as_ref())
            .and_then(|names| names.lock().ok().map(|names| names.clone()))
            .unwrap_or_default()
    }

    /// Streams currently feeding `track` (a composed desktop has one per
    /// captured process).
    pub fn input_count(&self, track: usize) -> usize {
        self.slot(track).map_or(0, |slot| slot.inputs.len())
    }

    fn slot(&self, track: usize) -> Option<&Slot> {
        self.slots.get(track).and_then(Option::as_ref)
    }

    /// Capture that ended since the previous call.
    pub fn take_finished(&mut self) -> Vec<Finished> {
        std::mem::take(&mut self.finished)
    }

    /// Joins exited workers, adopts new composite parts and releases drained
    /// rings. Cheap; called from `read_frames`.
    fn maintain(&mut self) {
        self.retiring.retain(|worker| !worker.is_finished());
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let Some(current) = slot else { continue };
            // A composite's part channel disconnects once its thread exited.
            let mut parts_closed = true;
            if let Some(parts) = &current.new_inputs {
                loop {
                    match parts.try_recv() {
                        Ok(ring) => current.inputs.push(ring),
                        Err(TryRecvError::Empty) => {
                            parts_closed = false;
                            break;
                        }
                        Err(TryRecvError::Disconnected) => break,
                    }
                }
            }
            let mut i = 0;
            while i < current.inputs.len() {
                if current.inputs[i].consumer.is_finished() {
                    let ring = current.inputs.swap_remove(i);
                    self.removed_frames += ring.stats.frames.load(Ordering::Relaxed);
                    self.removed_dropped += ring.stats.dropped_frames.load(Ordering::Relaxed);
                    if current.active {
                        self.finished.push(Finished::Input {
                            track: index,
                            input: ring.key,
                        });
                    }
                } else {
                    i += 1;
                }
            }
            if !current.active && current.inputs.is_empty() && parts_closed {
                *slot = None;
                self.finished.push(Finished::Track(index));
            }
        }
    }

    /// Drains all available packets of all tracks, non-blocking. Returns the
    /// number of packets delivered.
    pub fn read_frames(&mut self, mut f: impl FnMut(AudioFrameRef<'_>)) -> usize {
        self.maintain();
        let mut count = 0;
        for (track, slot) in self.slots.iter_mut().enumerate() {
            let Some(slot) = slot else { continue };
            for ring in &mut slot.inputs {
                let channels = ring.consumer.channels();
                let input = ring.key;
                while ring
                    .consumer
                    .pop_with(|header, samples| {
                        f(AudioFrameRef {
                            track,
                            input,
                            timestamp: header.timestamp,
                            channels,
                            samples,
                            flags: header.flags,
                        });
                    })
                    .is_some()
                {
                    count += 1;
                }
            }
        }
        count
    }

    /// Latest peak/RMS of `track` (lock-free).
    pub fn levels(&self, track: usize) -> AudioLevels {
        self.slot(track)
            .map(|slot| slot.levels.load())
            .unwrap_or_default()
    }

    /// Highest peak of `track` since the previous call (lock-free); 0 when
    /// no audio arrived meanwhile. One reader per track (the meter).
    pub fn take_peak(&self, track: usize) -> f32 {
        self.slot(track).map_or(0.0, |slot| slot.levels.take_peak())
    }

    pub fn events(&self) -> &Receiver<AudioEvent> {
        &self.events
    }

    fn stop_threads(&mut self) {
        self.stopped = true;
        // Signal all first so the threads wind down in parallel.
        for (_, worker) in &self.workers {
            worker.signal_stop();
        }
        let workers: Vec<Worker> = self
            .workers
            .drain(..)
            .map(|(_, worker)| worker)
            .chain(self.retiring.drain(..))
            .collect();
        // Dropping joins (StreamThread and CompositeThread both join on drop).
        drop(workers);
    }

    /// Stops producers, then drains their remaining packets before finalization.
    pub fn stop_and_drain(&mut self, f: impl FnMut(AudioFrameRef<'_>)) -> CaptureSummary {
        self.stop_threads();
        self.read_frames(f);
        self.summary()
    }

    /// Latched even if the bounded event channel drops the failure notification.
    pub fn had_source_failure(&self) -> bool {
        self.tracks.iter().any(|t| self.track_failed(t.index))
    }

    /// Whether `track` ever lost its source or failed (latched, independent of
    /// the lossy event channel). `false` for an unknown track.
    pub fn track_failed(&self, track: usize) -> bool {
        self.slot(track)
            .is_some_and(|slot| slot.levels.source_failed())
    }

    /// Persistent counters do not depend on delivery of lossy/throttled events.
    /// Monotonic: includes rings that were already released.
    pub fn dropped_frames(&self) -> u64 {
        self.removed_dropped
            + self
                .slots
                .iter()
                .flatten()
                .flat_map(|slot| &slot.inputs)
                .map(|ring| ring.stats.dropped_frames.load(Ordering::Relaxed))
                .sum::<u64>()
    }

    fn summary(&self) -> CaptureSummary {
        let per_track = |field: fn(&RingStats) -> &AtomicU64| -> Vec<u64> {
            self.tracks
                .iter()
                .map(|t| {
                    self.slot(t.index).map_or(0, |slot| {
                        slot.inputs
                            .iter()
                            .map(|ring| field(&ring.stats).load(Ordering::Relaxed))
                            .sum()
                    })
                })
                .collect()
        };
        CaptureSummary {
            frames: per_track(|s| &s.frames),
            dropped_frames: per_track(|s| &s.dropped_frames),
        }
    }

    /// Stops all threads and returns counters. Unread packets are discarded.
    pub fn stop(mut self) -> CaptureSummary {
        self.stop_threads();
        self.summary()
    }

    /// QPC time (100 ns) taken just before the tracks started: timeline origin.
    pub fn started_at(&self) -> i64 {
        self.started_at
    }
}

impl crate::live::TrackBackend for AudioCapture {
    fn add_track(&mut self, planned: &PlannedTrack) -> Result<usize, AudioError> {
        AudioCapture::add_track(self, planned)
    }

    fn remove_track(&mut self, index: usize) -> bool {
        AudioCapture::remove_track(self, index)
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop_threads();
    }
}

struct CompositeSpec {
    track: usize,
    excluded: Vec<ProcessTarget>,
    ring_ms: u32,
    parts: SyncSender<InputRing>,
    events: SyncSender<AudioEvent>,
    wake: Option<SyncSender<()>>,
    keys: Arc<AtomicU64>,
    unseparable: Arc<Mutex<Vec<String>>>,
}

/// Desktop remainder without two or more applications ([`crate::desktop`]):
/// once a second it lists the audio sessions and the process table, then
/// starts or stops one process-loopback INCLUDE stream per process to
/// capture. Each part has its own ring, handed to the engine over `parts`.
struct CompositeThread {
    stop: Arc<OwnedHandle>,
    thread: Option<JoinHandle<()>>,
}

impl CompositeThread {
    fn spawn(spec: CompositeSpec) -> Result<Self, AudioError> {
        let stop = Arc::new(OwnedHandle::new_event(true)?);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(format!("clipper-desktop-{}", spec.track))
            .spawn(move || compose_loop(spec, &thread_stop))
            .map_err(|e| AudioError::Os(format!("spawn desktop thread: {e}")))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for CompositeThread {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.stop.signal();
            let _ = thread.join();
        }
    }
}

struct Part {
    pid: u32,
    thread: StreamThread,
}

fn compose_loop(spec: CompositeSpec, stop: &OwnedHandle) {
    let _com = ComGuard::init();
    // Part threads report here, not to the engine: a part whose process
    // exits is routine and must not mark the desktop track as lost.
    let (part_events_tx, part_events) = sync_channel::<AudioEvent>(64);
    let part_levels = Arc::new(LevelCell::default());
    let mut parts: Vec<Part> = Vec::new();
    let mut started = false;
    let mut lost = false;
    loop {
        while part_events.try_recv().is_ok() {}
        match sessions::session_pids() {
            Ok(session_pids) => {
                let processes = process::process_table();
                let composition = desktop::compose(&processes, &session_pids, &spec.excluded);
                let running: Vec<u32> = parts.iter().map(|p| p.pid).collect();
                let (stop_pids, start_pids) = desktop::part_changes(&running, &composition.include);
                // Dropping a part joins its thread; its ring then finishes.
                parts.retain(|p| !stop_pids.contains(&p.pid));
                for pid in start_pids {
                    let (producer, consumer) = crate::ring::audio_ring(2, spec.ring_ms);
                    let stats = Arc::clone(consumer.stats());
                    let Ok(thread) = StreamThread::spawn_with_wake(
                        spec.track,
                        2,
                        process::include_source(ProcessTarget::Pid(pid)),
                        producer,
                        Arc::clone(&part_levels),
                        part_events_tx.clone(),
                        spec.wake.clone(),
                    ) else {
                        continue;
                    };
                    let ring = InputRing {
                        key: spec.keys.fetch_add(1, Ordering::Relaxed),
                        consumer,
                        stats,
                    };
                    // Bounded: if the engine stopped reading, drop the part.
                    if spec.parts.try_send(ring).is_ok() {
                        parts.push(Part { pid, thread });
                    }
                }
                if let Ok(mut names) = spec.unseparable.lock() {
                    *names = composition.unseparable;
                }
                if !started || lost {
                    started = true;
                    lost = false;
                    let _ = spec
                        .events
                        .try_send(AudioEvent::TrackStarted { track: spec.track });
                    if let Some(wake) = &spec.wake {
                        let _ = wake.try_send(());
                    }
                }
            }
            Err(error) => {
                if !lost {
                    lost = true;
                    let _ = spec.events.try_send(AudioEvent::SourceLost {
                        track: spec.track,
                        reason: format!("audio sessions could not be listed: {error}"),
                    });
                }
            }
        }
        // SAFETY: `stop` is a valid event handle owned by the caller's Arc.
        let wait = unsafe { WaitForSingleObject(stop.0, COMPOSE_INTERVAL_MS) };
        if wait != WAIT_TIMEOUT {
            break;
        }
    }
    // Signal all parts first so they wind down in parallel, then join.
    for part in &parts {
        part.thread.signal_stop();
    }
    for part in parts {
        part.thread.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AudioCapabilities, ProcessTarget, TrackContent};

    fn process_plan() -> RoutingPlan {
        RoutingPlan {
            tracks: vec![routing::PlannedTrack {
                source: 1,
                content: TrackContent::Application("a.exe".into()),
                stream: StreamPlan::ProcessInclude(ProcessTarget::Executable("a.exe".into())),
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn process_plan_is_refused_without_process_loopback() {
        let legacy = AudioCapabilities {
            process_loopback: false,
        };
        let modern = AudioCapabilities {
            process_loopback: true,
        };
        assert!(matches!(
            check_supported(&process_plan(), legacy),
            Err(AudioError::Unsupported(_))
        ));
        assert!(check_supported(&process_plan(), modern).is_ok());
        let endpoint = RoutingPlan {
            tracks: vec![routing::PlannedTrack {
                source: 1,
                content: TrackContent::DesktopFull,
                stream: StreamPlan::EndpointLoopback(None),
            }],
            warnings: Vec::new(),
        };
        assert!(check_supported(&endpoint, legacy).is_ok());
    }

    #[test]
    fn unavailable_endpoint_failure_stays_latched_without_consuming_notifications() {
        let plan = RoutingPlan {
            tracks: vec![routing::PlannedTrack {
                source: 1,
                content: crate::types::TrackContent::DesktopFull,
                stream: StreamPlan::EndpointLoopback(Some(
                    "clipper-nonexistent-test-endpoint".to_owned(),
                )),
            }],
            warnings: Vec::new(),
        };
        let capture = AudioCapture::start_plan(
            &plan,
            AudioConfig {
                event_capacity: 1,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !capture.had_source_failure() {
            assert!(
                std::time::Instant::now() < deadline,
                "source failure was not latched"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        while capture.events().try_recv().is_ok() {}
        assert!(capture.had_source_failure());
        capture.stop();
    }
}
