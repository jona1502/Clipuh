//! Windows recorder worker: WGC capture -> pacing -> Media Foundation encoder.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{
    self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, sync_channel,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clipper_capture::windows::{CaptureSession, CaptureSignal, list_sources};
use clipper_core::pipeline::{FramePacer, RateMeter, recording_file_name};
use clipper_core::recording::{
    CaptureSource, REPLAY_NOT_RUNNING, RecorderCommand, RecorderEndpoint, RecorderEvent,
    RecordingOptions, RecordingStats, ReplayOptions, StopReason,
};
use clipper_encoder::backends::media_foundation::{
    EncoderConfig, EncoderError, HardwareEncoder, MediaFoundation, SubmitOutcome,
};
use clipper_platform::windows::{ComApartment, D3DContext};
use clipper_replay::error::ReplayError;
use clipper_replay::export::{ExportJob, ExportPrepare, Exporter, SavedClip};
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::logic::{
    ClipNames, Counters, Lifecycle, Published, StopOutcome, classify_stop, fit_output_size,
    partial_path, publish_recording, replay_failure_text, replay_file_name, sweep_stale_partials,
    unique_file_name, validate_replay_options,
};
use crate::recording_audio::{AudioPlan, AudioReporter, AudioTest, RecordingAudio};
use crate::replay_session::{ReplaySession, WAKE_WAIT};
use crate::{emit, log};

const MAX_WAIT: Duration = Duration::from_millis(250);
const STATS_WINDOW: Duration = Duration::from_secs(1);
/// Clips that may wait behind the running export.
const EXPORT_QUEUE: usize = 3;
/// At shutdown, accepted saves get this long to finish before they fail.
const SHUTDOWN_EXPORT_WAIT: Duration = Duration::from_secs(10);
/// Stale `Clipper_Replay_*.mp4.partial` files older than this are removed.
const STALE_PARTIAL_AGE: Duration = Duration::from_secs(3600);

struct Recording {
    // Field order matters for implicit drops: capture first, then encoder,
    // then the device.
    session: Option<CaptureSession>,
    encoder: HardwareEncoder,
    audio: Option<RecordingAudio>,
    audio_failed: bool,
    video_end: i64,
    audio_reporter: AudioReporter,
    _d3d: D3DContext,
    pacer: FramePacer,
    meter: RateMeter,
    counters: Counters,
    started: Instant,
    final_path: PathBuf,
    partial: PathBuf,
    /// Source label of each audio track, in encoder order (file metadata).
    audio_labels: Vec<String>,
    wake: Receiver<()>,
    source_closed: Arc<AtomicBool>,
}

impl Recording {
    fn stats(&self) -> RecordingStats {
        self.counters
            .snapshot(self.started.elapsed(), self.meter.rate())
    }

    /// Moves every pending capture frame through pacer and encoder.
    fn pump(&mut self) -> Result<(), String> {
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        // Bound each video batch: continuous WGC delivery must not starve
        // the independent WASAPI rings or command processing.
        for _ in 0..4 {
            let Some(frame) = session
                .try_next_frame()
                .map_err(|error| error.to_string())?
            else {
                break;
            };
            self.counters.captured += 1;
            let Some(sample_time) = self.pacer.accept(frame.timestamp()) else {
                continue;
            };
            // A frame delivered late after a declared WGC gap must not move
            // the muxer timeline backwards.
            if sample_time < self.video_end - self.pacer.interval() {
                continue;
            }
            let outcome = self
                .encoder
                .submit(
                    frame.texture(),
                    frame.content_size(),
                    sample_time,
                    self.pacer.interval(),
                )
                .map_err(|error| encoder_message(&error))?;
            match outcome {
                SubmitOutcome::Submitted => {
                    self.counters.encoded += 1;
                    self.meter.record();
                    self.video_end = sample_time + self.pacer.interval();
                }
                SubmitOutcome::Dropped => self.counters.dropped += 1,
            }
            // `frame` drops here, returning its slot to the WGC pool.
        }
        if let Some(audio) = &self.audio {
            let due = clipper_audio::windows::qpc_now_ticks() - audio.origin() - 800_000;
            if due - self.video_end > 10_000_000 {
                self.audio_failed = true;
                return Err("Audio/video clock gap exceeded the bounded recovery window.".into());
            }
            // WGC is change-driven. Mark missing frames instead of letting
            // the synchronous sink writer stall audio while a scene is static.
            while self.video_end + self.pacer.interval() < due {
                self.encoder
                    .video_gap(self.video_end)
                    .map_err(|error| encoder_message(&error))?;
                self.video_end += self.pacer.interval();
            }
        }
        if let Some(audio) = &mut self.audio
            && let Err(message) = audio.pump(&mut self.encoder, false, self.video_end)
        {
            self.audio_failed = true;
            return Err(message);
        }
        Ok(())
    }
}

struct Worker<'a> {
    endpoint: &'a RecorderEndpoint,
    lifecycle: Lifecycle,
    init_error: Option<String>,
    // Field order is drop order: the recording and Media Foundation must be
    // released before the COM apartment is left.
    recording: Option<Recording>,
    replay: Option<ReplaySession>,
    audio_test: Option<AudioTest>,
    /// Spawned on the first save and kept across replay sessions, so
    /// stopping the replay never cancels a running export.
    exporter: Option<Exporter>,
    /// Hands export results to the UI. Declared after `exporter`, whose queued
    /// jobs hold its senders, so it is dropped (joined) after it.
    completions: Option<Completions>,
    /// Id of the next save request, for the logs.
    next_save: u64,
    /// Clip names of queued and running exports.
    clip_names: ClipNames,
    /// Folder of the running replay's clips.
    replay_output: PathBuf,
    /// Set before shutdown joins the exporter: late completion events must
    /// not block on a UI queue nobody drains.
    shutting_down: Arc<AtomicBool>,
    media_foundation: Option<MediaFoundation>,
    _com: Option<ComApartment>,
}

pub fn run(endpoint: &RecorderEndpoint) {
    let (com, init_error) = match ComApartment::init_multithreaded() {
        Ok(com) => (Some(com), None),
        Err(error) => (None, Some(format!("COM initialization failed: {error}"))),
    };
    if let Some(message) = &init_error {
        emit(
            endpoint,
            RecorderEvent::Error {
                message: message.clone(),
            },
        );
    }
    let mut worker = Worker {
        endpoint,
        lifecycle: Lifecycle::new(),
        init_error,
        recording: None,
        replay: None,
        audio_test: None,
        exporter: None,
        completions: None,
        next_save: 1,
        clip_names: ClipNames::default(),
        replay_output: PathBuf::new(),
        shutting_down: Arc::new(AtomicBool::new(false)),
        media_foundation: None,
        _com: com,
    };

    let mut running = true;
    while running {
        if worker.recording.is_none() && worker.audio_test.is_none() && worker.replay.is_none() {
            match endpoint.commands.recv() {
                Ok(command) => running = worker.handle(command),
                Err(_) => break,
            }
            continue;
        }
        worker.service_recording();
        worker.service_replay();
        worker.service_audio_test();
        loop {
            match endpoint.commands.try_recv() {
                Ok(command) => {
                    if !worker.handle(command) {
                        running = false;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    running = false;
                    break;
                }
            }
        }
    }
    worker.stop_recording(StopReason::Shutdown, None);
    worker.stop_replay(None, false);
    worker.stop_audio_test(None);
    // Accepted saves get a bounded time to finish (the UI normally keeps
    // draining events meanwhile). Afterwards completion events must not block
    // on a full UI queue and the exporter fails whatever is still queued.
    if let Some(exporter) = &worker.exporter {
        let pending = exporter.in_flight();
        if pending > 0 {
            log(format_args!(
                "shutdown: waiting for {pending} clip export(s)"
            ));
            if exporter.finish_queued(SHUTDOWN_EXPORT_WAIT) {
                log(format_args!("shutdown: clip exports drained"));
            } else {
                log(format_args!(
                    "shutdown: {} clip export(s) unfinished after {SHUTDOWN_EXPORT_WAIT:?}; queued ones fail",
                    exporter.in_flight()
                ));
            }
        }
    }
    worker.shutting_down.store(true, Ordering::Release);
    // Exporter first: its queued jobs own completion senders.
    drop(worker.exporter.take());
    drop(worker.completions.take());
}

/// Delivers export results to the UI on its own thread. The export thread only
/// queues them here, so a slow or stalled UI never holds up the export thread
/// and the saves queued behind it. At most one pending event per accepted job.
struct Completions {
    sender: Option<Sender<RecorderEvent>>,
    thread: Option<JoinHandle<()>>,
}

impl Completions {
    fn spawn(
        events: async_channel::Sender<RecorderEvent>,
        shutting_down: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<RecorderEvent>();
        let thread = std::thread::Builder::new()
            .name("clipper-replay-events".into())
            .spawn(move || {
                while let Ok(event) = receiver.recv() {
                    send_without_hanging(&events, event, &shutting_down);
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
    }

    fn sender(&self) -> Option<Sender<RecorderEvent>> {
        self.sender.clone()
    }
}

impl Drop for Completions {
    fn drop(&mut self) {
        // The thread ends once every sender clone is gone (the exporter that
        // holds the jobs is dropped first) and gives up on a full queue once
        // `shutting_down` is set.
        drop(self.sender.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Worker<'_> {
    fn send(&self, event: RecorderEvent) {
        emit(self.endpoint, event);
    }

    fn error(&self, message: impl Into<String>) {
        self.send(RecorderEvent::Error {
            message: message.into(),
        });
    }

    /// Returns `false` when the worker must exit.
    fn handle(&mut self, command: RecorderCommand) -> bool {
        match command {
            RecorderCommand::StartAudioTest {
                options,
                output_directory,
            } => {
                if self.replay.is_some() {
                    self.send(RecorderEvent::AudioTestFinished {
                        path: None,
                        error: Some(STOP_REPLAY_FIRST.into()),
                    });
                } else if self.recording.is_some() || self.audio_test.is_some() {
                    self.error("Stop the current recording or audio test first.");
                } else {
                    match AudioTest::start(&options, &output_directory) {
                        Ok(test) => {
                            self.audio_test = Some(test);
                            self.send(RecorderEvent::AudioTestStarted);
                        }
                        Err(message) => self.send(RecorderEvent::AudioTestFinished {
                            path: None,
                            error: Some(message),
                        }),
                    }
                }
            }
            RecorderCommand::StopAudioTest => self.stop_audio_test(None),
            RecorderCommand::Shutdown => {
                self.stop_audio_test(None);
                self.stop_recording(StopReason::Shutdown, None);
                self.stop_replay(None, false);
                return false;
            }
            RecorderCommand::RefreshSources => match list_sources() {
                Ok(sources) => self.send(RecorderEvent::SourcesListed(sources)),
                Err(error) => self.error(format!("could not list capture sources: {error}")),
            },
            RecorderCommand::StartRecording { source, options } => {
                if self.replay.is_some() {
                    self.send(RecorderEvent::RecordingFailed {
                        message: STOP_REPLAY_FIRST.into(),
                    });
                } else if self.audio_test.is_some() {
                    self.error("Stop the audio test first.");
                } else {
                    self.start_recording(&source, &options);
                }
            }
            RecorderCommand::StopRecording => match self.lifecycle.begin_stop() {
                Ok(()) => self.stop_recording_started(StopReason::User, None),
                Err(message) => self.error(message),
            },
            RecorderCommand::StartReplay { source, options } => {
                self.start_replay(&source, &options);
            }
            RecorderCommand::StopReplay => self.stop_replay(None, true),
            RecorderCommand::SetReplayDuration(duration) => {
                // Ignored while idle and for out-of-range values.
                if let Some(replay) = &mut self.replay {
                    replay.set_duration(duration);
                }
            }
            // Gain and noise gate settings are live. Enabling, disabling, adding or removing
            // sources applies to the next session: the track topology of a
            // file or replay buffer cannot change while it records.
            RecorderCommand::SetAudioGain {
                source,
                gain_percent,
            } => {
                if let Some(audio) = self.recording.as_mut().and_then(|r| r.audio.as_mut()) {
                    audio.set_gain(source, gain_percent);
                }
                if let Some(replay) = &mut self.replay {
                    replay.set_audio_gain(source, gain_percent);
                }
                if let Some(test) = &mut self.audio_test {
                    test.set_gain(source, gain_percent);
                }
            }
            RecorderCommand::SetAudioNoiseGate { source, settings } => {
                if let Some(audio) = self.recording.as_mut().and_then(|r| r.audio.as_mut()) {
                    audio.set_noise_gate(source, settings);
                }
                if let Some(replay) = &mut self.replay {
                    replay.set_audio_noise_gate(source, settings);
                }
                if let Some(test) = &mut self.audio_test {
                    test.set_noise_gate(source, settings);
                }
            }
            RecorderCommand::SaveReplay => self.save_replay(),
        }
        true
    }

    /// Best-effort notification: never blocks the worker on a full UI queue.
    fn notify(&self, event: RecorderEvent) {
        let _ = self.endpoint.events.try_send(event);
    }

    fn start_replay(&mut self, source: &CaptureSource, options: &ReplayOptions) {
        if let Some(replay) = &self.replay {
            // Idempotent: the UI is waiting for the running state.
            self.send(RecorderEvent::ReplayStarted {
                encoder: replay.encoder_name().to_owned(),
            });
            return;
        }
        let stopped = |error: String| RecorderEvent::ReplayStopped { error: Some(error) };
        if self.recording.is_some() || self.audio_test.is_some() {
            self.send(stopped("stop the current recording first".into()));
            return;
        }
        if let Err(message) = validate_replay_options(options) {
            self.send(stopped(message));
            return;
        }
        match self.try_start_replay(source, options) {
            Ok(replay) => {
                let encoder = replay.encoder_name().to_owned();
                self.replay_output = options.recording.output_directory.clone();
                self.sweep_stale_partials();
                self.replay = Some(replay);
                self.send(RecorderEvent::ReplayStarted { encoder });
            }
            Err(message) => self.send(stopped(message)),
        }
    }

    fn try_start_replay(
        &mut self,
        source: &CaptureSource,
        options: &ReplayOptions,
    ) -> Result<ReplaySession, String> {
        if let Some(message) = &self.init_error {
            return Err(replay_failure_text(message));
        }
        if self.media_foundation.is_none() {
            let mf = MediaFoundation::start()
                .map_err(|error| replay_failure_text(&encoder_message(&error)))?;
            self.media_foundation = Some(mf);
        }
        let Some(mf) = self.media_foundation.as_ref() else {
            return Err("Media Foundation is not available".into());
        };
        ReplaySession::start(mf, source, options)
    }

    /// Removes stale `Clipper_Replay_*.mp4.partial` files of crashed runs in
    /// the clip folder, once per replay start, on the export thread.
    fn sweep_stale_partials(&mut self) {
        if self.ensure_exporter().is_err() {
            return;
        }
        let Some(exporter) = &self.exporter else {
            return;
        };
        let directory = self.replay_output.clone();
        let _ = exporter.try_run(Box::new(move || {
            sweep_stale_partials(&directory, std::time::SystemTime::now(), STALE_PARTIAL_AGE);
        }));
    }

    /// Stops the replay buffer (also when idle, so the UI stays consistent)
    /// and reports it. Running exports are not touched.
    fn stop_replay(&mut self, error: Option<String>, report: bool) {
        let session = self.replay.take();
        let had_session = session.is_some();
        if let Some(session) = session {
            let exports = self.exporter.as_ref().map_or(0, Exporter::in_flight);
            log(format_args!(
                "replay stop: {}; {exports} clip export(s) in flight continue",
                error.as_deref().unwrap_or("requested")
            ));
            let started = Instant::now();
            let diagnostics = session.diagnostics();
            let delivered = session.capture_delivered();
            session.finish_audio_report(self.endpoint);
            // Releases capture, encoder, audio, buffer and device, in that order.
            drop(session);
            log(format_args!(
                "replay stop: capture and encoder released in {:?}; {diagnostics:?}, capture delivered {delivered} frames",
                started.elapsed()
            ));
            // Aggregated A/V timing health (docs/quality/av-sync-audit.md).
            log(format_args!(
                "replay stop: a/v sync: {} audio re-anchors (largest {} frames at 48 kHz), {} repeated video frames, {} audio frames lost",
                diagnostics.audio_resyncs,
                diagnostics.audio_max_resync_frames,
                diagnostics.repeated_frames,
                diagnostics.audio_glitch_frames
            ));
        }
        if report || (had_session && error.is_some()) {
            self.send(RecorderEvent::ReplayStopped { error });
        }
    }

    fn service_replay(&mut self) {
        let Some(replay) = self.replay.as_mut() else {
            return;
        };
        match replay.wake.recv_timeout(WAKE_WAIT) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_millis(10)),
        }
        if let Err(message) = replay.pump() {
            self.fail_replay(message);
            return;
        }
        replay.report_audio(self.endpoint);
        if replay.source_closed() {
            self.fail_replay("the capture source was closed".into());
            return;
        }
        if replay.status_due() {
            let in_flight = self.exporter.as_ref().map_or(0, Exporter::in_flight);
            // Never block on status: drop it if the UI queue is full.
            let _ = self
                .endpoint
                .events
                .try_send(RecorderEvent::ReplayStatus(replay.status(in_flight)));
        }
    }

    /// Fatal runtime error: first try to keep the footage by saving the
    /// configured duration as an automatic save, then tear the session down.
    fn fail_replay(&mut self, message: String) {
        self.queue_save(true);
        self.stop_replay(Some(message), true);
    }

    fn save_failed(&self, message: impl Into<String>) {
        self.notify(RecorderEvent::ReplaySaveFailed {
            message: message.into(),
        });
    }

    /// Snapshots the buffer (Arc clones, no file I/O) and queues the export.
    fn save_replay(&mut self) {
        self.queue_save(false);
    }

    /// `automatic`: save triggered by a fatal replay error; failures are
    /// silent when there is nothing to save yet.
    fn queue_save(&mut self, automatic: bool) {
        let id = self.next_save;
        self.next_save += 1;
        let reject = |worker: &Self, message: String| {
            log(format_args!("save #{id}: rejected: {message}"));
            worker.save_failed(message);
        };
        let Some(replay) = self.replay.as_mut() else {
            reject(self, REPLAY_NOT_RUNNING.to_owned());
            return;
        };
        let snapshot = match replay.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) if automatic => {
                log(format_args!("save #{id}: nothing to keep: {error}"));
                return;
            }
            Err(ReplayError::NotReady) => {
                let message = "The replay buffer is still filling; try again in a moment.";
                reject(self, message.to_owned());
                return;
            }
            Err(error) => {
                reject(self, error.to_string());
                return;
            }
        };
        if let Err(error) = self.ensure_exporter() {
            reject(self, format!("Could not start the clip exporter: {error}"));
            return;
        }
        let (Some(exporter), Some(completed)) = (
            self.exporter.as_ref(),
            self.completions.as_ref().and_then(Completions::sender),
        ) else {
            return;
        };
        let directory = self.replay_output.clone();
        // In-memory reservation only: folder creation and the on-disk name
        // check run on the export thread (`prepare`).
        let name = self.clip_names.reserve(&replay_clip_base(), |_| false);
        let interval = format!(
            "last {:?} up to {:.3} s, {} GOPs",
            snapshot.requested,
            snapshot.end_pts as f64 / 1e7,
            snapshot.gops.len()
        );
        let job = ExportJob {
            snapshot,
            final_path: directory.join(&name),
        };
        let prepare: ExportPrepare = Box::new(move |job| {
            log(format_args!("save #{id}: export started"));
            fs::create_dir_all(&directory)
                .map_err(|error| ReplayError::io("creating the clip folder", &directory, error))?;
            let base = job
                .final_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let unique = unique_file_name(&base, |candidate| {
                let path = directory.join(candidate);
                path.exists() || partial_path(&path).exists()
            });
            job.final_path = directory.join(unique);
            Ok(())
        });
        let names = self.clip_names.clone();
        let reserved = name.clone();
        let done = Box::new(move |result: Result<SavedClip, ReplayError>| {
            // Runs on the export thread, only after export_clip returned:
            // the file is finalized and renamed on Ok.
            let event = match result {
                Ok(saved) => {
                    log(format_args!(
                        "save #{id}: saved {} ({:?}, {} bytes)",
                        saved.path.display(),
                        saved.duration,
                        saved.bytes
                    ));
                    RecorderEvent::ReplaySaved {
                        path: saved.path,
                        duration: saved.duration,
                    }
                }
                Err(error) => {
                    log(format_args!("save #{id}: failed: {error}"));
                    RecorderEvent::ReplaySaveFailed {
                        message: error.to_string(),
                    }
                }
            };
            names.release(&reserved);
            // Never blocks: the forwarder thread waits for room in the UI queue.
            let _ = completed.send(event);
        });
        // Before submitting: a quick export could otherwise report
        // ReplaySaved ahead of ReplaySaving and leave "Saving…" on screen.
        self.notify(RecorderEvent::ReplaySaving);
        match exporter.try_submit_with(job, Some(prepare), done) {
            Ok(()) => log(format_args!("save #{id}: queued ({interval}) as {name}")),
            Err(error) => {
                self.clip_names.release(&name);
                let message = match error {
                    ReplayError::Busy => "Already saving clips; try again in a moment.".to_owned(),
                    other => other.to_string(),
                };
                reject(self, message);
            }
        }
    }

    /// Spawns the export thread and its completion forwarder, once.
    fn ensure_exporter(&mut self) -> std::io::Result<()> {
        if self.completions.is_none() {
            self.completions = Some(Completions::spawn(
                self.endpoint.events.clone(),
                Arc::clone(&self.shutting_down),
            )?);
        }
        if self.exporter.is_none() {
            self.exporter = Some(Exporter::spawn(EXPORT_QUEUE)?);
        }
        Ok(())
    }

    fn stop_audio_test(&mut self, failure: Option<String>) {
        let Some(test) = self.audio_test.take() else {
            return;
        };
        let result = test.finish().and_then(|path| {
            if let Some(message) = failure {
                let _ = fs::remove_file(path);
                Err(message)
            } else {
                Ok(path)
            }
        });
        let (path, error) = match result {
            Ok(path) => (Some(path), None),
            Err(error) => (None, Some(error)),
        };
        self.send(RecorderEvent::AudioTestFinished { path, error });
        AudioReporter::finish(self.endpoint);
    }

    fn service_audio_test(&mut self) {
        let Some(test) = &mut self.audio_test else {
            return;
        };
        let _ = test.wake.recv_timeout(MAX_WAIT);
        if let Err(message) = test.pump(false) {
            self.stop_audio_test(Some(message));
            return;
        }
        test.report(self.endpoint);
        if test.elapsed() >= Duration::from_secs(30) {
            self.stop_audio_test(None);
        }
    }

    fn start_recording(&mut self, source: &CaptureSource, options: &RecordingOptions) {
        if let Err(message) = self.lifecycle.begin_start() {
            self.error(message);
            return;
        }
        match self.try_start(source, options) {
            Ok(recording) => {
                self.lifecycle.started();
                self.send(RecorderEvent::RecordingStarted {
                    path: recording.final_path.clone(),
                    encoder: recording.encoder.name().to_owned(),
                });
                self.recording = Some(recording);
            }
            Err(message) => {
                self.lifecycle.start_failed(&message);
                self.send(RecorderEvent::RecordingFailed { message });
            }
        }
    }

    fn try_start(
        &mut self,
        source: &CaptureSource,
        options: &RecordingOptions,
    ) -> Result<Recording, String> {
        if let Some(message) = &self.init_error {
            return Err(message.clone());
        }
        if self.media_foundation.is_none() {
            let mf = MediaFoundation::start().map_err(|error| encoder_message(&error))?;
            self.media_foundation = Some(mf);
        }
        let Some(mf) = self.media_foundation.as_ref() else {
            return Err("Media Foundation is not available".into());
        };

        fs::create_dir_all(&options.output_directory).map_err(|error| {
            format!(
                "could not create the output folder {}: {error}",
                options.output_directory.display()
            )
        })?;
        let (final_path, partial) = choose_paths(&options.output_directory);
        precheck_writable(&partial)?;

        let result = build_recording(mf, source, options, final_path, partial.clone());
        if result.is_err() {
            let _ = fs::remove_file(&partial);
        }
        result
    }

    /// Waits for a capture wake (bounded), moves frames and reports stats.
    fn service_recording(&mut self) {
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        match recording.wake.recv_timeout(MAX_WAIT) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_millis(10)),
        }
        if let Err(message) = recording.pump() {
            self.stop_recording(StopReason::Error, Some(message));
            return;
        }
        recording
            .audio_reporter
            .report(self.endpoint, recording.audio.as_mut());
        if recording.source_closed.load(Ordering::Acquire) {
            self.stop_recording(StopReason::SourceClosed, None);
            return;
        }
        if recording
            .meter
            .update(recording.started.elapsed(), STATS_WINDOW)
        {
            let stats = recording.stats();
            // Never block on stats: drop them if the UI queue is full.
            let _ = self.endpoint.events.try_send(RecorderEvent::Stats(stats));
        }
    }

    /// Stops an active recording (no-op when idle), validating via lifecycle.
    fn stop_recording(&mut self, reason: StopReason, failure: Option<String>) {
        if self.recording.is_none() {
            return;
        }
        let _ = self.lifecycle.begin_stop();
        self.stop_recording_started(reason, failure);
    }

    /// Finalizes the active recording after `begin_stop` succeeded.
    fn stop_recording_started(&mut self, reason: StopReason, failure: Option<String>) {
        let Some(recording) = self.recording.take() else {
            self.lifecycle.stopped();
            return;
        };
        let Recording {
            session,
            mut encoder,
            mut audio,
            audio_failed,
            video_end: _,
            audio_reporter: _,
            _d3d,
            meter,
            counters,
            started,
            final_path,
            partial,
            audio_labels,
            ..
        } = recording;
        // Stop frames first, then drain and finalize the encoder.
        drop(session);
        let stats = counters.snapshot(started.elapsed(), meter.rate());
        let audio_finish = audio.as_mut().map_or(Ok(()), |audio| {
            let end = clipper_audio::windows::qpc_now_ticks().saturating_sub(audio.origin());
            encoder
                .video_gap(end)
                .map_err(|error| encoder_message(&error))?;
            audio.pump(&mut encoder, true, end)
        });
        drop(audio);
        AudioReporter::finish(self.endpoint);
        let finish = encoder
            .finish()
            .map_err(|error| encoder_message(&error))
            .and(audio_finish);
        drop(_d3d);

        let finish = if audio_failed {
            Err(failure.clone().unwrap_or_default())
        } else {
            finish
        };
        let outcome = match classify_stop(counters.encoded, finish) {
            StopOutcome::Success => {
                // Best effort: an unnamed file is still a complete recording.
                if let Err(error) =
                    clipper_replay::track_names::name_audio_tracks(&partial, &audio_labels)
                {
                    log(format_args!(
                        "recording: audio track names not written: {error}"
                    ));
                }
                match publish_recording(&partial, &final_path) {
                    Published::Saved(path) => {
                        if path != final_path {
                            log(format_args!(
                                "recording: {} was taken meanwhile; saved as {}",
                                final_path.display(),
                                path.display()
                            ));
                        }
                        Ok(path)
                    }
                    Published::Kept { reason } => {
                        // Never deleted: it may be the only copy of the recording.
                        log(format_args!("recording: not published: {reason}"));
                        self.lifecycle.stopped();
                        self.send(RecorderEvent::RecordingFailed {
                            message: format!(
                                "the recording could not be saved as {}: {reason}; the file was kept as {}",
                                final_path.display(),
                                partial.display()
                            ),
                        });
                        return;
                    }
                }
            }
            StopOutcome::Failure(message) => Err(message),
        };
        self.lifecycle.stopped();
        match outcome {
            Ok(path) => {
                if let Some(message) = failure {
                    self.error(message);
                }
                self.send(RecorderEvent::RecordingFinished {
                    path,
                    stats,
                    reason,
                });
            }
            Err(message) => {
                let _ = fs::remove_file(&partial);
                let message = match failure {
                    Some(cause) if cause != message => format!("{cause}; {message}"),
                    _ => message,
                };
                self.send(RecorderEvent::RecordingFailed { message });
            }
        }
    }
}

fn build_recording(
    mf: &MediaFoundation,
    source: &CaptureSource,
    options: &RecordingOptions,
    final_path: PathBuf,
    partial: PathBuf,
) -> Result<Recording, String> {
    let d3d =
        D3DContext::new().map_err(|error| format!("could not create the GPU device: {error}"))?;

    let (wake_tx, wake) = sync_channel::<()>(1);
    let source_closed = Arc::new(AtomicBool::new(false));
    let session = CaptureSession::start(
        &d3d,
        source,
        signal_handler(wake_tx.clone(), Arc::clone(&source_closed)),
    )
    .map_err(|error| error.to_string())?;
    // Optional (Windows 11 24H2+); older builds keep the WGC default.
    let _ = session.set_target_fps(options.fps);

    let (width, height) = session.size();
    let (output_width, output_height) =
        fit_output_size((width, height), (options.width, options.height));
    let config = EncoderConfig {
        input_width: round_up_even(width),
        input_height: round_up_even(height),
        output_width,
        output_height,
        fps: options.fps,
        bitrate_mbps: options.bitrate_mbps,
        profile: options.profile,
    };
    // The plan decides the file's audio tracks (mix + one per planned source
    // when separate), so it must exist before the encoder.
    let plan = AudioPlan::for_session(&options.audio, Some(source));
    let audio_reporter = AudioReporter::new(plan.idle_states());
    let audio_labels = plan.labels();
    let encoder =
        HardwareEncoder::create_with_audio(mf, &d3d, &config, &partial, plan.encoder_tracks())
            .map_err(|error| encoder_message(&error))?;

    // Initialize expensive GPU/codec resources before audio starts, otherwise
    // hardware encoder discovery can overflow the bounded audio rings.
    let audio = RecordingAudio::start(plan, wake_tx)?;
    let origin = audio.as_ref().map(RecordingAudio::origin);

    Ok(Recording {
        session: Some(session),
        encoder,
        audio,
        audio_failed: false,
        video_end: 0,
        audio_reporter,
        _d3d: d3d,
        pacer: origin.map_or_else(
            || FramePacer::new(options.fps),
            |origin| FramePacer::with_origin(options.fps, origin),
        ),
        meter: RateMeter::new(),
        counters: Counters::default(),
        started: Instant::now(),
        final_path,
        partial,
        audio_labels,
        wake,
        source_closed,
    })
}

/// Runs on a WGC thread: must never block or allocate.
pub(crate) fn signal_handler(
    wake: SyncSender<()>,
    source_closed: Arc<AtomicBool>,
) -> Box<dyn Fn(CaptureSignal) + Send + Sync + 'static> {
    Box::new(move |signal| {
        if signal == CaptureSignal::SourceClosed {
            source_closed.store(true, Ordering::Release);
        }
        // Full means a wake is already pending, which is enough.
        let _ = wake.try_send(());
    })
}

pub(crate) fn round_up_even(value: u32) -> u32 {
    value.max(2).saturating_add(1) & !1
}

/// Delivers a completion event from the export thread. Waits for room in the
/// UI queue in short steps and gives up as soon as the worker is shutting
/// down, so joining the exporter can never hang on a queue nobody drains.
fn send_without_hanging(
    events: &async_channel::Sender<RecorderEvent>,
    mut event: RecorderEvent,
    shutting_down: &AtomicBool,
) {
    loop {
        match events.try_send(event) {
            Ok(()) | Err(async_channel::TrySendError::Closed(_)) => return,
            Err(async_channel::TrySendError::Full(back)) => {
                if shutting_down.load(Ordering::Acquire) {
                    return;
                }
                event = back;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

const STOP_REPLAY_FIRST: &str = "Stop instant replay first.";

#[allow(unsafe_code)]
fn replay_clip_base() -> String {
    // SAFETY: GetLocalTime takes no arguments and only returns a value.
    let now = unsafe { GetLocalTime() };
    replay_file_name(
        now.wYear,
        now.wMonth,
        now.wDay,
        now.wHour,
        now.wMinute,
        now.wSecond,
    )
}

#[allow(unsafe_code)]
fn choose_paths(directory: &Path) -> (PathBuf, PathBuf) {
    // SAFETY: GetLocalTime takes no arguments and only returns a value.
    let now = unsafe { GetLocalTime() };
    let base = recording_file_name(
        now.wYear,
        now.wMonth,
        now.wDay,
        now.wHour,
        now.wMinute,
        now.wSecond,
    );
    let name = unique_file_name(&base, |candidate| {
        let path = directory.join(candidate);
        path.exists() || partial_path(&path).exists()
    });
    let final_path = directory.join(name);
    let partial = partial_path(&final_path);
    (final_path, partial)
}

/// Fails early with a clear message if the output location is not writable.
fn precheck_writable(partial: &Path) -> Result<(), String> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial)
    {
        Ok(file) => {
            drop(file);
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::PermissionDenied => Err(format!(
            "no permission to write to {}",
            partial.parent().unwrap_or(partial).display()
        )),
        Err(error) => Err(format!("cannot create the output file: {error}")),
    }
}

pub(crate) fn encoder_message(error: &EncoderError) -> String {
    match error {
        EncoderError::DiskFull => "not enough disk space to keep recording".into(),
        EncoderError::AccessDenied => "no permission to write the output file".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_up_to_even() {
        assert_eq!(round_up_even(1920), 1920);
        assert_eq!(round_up_even(1081), 1082);
        assert_eq!(round_up_even(0), 2);
        assert_eq!(round_up_even(u32::MAX), u32::MAX - 1);
    }

    fn failed(index: usize) -> RecorderEvent {
        RecorderEvent::ReplaySaveFailed {
            message: index.to_string(),
        }
    }

    #[test]
    fn completions_never_block_the_export_thread_and_keep_order() {
        // A UI queue of one that nobody reads yet: the export side must still
        // hand over every result immediately.
        let (events, ui) = async_channel::bounded(1);
        let completions = Completions::spawn(events, Arc::new(AtomicBool::new(false))).unwrap();
        let sender = completions.sender().unwrap();
        for index in 0..5 {
            sender.send(failed(index)).unwrap();
        }
        let delivered: Vec<_> = (0..5).map(|_| ui.recv_blocking().unwrap()).collect();
        assert_eq!(delivered, (0..5).map(failed).collect::<Vec<_>>());
        drop(sender);
        drop(completions);
    }

    #[test]
    fn completions_give_up_on_a_stalled_queue_at_shutdown() {
        let (events, ui) = async_channel::bounded(1);
        let shutting_down = Arc::new(AtomicBool::new(false));
        let completions = Completions::spawn(events, Arc::clone(&shutting_down)).unwrap();
        let sender = completions.sender().unwrap();
        for index in 0..3 {
            sender.send(failed(index)).unwrap();
        }
        drop(sender);
        shutting_down.store(true, Ordering::Release);
        let (joined, done) = mpsc::channel();
        std::thread::spawn(move || {
            drop(completions);
            joined.send(()).unwrap();
        });
        done.recv_timeout(Duration::from_secs(10))
            .expect("joining the forwarder hung on a full UI queue");
        drop(ui);
    }
}
