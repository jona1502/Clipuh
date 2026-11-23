//! WASAPI to aligned AAC on the recorder worker. No file I/O on capture threads.
//!
//! The encoder's track topology is fixed per session: the [`AudioPlan`]
//! (sources resolved against the capture source and the Windows version)
//! decides the encoder's track count before the encoder exists. Capture is
//! not: enabling, disabling, adding, removing or retargeting sources is
//! applied to a running session by [`RecordingAudio::reconfigure`] (see
//! [`clipper_audio::live`]); a source added that way feeds the mix, and gets
//! an isolated track only if it owned one at session start. Gain and noise
//! gate changes are live ([`RecordingAudio::set_gain`],
//! [`RecordingAudio::set_noise_gate`]).
//! Each source passes through its linked stereo gate before ramped gain and
//! limiting, then feeds both mix and isolated tracks. Thresholds therefore
//! remain independent of gain, Mute and Solo. Gating changes no timestamps.
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use clipper_audio::live::{LiveRouting, LiveTrack};
use clipper_audio::mixer::TimelineMixer;
use clipper_audio::routing::{self, PlannedTrack, RoutingPlan};
use clipper_audio::windows::{
    AudioCapture, Finished, capabilities, executable_of_window, list_devices, qpc_now_ticks,
};
use clipper_audio::{
    AudioCapabilities, AudioConfig, AudioEvent, AudioSelection, DeviceFlow, MicrophoneChoice,
    ProcessTarget, SourceRequest, SourceRequestKind, TrackContent,
};
use clipper_core::recording::{
    AudioSourceState, AudioSourceStatus, AudioSourceTarget, CaptureSource, MAX_AUDIO_GAIN_PERCENT,
    MAX_RECORDED_AUDIO_SOURCES, NoiseGateSettings, RecorderEndpoint, RecorderEvent,
    RecordingAudioOptions, RecordingAudioSource, SourceKind, SourceLevel,
};
use clipper_encoder::backends::media_foundation::HardwareEncoder;
use clipper_encoder::backends::packet_encoder::PacketEncoder;

/// Destination of mixed AAC-ready audio: the MP4 sink writer of a manual
/// recording or the packet encoder feeding the replay buffer.
pub trait AudioSink {
    fn submit_audio(&mut self, track: usize, samples: &[f32], time: i64) -> Result<(), String>;
}

impl AudioSink for HardwareEncoder {
    fn submit_audio(&mut self, track: usize, samples: &[f32], time: i64) -> Result<(), String> {
        HardwareEncoder::submit_audio(self, track, samples, time).map_err(|e| e.to_string())
    }
}

impl AudioSink for PacketEncoder {
    fn submit_audio(&mut self, track: usize, samples: &[f32], time: i64) -> Result<(), String> {
        PacketEncoder::submit_audio(self, track, samples, time).map_err(|e| e.to_string())
    }
}

/// Meter events go out at most this often (10 Hz).
/// Meter refresh: ~30 Hz is smooth once the UI interpolates, and cheap.
const METER_INTERVAL: Duration = Duration::from_millis(33);

/// How often a composed desktop track's unseparable-application list is
/// re-read for the UI status and the log.
const NOTE_INTERVAL: Duration = Duration::from_secs(1);

/// Mixer sources resolved against the session's capture source and the
/// Windows version.
pub struct AudioPlan {
    sources: Vec<RecordingAudioSource>,
    routing: RoutingPlan,
    skipped: Vec<(u32, String)>,
    separate: bool,
    /// Captured window (`None`: display or audio test), kept so live source
    /// changes resolve `Game` the same way the session start did.
    window: Option<u64>,
}

/// Resolves mixer sources to capture requests. `window` is the HWND of the
/// captured window (`None` for a display or no capture at all) and `exe_of`
/// maps it to the owning executable. Sources that cannot be captured come
/// back as `(id, reason)`. At most [`MAX_RECORDED_AUDIO_SOURCES`] are kept.
pub fn resolve_selection(
    options: &RecordingAudioOptions,
    window: Option<u64>,
    exe_of: impl Fn(u64) -> Option<String>,
) -> (AudioSelection, Vec<(u32, String)>) {
    let mut selection = AudioSelection::default();
    let mut skipped = Vec::new();
    for source in &options.sources {
        let kind = match &source.target {
            AudioSourceTarget::Game => match window.map(&exe_of) {
                None => {
                    skipped.push((
                        source.id,
                        "Game audio follows a captured window; a whole display is being captured."
                            .to_owned(),
                    ));
                    continue;
                }
                Some(None) => {
                    skipped.push((
                        source.id,
                        "The captured window's application could not be identified.".to_owned(),
                    ));
                    continue;
                }
                Some(Some(exe)) => SourceRequestKind::Process(ProcessTarget::Executable(exe)),
            },
            AudioSourceTarget::Application(exe) => {
                SourceRequestKind::Process(ProcessTarget::Executable(exe.clone()))
            }
            AudioSourceTarget::Desktop(device) => SourceRequestKind::Desktop {
                device: device.clone(),
            },
            AudioSourceTarget::Microphone(device) => SourceRequestKind::Microphone(
                device.as_ref().map_or(MicrophoneChoice::Default, |id| {
                    MicrophoneChoice::Device(id.clone())
                }),
            ),
        };
        if selection.sources.len() >= MAX_RECORDED_AUDIO_SOURCES {
            skipped.push((
                source.id,
                format!("At most {MAX_RECORDED_AUDIO_SOURCES} sources can be recorded."),
            ));
            continue;
        }
        selection.sources.push(SourceRequest {
            id: source.id,
            kind,
        });
    }
    (selection, skipped)
}

/// Endpoint ids of the current Windows default devices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DefaultDevices {
    pub render: Option<String>,
    pub capture: Option<String>,
}

impl DefaultDevices {
    fn query() -> Self {
        let default_of = |flow| {
            list_devices(flow)
                .ok()
                .and_then(|devices| devices.into_iter().find(|d| d.is_default))
                .map(|d| d.id)
        };
        Self {
            render: default_of(DeviceFlow::Render),
            capture: default_of(DeviceFlow::Capture),
        }
    }
}

/// A device chosen explicitly that is also the Windows default is the same
/// signal as a source following the default. When both are selected the
/// explicit one becomes "default" too, so routing reports it as a duplicate
/// instead of recording (and mixing) the device twice.
fn alias_default_devices(selection: &mut AudioSelection, defaults: &DefaultDevices) {
    let follows_default_render = selection
        .sources
        .iter()
        .any(|s| matches!(&s.kind, SourceRequestKind::Desktop { device: None }));
    let follows_default_capture = selection.sources.iter().any(|s| {
        matches!(
            &s.kind,
            SourceRequestKind::Microphone(MicrophoneChoice::Default)
        )
    });
    for source in &mut selection.sources {
        match &mut source.kind {
            SourceRequestKind::Desktop { device } if follows_default_render => {
                if device.is_some() && *device == defaults.render {
                    *device = None;
                }
            }
            SourceRequestKind::Microphone(choice) if follows_default_capture => {
                let is_default = matches!(
                    &*choice,
                    MicrophoneChoice::Device(id) if Some(id) == defaults.capture.as_ref()
                );
                if is_default {
                    *choice = MicrophoneChoice::Default;
                }
            }
            _ => {}
        }
    }
}

/// Audio tracks a session with `options` is expected to encode on this
/// Windows version (mix plus isolated tracks), for storage estimates. A
/// `Game` source is assumed to resolve (a display capture, known only at
/// start, would skip it). Never exceeds
/// [`RecordingAudioOptions::encoded_track_count`].
pub fn planned_audio_tracks(options: &RecordingAudioOptions) -> u32 {
    planned_audio_tracks_with(options, capabilities(), &DefaultDevices::default())
}

fn planned_audio_tracks_with(
    options: &RecordingAudioOptions,
    caps: AudioCapabilities,
    defaults: &DefaultDevices,
) -> u32 {
    AudioPlan::build(
        options,
        Some(1),
        // Stand-in executable that cannot collide with a configured one.
        |_| Some("<captured window>".to_owned()),
        caps,
        defaults,
    )
    .encoder_tracks() as u32
}

/// The captured window of a session (`None` for a display or no capture),
/// which `Game` audio follows.
pub fn session_window(source: Option<&CaptureSource>) -> Option<u64> {
    source
        .filter(|source| source.kind == SourceKind::Window)
        .map(|source| source.id)
}

impl AudioPlan {
    /// Plans the audio of a session capturing `source` (`None`: audio test).
    pub fn for_session(options: &RecordingAudioOptions, source: Option<&CaptureSource>) -> Self {
        Self::for_window(options, session_window(source))
    }

    /// Plans the audio of a session capturing `window` (`None`: a display
    /// or no capture).
    pub fn for_window(options: &RecordingAudioOptions, window: Option<u64>) -> Self {
        Self::build(
            options,
            window,
            executable_of_window,
            capabilities(),
            &DefaultDevices::query(),
        )
    }

    fn build(
        options: &RecordingAudioOptions,
        window: Option<u64>,
        exe_of: impl Fn(u64) -> Option<String>,
        caps: AudioCapabilities,
        defaults: &DefaultDevices,
    ) -> Self {
        let (mut selection, mut skipped) = resolve_selection(options, window, exe_of);
        alias_default_devices(&mut selection, defaults);
        let mut routing = routing::plan(&selection, caps);
        for (id, warning) in &routing.warnings {
            skipped.push((*id, warning.message().to_owned()));
        }
        // The MP4 writer holds one mix plus MAX_RECORDED_AUDIO_SOURCES tracks.
        routing.tracks.truncate(MAX_RECORDED_AUDIO_SOURCES);
        Self {
            sources: options.sources.clone(),
            routing,
            skipped,
            separate: options.separate_tracks,
            window,
        }
    }

    /// The plan for changed `options` within the running session: same
    /// captured window and, because the encoder topology cannot change,
    /// the session's separate-tracks setting.
    fn replan(&self, options: &RecordingAudioOptions) -> Self {
        let mut next = Self::build(
            options,
            self.window,
            executable_of_window,
            capabilities(),
            &DefaultDevices::query(),
        );
        next.separate = self.separate;
        next
    }

    /// Audio tracks of the encoder: the mix plus, when separate tracks are on,
    /// one isolated track per planned track (never silently downgraded).
    pub fn encoder_tracks(&self) -> usize {
        match self.routing.tracks.len() {
            0 => 0,
            n => 1 + if self.separate { n } else { 0 },
        }
    }

    /// Track labels of the encoder: "Mix", then each isolated track.
    pub fn labels(&self) -> Vec<String> {
        if self.routing.tracks.is_empty() {
            return Vec::new();
        }
        let mut labels = vec!["Mix".to_owned()];
        if self.separate {
            labels.extend(self.routing.tracks.iter().map(|t| t.content.label()));
        }
        labels
    }

    fn gain_percent(&self, source: u32) -> u16 {
        self.sources
            .iter()
            .find(|s| s.id == source)
            .map_or(100, |s| s.gain_percent.min(MAX_AUDIO_GAIN_PERCENT))
    }

    /// Status of every requested source given the per-track states, the
    /// session's isolated outputs and sources refused by a live change.
    fn states(
        &self,
        tracks: &[TrackRt],
        live: &LiveRouting,
        rejected: &[(u32, String)],
    ) -> Vec<AudioSourceState> {
        self.sources
            .iter()
            .map(|source| {
                let status = match tracks.iter().find(|track| track.source == source.id) {
                    Some(track) => match &track.live {
                        Live::Starting => AudioSourceStatus::Starting,
                        Live::Capturing => {
                            let output = live.output_of(source.id).map(|o| o + 1);
                            let note = if self.separate && output.is_none() {
                                Some(
                                    "Recorded in the mix; its own track starts with the next session."
                                        .to_owned(),
                                )
                            } else {
                                track.note.clone()
                            };
                            AudioSourceStatus::Capturing {
                                track: output,
                                note,
                            }
                        }
                        Live::Waiting(reason) => AudioSourceStatus::Waiting(reason.clone()),
                        Live::Failed(message) => AudioSourceStatus::Failed(message.clone()),
                    },
                    None => match rejected.iter().find(|(id, _)| *id == source.id) {
                        Some((_, reason)) => AudioSourceStatus::Failed(reason.clone()),
                        None => AudioSourceStatus::Skipped(
                            self.skipped
                                .iter()
                                .find(|(id, _)| *id == source.id)
                                .map_or("Not available.", |(_, reason)| reason.as_str())
                                .to_owned(),
                        ),
                    },
                };
                AudioSourceState {
                    source: source.id,
                    status,
                }
            })
            .collect()
    }

    /// States of a session that captures no audio (every source skipped).
    pub fn idle_states(&self) -> Vec<AudioSourceState> {
        self.states(&[], &LiveRouting::default(), &[])
    }

    /// States of a session that was started without audio and cannot gain
    /// audio tracks while it runs: every enabled source waits for a restart.
    pub fn restart_states(&self) -> Vec<AudioSourceState> {
        self.sources
            .iter()
            .map(|source| AudioSourceState {
                source: source.id,
                status: AudioSourceStatus::Skipped(
                    "This session started without audio; restart it to record audio.".to_owned(),
                ),
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Live {
    Starting,
    Capturing,
    Waiting(String),
    Failed(String),
}

/// Runtime state of one capture track.
struct TrackRt {
    /// Capture and mixer track index (unique within the session).
    index: usize,
    source: u32,
    /// Executable of an application track. These may be absent without
    /// failing the session.
    process: Option<String>,
    /// May legitimately deliver no packet at all (a desktop remainder behind
    /// process loopback, which Windows may not feed while nothing plays).
    quiet_ok: bool,
    live: Live,
    gain: f32,
    /// Started by a live source change. Its failure only marks the source
    /// `Failed`: adding a broken device must not end a running replay.
    live_added: bool,
    /// Status note (applications a composed desktop cannot contain).
    note: Option<String>,
}

impl TrackRt {
    fn new(index: usize, planned: &PlannedTrack, gain: f32, live_added: bool) -> Self {
        Self {
            index,
            source: planned.source,
            process: match &planned.content {
                TrackContent::Application(exe) => Some(exe.clone()),
                _ => None,
            },
            quiet_ok: matches!(
                planned.content,
                TrackContent::DesktopExcluding(_) | TrackContent::DesktopExcludingMany(_)
            ),
            live: Live::Starting,
            gain,
            live_added,
            note: None,
        }
    }
}

impl TrackRt {
    /// Losing this track is routine and the capture thread recovers by
    /// itself: an application that exits, or the desktop remainder that
    /// reopens as the full mix when its excluded application exits. Never
    /// fails a session.
    fn loss_is_routine(&self) -> bool {
        self.process.is_some() || self.quiet_ok
    }
}

/// What one capture event does to a track. `Err` only for failures that must
/// end the session (see [`RecordingAudio::start`]); `Ok(None)`: no change.
/// Application tracks only ever become `Waiting`/`Failed`; device tracks end
/// a manual recording when lost, but only turn `Waiting` in a tolerant
/// (replay) session.
fn transition(rt: &TrackRt, tolerant: bool, event: AudioEvent) -> Result<Option<Live>, String> {
    Ok(match event {
        AudioEvent::TrackStarted { .. } => Some(Live::Capturing),
        AudioEvent::SourceLost { reason, .. } => match &rt.process {
            Some(exe) => Some(Live::Waiting(format!("{exe} is not running"))),
            None if tolerant || rt.loss_is_routine() => Some(Live::Waiting(reason)),
            None => return Err(format!("Audio source lost: {reason}")),
        },
        AudioEvent::Failed { message, .. } => {
            if rt.process.is_none() && !rt.live_added {
                return Err(format!("Audio capture failed: {message}"));
            }
            Some(Live::Failed(message))
        }
        // Persistent counters cover even throttled or lost events.
        AudioEvent::Overflow { .. } | AudioEvent::DeviceChanged { .. } => None,
    })
}

/// A desktop or microphone track whose device is not present cannot start:
/// an actionable error instead of a stream that never delivers.
fn check_device(planned: &PlannedTrack) -> Result<(), String> {
    let (flow, selected) = match &planned.stream {
        routing::StreamPlan::EndpointLoopback(device) => (DeviceFlow::Render, device.as_ref()),
        routing::StreamPlan::Microphone(MicrophoneChoice::Device(id)) => {
            (DeviceFlow::Capture, Some(id))
        }
        routing::StreamPlan::Microphone(MicrophoneChoice::Default) => (DeviceFlow::Capture, None),
        // Process loopback is system-wide: no device to check.
        _ => return Ok(()),
    };
    let devices = list_devices(flow).map_err(|e| e.to_string())?;
    if devices
        .iter()
        .any(|d| selected.map_or(d.is_default, |id| &d.id == id))
    {
        Ok(())
    } else {
        Err(format!(
            "Selected {flow:?} audio device is unavailable. Select an available device or disable this source."
        ))
    }
}

pub struct RecordingAudio {
    capture: AudioCapture,
    mixer: TimelineMixer,
    plan: AudioPlan,
    tracks: Vec<TrackRt>,
    /// Running tracks and the session's fixed isolated outputs.
    live: LiveRouting,
    /// Sources a live change could not start: `(source id, reason)`.
    rejected: Vec<(u32, String)>,
    last_note_check: Instant,
    origin: i64,
    initial_dropped: u64,
    /// Replay: dropped and late audio is counted and rendered as silence, and
    /// a lost device only turns its source `Waiting` instead of failing the
    /// session. A dead device stream (`Failed`) still fails it.
    tolerant: bool,
    changed: bool,
}

impl RecordingAudio {
    /// Starts capturing `plan`; `Ok(None)` when nothing is capturable.
    ///
    /// Startup policy: device tracks (desktop, microphone) must start within
    /// 5 s, else the session fails with an actionable error. Application
    /// tracks may stay `Waiting` ("<exe> is not running") and render silence
    /// until the audio engine finds the process (it re-resolves by
    /// executable); the session never fails because an app is closed. During
    /// the session a lost application track only becomes `Waiting` again.
    /// A lost device track fails a manual recording; a replay keeps running
    /// (silence, `Waiting`).
    pub fn start(plan: AudioPlan, wake: SyncSender<()>) -> Result<Option<Self>, String> {
        if plan.routing.tracks.is_empty() {
            return Ok(None);
        }
        for planned in &plan.routing.tracks {
            check_device(planned)?;
        }
        let capture = AudioCapture::start_plan(&plan.routing, AudioConfig::default(), Some(wake))
            .map_err(|e| e.to_string())?;
        // The engine starts the plan's tracks in order.
        let started: Vec<LiveTrack> = capture
            .tracks()
            .iter()
            .zip(&plan.routing.tracks)
            .map(|(info, planned)| LiveTrack {
                index: info.index,
                planned: planned.clone(),
            })
            .collect();
        let tracks: Vec<TrackRt> = started
            .iter()
            .map(|t| {
                let gain = f32::from(plan.gain_percent(t.planned.source)) / 100.0;
                TrackRt::new(t.index, &t.planned, gain, false)
            })
            .collect();
        let live = LiveRouting::new(started, plan.separate);
        let mut this = Self {
            capture,
            mixer: TimelineMixer::new(0, Vec::new(), false),
            plan,
            tracks,
            live,
            rejected: Vec::new(),
            last_note_check: Instant::now(),
            origin: 0,
            initial_dropped: 0,
            tolerant: false,
            changed: false,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while this
            .tracks
            .iter()
            .any(|t| t.process.is_none() && t.live == Live::Starting)
        {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("Audio startup timed out; recording was not started.".into());
            }
            if let Ok(event) = this
                .capture
                .events()
                .recv_timeout(left.min(Duration::from_millis(100)))
            {
                this.apply(event)?;
            }
            // Startup is not part of the recording. Discard early packets
            // until every device is ready, keeping startup bounded.
            this.capture.read_frames(|_| {});
        }
        for track in &mut this.tracks {
            if let (Live::Starting, Some(exe)) = (&track.live, &track.process) {
                track.live = Live::Waiting(format!("{exe} is not running"));
            }
        }
        this.origin = qpc_now_ticks();
        this.mixer = TimelineMixer::with_outputs(this.origin, this.live.outputs());
        for track in &this.tracks {
            this.mixer
                .add_source(track.index, track.gain, this.live.output_of(track.source));
            if let Some(source) = this.plan.sources.iter().find(|s| s.id == track.source) {
                this.mixer.set_noise_gate(track.index, source.noise_gate);
                this.mixer.reset_noise_gate(track.index);
            }
        }
        this.initial_dropped = this.capture.dropped_frames();
        this.changed = false;
        Ok(Some(this))
    }

    /// Updates track state from one capture event. `Err` only for failures
    /// that must end the session (see [`Self::start`]).
    fn apply(&mut self, event: AudioEvent) -> Result<(), String> {
        let index = match &event {
            AudioEvent::TrackStarted { track }
            | AudioEvent::SourceLost { track, .. }
            | AudioEvent::DeviceChanged { track, .. }
            | AudioEvent::Overflow { track, .. }
            | AudioEvent::Failed { track, .. } => *track,
        };
        // Events of removed tracks (unknown index) are stale: ignored.
        let Some(rt) = self.tracks.iter_mut().find(|t| t.index == index) else {
            return Ok(());
        };
        if matches!(
            &event,
            AudioEvent::TrackStarted { .. }
                | AudioEvent::SourceLost { .. }
                | AudioEvent::DeviceChanged { .. }
                | AudioEvent::Failed { .. }
        ) {
            self.mixer.reset_noise_gate(index);
        }
        if let Some(live) = transition(rt, self.tolerant, event)?
            && rt.live != live
        {
            rt.live = live;
            self.changed = true;
        }
        Ok(())
    }

    fn drain_events(&mut self) -> Result<(), String> {
        while let Ok(event) = self.capture.events().try_recv() {
            self.apply(event)?;
        }
        Ok(())
    }

    /// Current state of every requested source.
    pub fn states(&self) -> Vec<AudioSourceState> {
        self.plan.states(&self.tracks, &self.live, &self.rejected)
    }

    /// Applies changed audio sources to the running session without touching
    /// video, the encoder or the replay buffer (see [`clipper_audio::live`]).
    /// Sources that cannot start are reported `Failed`, never as capturing.
    pub fn reconfigure(&mut self, options: &RecordingAudioOptions) {
        let next = self.plan.replan(options);
        let gain_of = |source: u32| f32::from(next.gain_percent(source)) / 100.0;
        let change = self.live.apply(
            &next.routing,
            &mut self.capture,
            &mut self.mixer,
            gain_of,
            check_device,
        );
        for removed in &change.removed {
            self.tracks.retain(|t| t.index != removed.index);
        }
        for added in &change.added {
            let gain = gain_of(added.planned.source);
            self.tracks
                .push(TrackRt::new(added.index, &added.planned, gain, true));
        }
        // Plan order, like the running tracks.
        let order: Vec<usize> = self.live.tracks().iter().map(|t| t.index).collect();
        self.tracks
            .sort_by_key(|t| order.iter().position(|i| *i == t.index));
        for track in &mut self.tracks {
            track.gain = gain_of(track.source);
            self.mixer.set_gain(track.index, track.gain);
            if let Some(source) = next.sources.iter().find(|s| s.id == track.source) {
                self.mixer.set_noise_gate(track.index, source.noise_gate);
            }
        }
        for added in &change.added {
            self.mixer.reset_noise_gate(added.index);
        }
        if !change.is_empty() {
            log::info!(
                "audio sources changed live: +{} -{} failed {:?}",
                change.added.len(),
                change.removed.len(),
                change.failed
            );
        }
        self.rejected = change.failed;
        self.plan = next;
        self.changed = true;
    }

    /// The states, once, after they changed since the last call.
    fn take_changed_states(&mut self) -> Option<Vec<AudioSourceState>> {
        std::mem::take(&mut self.changed).then(|| self.states())
    }

    /// Post-gain peak of every captured source.
    pub fn levels(&mut self) -> Vec<SourceLevel> {
        let mut out: Vec<SourceLevel> = Vec::new();
        for track in &self.tracks {
            // Held peak since the last report: no transient or clip is missed.
            let peak = self.mixer.take_peak(track.index);
            match out.iter_mut().find(|l| l.source == track.source) {
                Some(level) => level.peak = level.peak.max(peak),
                None => out.push(SourceLevel {
                    source: track.source,
                    peak,
                }),
            }
        }
        out
    }

    /// Live gain change of one source (ramped by the mixer). Returns `false`
    /// when this session does not capture the source.
    pub fn set_gain(&mut self, source: u32, gain_percent: u16) -> bool {
        let gain = f32::from(gain_percent.min(MAX_AUDIO_GAIN_PERCENT)) / 100.0;
        let mut found = false;
        for track in &mut self.tracks {
            if track.source == source {
                track.gain = gain;
                self.mixer.set_gain(track.index, gain);
                found = true;
            }
        }
        found
    }

    pub fn set_noise_gate(&mut self, source: u32, settings: NoiseGateSettings) {
        for track in &self.tracks {
            if track.source == source {
                self.mixer.set_noise_gate(track.index, settings);
            }
        }
    }

    pub fn origin(&self) -> i64 {
        self.origin
    }

    /// Instant replay mode: audio gaps become silence plus a counter (see
    /// [`RecordingAudio::glitched_frames`]) and a lost device only turns its
    /// source `Waiting`.
    pub fn set_tolerant(&mut self, tolerant: bool) {
        self.tolerant = tolerant;
    }

    /// Audio frames lost to ring overflow or the mixing window since start.
    pub fn glitched_frames(&self) -> u64 {
        self.capture
            .dropped_frames()
            .saturating_sub(self.initial_dropped)
            + self.mixer.late_frames()
    }

    /// Mixer re-anchors of all sources (device clock drift against QPC or a
    /// timestamp discontinuity), for the session diagnostics.
    pub fn resync_stats(&self) -> clipper_audio::mixer::ResyncStats {
        self.mixer.resync_stats()
    }

    /// Reads and throws away pending audio (no video yet, nothing to align
    /// it to) so the capture rings never back up. Reports source failure.
    pub fn discard(&mut self) -> Result<(), String> {
        self.capture.read_frames(|_| {});
        self.drain_events()?;
        self.check_latched_failure()
    }

    /// The capture engine latches any source loss per track, which fails a
    /// manual recording for every device track (application tracks and
    /// desktop remainders may come and go). Checked per track so an unrelated application track cannot
    /// mask a lost microphone when its event was dropped by the bounded
    /// event channel.
    fn check_latched_failure(&self) -> Result<(), String> {
        if !self.tolerant
            && self.tracks.iter().any(|t| {
                !t.loss_is_routine() && !t.live_added && self.capture.track_failed(t.index)
            })
        {
            return Err("An audio source failed; recording is incomplete.".into());
        }
        Ok(())
    }

    /// Starts the audio timeline at the first video sample (`qpc` ticks).
    pub fn start_at(&mut self, qpc: i64) {
        self.mixer.skip_to(qpc);
    }

    /// Moves captured packets into the mixer and applies capture events.
    fn ingest(&mut self, finish: bool) -> Result<(), String> {
        let push = |mixer: &mut TimelineMixer, frame: clipper_audio::AudioFrameRef<'_>| {
            mixer.push_input(
                frame.track,
                frame.input,
                frame.timestamp,
                frame.channels,
                frame.samples,
            )
        };
        if finish {
            self.capture
                .stop_and_drain(|frame| push(&mut self.mixer, frame));
        } else {
            self.capture
                .read_frames(|frame| push(&mut self.mixer, frame));
        }
        for finished in self.capture.take_finished() {
            // Removed tracks were retired in the mixer when removed.
            if let Finished::Input { track, input } = finished {
                self.mixer.retire_input(track, input);
            }
        }
        self.refresh_notes();
        self.check_latched_failure()?;
        let dropped = self
            .capture
            .dropped_frames()
            .saturating_sub(self.initial_dropped);
        if dropped > 0 && !self.tolerant {
            return Err(format!(
                "Audio capture dropped {dropped} frames; recording is incomplete."
            ));
        }
        self.drain_events()
    }

    /// Re-reads (at most once a second) which applications a composed
    /// desktop track cannot contain, for its status and the log.
    fn refresh_notes(&mut self) {
        if self.last_note_check.elapsed() < NOTE_INTERVAL {
            return;
        }
        self.last_note_check = Instant::now();
        for track in &mut self.tracks {
            let names = self.capture.unseparable(track.index);
            let note = (!names.is_empty()).then(|| {
                format!(
                    "Not in desktop audio (would duplicate a separated app): {}",
                    names.join(", ")
                )
            });
            if note != track.note {
                if let Some(note) = &note {
                    log::warn!("audio source {}: {note}", track.source);
                }
                track.note = note;
                self.changed = true;
            }
        }
    }

    /// A device track that never delivered a sample fails a finished clip;
    /// application tracks and desktop remainders may legitimately have stayed
    /// silent.
    fn missing_device_audio(&self) -> bool {
        self.tracks.iter().any(|t| {
            t.process.is_none() && !t.quiet_ok && !t.live_added && !self.mixer.has_received(t.index)
        })
    }

    pub fn pump(
        &mut self,
        encoder: &mut dyn AudioSink,
        finish: bool,
        video_end: i64,
    ) -> Result<(), String> {
        self.ingest(finish)?;
        let video_limit = self.origin.saturating_add(video_end);
        let end = if finish {
            video_limit
        } else {
            qpc_now_ticks().saturating_sub(800_000).min(video_limit)
        };
        self.mixer.render_until(end, |track, time, samples| {
            encoder.submit_audio(track, samples, time)
        })?;
        if finish && self.missing_device_audio() {
            return Err(
                "An enabled audio source delivered no samples; clip is not reported as successful."
                    .into(),
            );
        }
        if self.mixer.late_frames() > 0 && !self.tolerant {
            return Err(format!(
                "Audio packets missed the bounded mixing window ({} frames); clip is not reported as successful.",
                self.mixer.late_frames()
            ));
        }
        Ok(())
    }
}

/// Sends per-source statuses (at start, then only on change) and levels (at
/// most about 15 Hz) of one session, and the empty lists when it ends.
pub struct AudioReporter {
    idle: Vec<AudioSourceState>,
    started: bool,
    last_meter: Instant,
    /// Levels of the last meter event, so an unchanged meter (all silent, or
    /// frozen sources) sends nothing and the UI never repaints for it.
    last_levels: Vec<SourceLevel>,
}

/// Whether a meter update must be sent: only when it differs from the last
/// one sent. The first update is always sent, so the UI sees every source.
fn levels_changed(last: &[SourceLevel], now: &[SourceLevel]) -> bool {
    last != now
}

impl AudioReporter {
    /// `idle`: the states to announce when the session captures no audio.
    pub fn new(idle: Vec<AudioSourceState>) -> Self {
        Self {
            idle,
            started: false,
            last_meter: Instant::now(),
            last_levels: Vec::new(),
        }
    }

    /// Never blocks except for the single announcement at session start.
    pub fn report(&mut self, endpoint: &RecorderEndpoint, audio: Option<&mut RecordingAudio>) {
        if !self.started {
            self.started = true;
            let states = audio
                .as_deref()
                .map_or_else(|| std::mem::take(&mut self.idle), RecordingAudio::states);
            crate::emit(endpoint, RecorderEvent::AudioSources(states));
        }
        let Some(audio) = audio else { return };
        if let Some(states) = audio.take_changed_states() {
            let _ = endpoint
                .events
                .try_send(RecorderEvent::AudioSources(states));
        }
        if self.last_meter.elapsed() >= METER_INTERVAL {
            self.last_meter = Instant::now();
            let levels = audio.levels();
            if levels_changed(&self.last_levels, &levels)
                && endpoint
                    .events
                    .try_send(RecorderEvent::AudioLevels(levels.clone()))
                    .is_ok()
            {
                self.last_levels = levels;
            }
        }
    }

    /// Session ended: clears the UI's meters and statuses.
    pub fn finish(endpoint: &RecorderEndpoint) {
        crate::emit(endpoint, RecorderEvent::AudioLevels(Vec::new()));
        crate::emit(endpoint, RecorderEvent::AudioSources(Vec::new()));
    }
}

/// Explicit, bounded 30-second audio diagnostic. File writes happen only on
/// the recorder worker; the WASAPI threads continue to feed bounded rings.
/// It records the enabled sources like a session would and writes the mix.
pub struct AudioTest {
    audio: RecordingAudio,
    file: std::io::BufWriter<std::fs::File>,
    path: std::path::PathBuf,
    bytes: u32,
    pub wake: std::sync::mpsc::Receiver<()>,
    started: Instant,
    pub reporter: AudioReporter,
}

impl AudioTest {
    pub fn set_gain(&mut self, source: u32, gain_percent: u16) {
        self.audio.set_gain(source, gain_percent);
    }

    pub fn set_noise_gate(&mut self, source: u32, settings: NoiseGateSettings) {
        self.audio.set_noise_gate(source, settings);
    }

    pub fn start(
        options: &RecordingAudioOptions,
        directory: &std::path::Path,
    ) -> Result<Self, String> {
        use std::io::Write;
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let (tx, wake) = std::sync::mpsc::sync_channel(1);
        let plan = AudioPlan::for_session(options, None);
        let reporter = AudioReporter::new(plan.idle_states());
        let audio =
            RecordingAudio::start(plan, tx)?.ok_or("Enable an audio source before testing.")?;
        let path = directory.join(format!("Clipper_AudioTest_{}.wav", qpc_now_ticks()));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let mut file = std::io::BufWriter::new(file);
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&36_u32.to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16_u32.to_le_bytes());
        header.extend_from_slice(&1_u16.to_le_bytes());
        header.extend_from_slice(&2_u16.to_le_bytes());
        header.extend_from_slice(&48_000_u32.to_le_bytes());
        header.extend_from_slice(&192_000_u32.to_le_bytes());
        header.extend_from_slice(&4_u16.to_le_bytes());
        header.extend_from_slice(&16_u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&0_u32.to_le_bytes());
        if let Err(error) = file.write_all(&header).and_then(|()| file.flush()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error.to_string());
        }
        Ok(Self {
            audio,
            file,
            path,
            bytes: 0,
            wake,
            started: Instant::now(),
            reporter,
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Statuses and levels for the UI (see [`AudioReporter::report`]).
    pub fn report(&mut self, endpoint: &RecorderEndpoint) {
        self.reporter.report(endpoint, Some(&mut self.audio));
    }

    pub fn pump(&mut self, finish: bool) -> Result<(), String> {
        use std::io::Write;
        self.audio.ingest(finish)?;
        let end = qpc_now_ticks().saturating_sub(if finish { 0 } else { 800_000 });
        let file = &mut self.file;
        let bytes = &mut self.bytes;
        self.audio.mixer.render_until(end, |track, _, samples| {
            if track == 0 {
                for sample in samples {
                    file.write_all(&((*sample * 32767.0).round() as i16).to_le_bytes())
                        .map_err(|e| e.to_string())?;
                }
                *bytes += samples.len() as u32 * 2;
            }
            Ok::<_, String>(())
        })?;
        if finish && self.audio.missing_device_audio() {
            return Err("An enabled source delivered no samples.".into());
        }
        if self.audio.mixer.late_frames() > 0 {
            return Err("Audio packets missed the mixing window.".into());
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<std::path::PathBuf, String> {
        use std::io::{Seek, SeekFrom, Write};
        let result = (|| {
            self.pump(true)?;
            self.file.flush().map_err(|e| e.to_string())?;
            self.file
                .seek(SeekFrom::Start(4))
                .map_err(|e| e.to_string())?;
            self.file
                .write_all(&(36 + self.bytes).to_le_bytes())
                .map_err(|e| e.to_string())?;
            self.file
                .seek(SeekFrom::Start(40))
                .map_err(|e| e.to_string())?;
            self.file
                .write_all(&self.bytes.to_le_bytes())
                .map_err(|e| e.to_string())?;
            self.file.flush().map_err(|e| e.to_string())?;
            Ok(self.path.clone())
        })();
        let path = self.path.clone();
        drop(self);
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: u32, target: AudioSourceTarget) -> RecordingAudioSource {
        RecordingAudioSource {
            id,
            target,
            gain_percent: 100,
            noise_gate: NoiseGateSettings::default(),
        }
    }

    fn options(sources: Vec<RecordingAudioSource>) -> RecordingAudioOptions {
        RecordingAudioOptions {
            sources,
            separate_tracks: true,
        }
    }

    fn requests(selection: &AudioSelection) -> Vec<(u32, SourceRequestKind)> {
        selection
            .sources
            .iter()
            .map(|r| (r.id, r.kind.clone()))
            .collect()
    }

    #[test]
    fn game_follows_the_captured_window_executable() {
        let sources = options(vec![source(1, AudioSourceTarget::Game)]);
        let (selection, skipped) = resolve_selection(&sources, Some(42), |hwnd| {
            (hwnd == 42).then(|| "game.exe".into())
        });
        assert!(skipped.is_empty());
        assert_eq!(
            requests(&selection),
            [(
                1,
                SourceRequestKind::Process(ProcessTarget::Executable("game.exe".into()))
            )]
        );
    }

    #[test]
    fn game_is_skipped_for_a_display_or_unknown_window() {
        let sources = options(vec![source(1, AudioSourceTarget::Game)]);
        let (selection, skipped) = resolve_selection(&sources, None, |_| Some("x.exe".into()));
        assert!(selection.sources.is_empty());
        assert_eq!(skipped[0].0, 1);
        assert!(skipped[0].1.contains("whole display"));
        let (selection, skipped) = resolve_selection(&sources, Some(7), |_| None);
        assert!(selection.sources.is_empty());
        assert_eq!(skipped.len(), 1);
    }

    #[test]
    fn devices_and_applications_map_one_to_one() {
        let sources = options(vec![
            source(1, AudioSourceTarget::Application("Discord.exe".into())),
            source(2, AudioSourceTarget::Desktop(Some("dev".into()))),
            source(3, AudioSourceTarget::Microphone(None)),
            source(4, AudioSourceTarget::Microphone(Some("mic".into()))),
            source(5, AudioSourceTarget::Desktop(None)),
        ]);
        let (selection, skipped) = resolve_selection(&sources, None, |_| None);
        assert_eq!(
            requests(&selection),
            [
                (
                    1,
                    SourceRequestKind::Process(ProcessTarget::Executable("Discord.exe".into()))
                ),
                (
                    2,
                    SourceRequestKind::Desktop {
                        device: Some("dev".into())
                    }
                ),
                (3, SourceRequestKind::Microphone(MicrophoneChoice::Default)),
                (
                    4,
                    SourceRequestKind::Microphone(MicrophoneChoice::Device("mic".into()))
                ),
            ]
        );
        // The fifth source exceeds the MP4 writer's isolated track limit.
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, 5);
    }

    #[test]
    fn plan_counts_tracks_from_what_can_be_captured() {
        let caps = AudioCapabilities {
            process_loopback: true,
        };
        let sources = options(vec![
            source(1, AudioSourceTarget::Game),
            source(2, AudioSourceTarget::Microphone(None)),
        ]);
        let plan = AudioPlan::build(&sources, None, |_| None, caps, &DefaultDevices::default());
        assert_eq!(plan.encoder_tracks(), 2);
        assert_eq!(plan.labels(), ["Mix", "Microphone"]);
        let states = plan.idle_states();
        assert!(matches!(states[0].status, AudioSourceStatus::Skipped(_)));
        let mut no_separate = sources.clone();
        no_separate.separate_tracks = false;
        let plan = AudioPlan::build(
            &no_separate,
            None,
            |_| None,
            caps,
            &DefaultDevices::default(),
        );
        assert_eq!(
            (plan.encoder_tracks(), plan.labels()),
            (1, vec![String::from("Mix")])
        );
        let none = options(vec![source(1, AudioSourceTarget::Game)]);
        let plan = AudioPlan::build(&none, None, |_| None, caps, &DefaultDevices::default());
        assert_eq!(plan.encoder_tracks(), 0);
        assert!(plan.labels().is_empty());
    }

    const MODERN: AudioCapabilities = AudioCapabilities {
        process_loopback: true,
    };
    const LEGACY: AudioCapabilities = AudioCapabilities {
        process_loopback: false,
    };

    fn plan_for(
        sources: Vec<RecordingAudioSource>,
        caps: AudioCapabilities,
        defaults: &DefaultDevices,
    ) -> AudioPlan {
        AudioPlan::build(
            &options(sources),
            Some(1),
            |_| Some("game.exe".into()),
            caps,
            defaults,
        )
    }

    fn status_of(plan: &AudioPlan, id: u32) -> AudioSourceStatus {
        plan.idle_states()
            .into_iter()
            .find(|s| s.source == id)
            .unwrap()
            .status
    }

    #[test]
    fn legacy_windows_reports_apps_unavailable_and_tracks_match() {
        let plan = plan_for(
            vec![
                source(1, AudioSourceTarget::Game),
                source(2, AudioSourceTarget::Application("Discord.exe".into())),
                source(3, AudioSourceTarget::Desktop(None)),
                source(4, AudioSourceTarget::Microphone(None)),
            ],
            LEGACY,
            &DefaultDevices::default(),
        );
        // Mix + desktop (full mix) + microphone: nothing silently collapsed.
        assert_eq!(plan.encoder_tracks(), 3);
        assert_eq!(plan.labels(), ["Mix", "Desktop (full mix)", "Microphone"]);
        assert_eq!(plan.routing.tracks.len() + 1, plan.encoder_tracks());
        for id in [1, 2] {
            assert!(
                matches!(status_of(&plan, id), AudioSourceStatus::Skipped(m) if m.contains("Windows 11")),
                "source {id}"
            );
        }
    }

    #[test]
    fn every_source_has_a_track_or_a_reason() {
        let plan = plan_for(
            vec![
                source(1, AudioSourceTarget::Game),
                source(2, AudioSourceTarget::Application("Discord.exe".into())),
                source(3, AudioSourceTarget::Desktop(None)),
                source(4, AudioSourceTarget::Microphone(None)),
            ],
            MODERN,
            &DefaultDevices::default(),
        );
        // Two apps: the desktop remainder is composed without both (Bug 1
        // regression: it used to be skipped as "Desktop audio is off").
        assert_eq!(
            plan.labels(),
            [
                "Mix",
                "game.exe",
                "Discord.exe",
                "Desktop (without game.exe, Discord.exe)",
                "Microphone"
            ]
        );
        assert_eq!(plan.encoder_tracks(), 5);
        assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
        let planned: Vec<u32> = plan.routing.tracks.iter().map(|t| t.source).collect();
        assert_eq!(planned, [1, 2, 3, 4]);
    }

    #[test]
    fn one_app_makes_the_desktop_track_exclude_it() {
        let plan = plan_for(
            vec![
                source(1, AudioSourceTarget::Application("Discord.exe".into())),
                source(2, AudioSourceTarget::Desktop(None)),
            ],
            MODERN,
            &DefaultDevices::default(),
        );
        assert_eq!(
            plan.labels(),
            ["Mix", "Discord.exe", "Desktop (without Discord.exe)"]
        );
    }

    #[test]
    fn game_and_same_application_are_recorded_once() {
        let plan = plan_for(
            vec![
                source(1, AudioSourceTarget::Game),
                source(2, AudioSourceTarget::Application("GAME.EXE".into())),
            ],
            MODERN,
            &DefaultDevices::default(),
        );
        assert_eq!(plan.encoder_tracks(), 2);
        assert!(matches!(
            status_of(&plan, 2),
            AudioSourceStatus::Skipped(m) if m.contains("already recorded")
        ));
    }

    #[test]
    fn explicit_default_device_is_not_recorded_twice() {
        let defaults = DefaultDevices {
            render: Some("render-default".into()),
            capture: Some("mic-default".into()),
        };
        let plan = plan_for(
            vec![
                source(1, AudioSourceTarget::Desktop(None)),
                source(2, AudioSourceTarget::Desktop(Some("render-default".into()))),
                source(3, AudioSourceTarget::Microphone(None)),
                source(4, AudioSourceTarget::Microphone(Some("mic-default".into()))),
            ],
            MODERN,
            &defaults,
        );
        assert_eq!(plan.labels(), ["Mix", "Desktop (full mix)", "Microphone"]);
        for id in [2, 4] {
            assert!(matches!(
                status_of(&plan, id),
                AudioSourceStatus::Skipped(_)
            ));
        }
        // A genuinely different device is kept.
        let plan = plan_for(
            vec![
                source(1, AudioSourceTarget::Desktop(None)),
                source(2, AudioSourceTarget::Desktop(Some("headset".into()))),
            ],
            MODERN,
            &defaults,
        );
        assert_eq!(plan.encoder_tracks(), 3);
        // Alone, an explicit device stays pinned (it does not follow the default).
        let plan = plan_for(
            vec![source(
                1,
                AudioSourceTarget::Desktop(Some("render-default".into())),
            )],
            MODERN,
            &defaults,
        );
        assert_eq!(
            plan.routing.tracks[0].stream,
            clipper_audio::routing::StreamPlan::EndpointLoopback(Some("render-default".into()))
        );
    }

    #[test]
    fn planned_tracks_follow_capabilities_and_stay_below_the_upper_bound() {
        let opts = options(vec![
            source(1, AudioSourceTarget::Game),
            source(2, AudioSourceTarget::Application("Discord.exe".into())),
            source(3, AudioSourceTarget::Desktop(None)),
            source(4, AudioSourceTarget::Microphone(None)),
        ]);
        let defaults = DefaultDevices::default();
        assert_eq!(planned_audio_tracks_with(&opts, MODERN, &defaults), 4);
        // Legacy: apps dropped, desktop becomes the full mix again.
        assert_eq!(planned_audio_tracks_with(&opts, LEGACY, &defaults), 3);
        for caps in [MODERN, LEGACY] {
            assert!(
                planned_audio_tracks_with(&opts, caps, &defaults) <= opts.encoded_track_count()
            );
        }
        let mut mixed_only = opts;
        mixed_only.separate_tracks = false;
        assert_eq!(planned_audio_tracks_with(&mixed_only, MODERN, &defaults), 1);
    }

    fn track(process: Option<&str>, quiet_ok: bool) -> TrackRt {
        TrackRt {
            index: 0,
            source: 1,
            process: process.map(str::to_owned),
            quiet_ok,
            live: Live::Capturing,
            gain: 1.0,
            live_added: false,
            note: None,
        }
    }

    /// TEST J / M: a device source added live that fails, or whose worker
    /// exits, is reported `Failed` and never shown as capturing; it does not
    /// end the replay (other sources keep recording).
    #[test]
    fn failed_live_added_device_is_failed_not_fatal() {
        let mut rt = track(None, false);
        rt.live_added = true;
        for tolerant in [false, true] {
            assert_eq!(
                transition(&rt, tolerant, failed("activation failed")),
                Ok(Some(Live::Failed("activation failed".into())))
            );
        }
        // Devices present at session start keep their strict policy.
        assert!(transition(&track(None, false), true, failed("gone")).is_err());
    }

    /// TEST M: a stream that exits after starting is reported, not kept
    /// as capturing.
    #[test]
    fn worker_loss_after_start_leaves_capturing() {
        let rt = track(None, false);
        assert_eq!(
            transition(&rt, true, lost("device removed")),
            Ok(Some(Live::Waiting("device removed".into())))
        );
        let app = track(Some("Spotify.exe"), false);
        assert_eq!(
            transition(&app, true, lost("process exited")),
            Ok(Some(Live::Waiting("Spotify.exe is not running".into())))
        );
    }

    /// Status mapping of a running session: isolated tracks follow the
    /// session's fixed outputs, a live-added source in a separate-tracks
    /// session is honestly "mix only", refused sources are `Failed`, and
    /// the composed desktop carries its limitation note.
    #[test]
    fn states_report_outputs_refusals_and_notes() {
        let caps = MODERN;
        let opts = RecordingAudioOptions {
            sources: vec![
                source(1, AudioSourceTarget::Desktop(None)),
                source(2, AudioSourceTarget::Application("Spotify.exe".into())),
                source(3, AudioSourceTarget::Application("Discord.exe".into())),
                source(4, AudioSourceTarget::Microphone(None)),
            ],
            separate_tracks: true,
        };
        let plan = AudioPlan::build(&opts, None, |_| None, caps, &DefaultDevices::default());
        // Bug 1: the desktop is planned (composed) next to both apps.
        assert_eq!(plan.routing.tracks.len(), 4);
        assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
        let started: Vec<LiveTrack> = plan.routing.tracks[..3]
            .iter()
            .enumerate()
            .map(|(index, planned)| LiveTrack {
                index,
                planned: planned.clone(),
            })
            .collect();
        let live = LiveRouting::new(started, true);
        let mut tracks: Vec<TrackRt> = live
            .tracks()
            .iter()
            .map(|t| {
                let mut rt = TrackRt::new(t.index, &t.planned, 1.0, false);
                rt.live = Live::Capturing;
                rt
            })
            .collect();
        tracks[0].note = Some("Not in desktop audio: explorer.exe".into());
        assert!(tracks[0].quiet_ok);
        // Source 4 was added live and is capturing (mix only).
        let mut mic = TrackRt::new(9, &plan.routing.tracks[3], 1.0, true);
        mic.live = Live::Capturing;
        tracks.push(mic);
        let states = plan.states(&tracks, &live, &[]);
        let status = |id: u32| {
            states
                .iter()
                .find(|s| s.source == id)
                .unwrap()
                .status
                .clone()
        };
        assert_eq!(
            status(1),
            AudioSourceStatus::Capturing {
                track: Some(1),
                note: Some("Not in desktop audio: explorer.exe".into())
            }
        );
        assert_eq!(
            status(3),
            AudioSourceStatus::Capturing {
                track: Some(3),
                note: None
            }
        );
        assert!(matches!(
            status(4),
            AudioSourceStatus::Capturing {
                track: None,
                note: Some(_)
            }
        ));
        let refused = plan.states(
            &tracks[..3],
            &live,
            &[(4, "Selected Capture audio device is unavailable.".into())],
        );
        assert_eq!(
            refused[3].status,
            AudioSourceStatus::Failed("Selected Capture audio device is unavailable.".into())
        );
    }

    fn lost(reason: &str) -> AudioEvent {
        AudioEvent::SourceLost {
            track: 0,
            reason: reason.into(),
        }
    }

    fn failed(message: &str) -> AudioEvent {
        AudioEvent::Failed {
            track: 0,
            message: message.into(),
        }
    }

    #[test]
    fn lost_application_waits_and_never_fails_the_session() {
        for tolerant in [false, true] {
            let rt = track(Some("game.exe"), false);
            assert_eq!(
                transition(&rt, tolerant, lost("process 5 has exited")),
                Ok(Some(Live::Waiting("game.exe is not running".into())))
            );
            assert_eq!(
                transition(&rt, tolerant, failed("boom")),
                Ok(Some(Live::Failed("boom".into())))
            );
            assert_eq!(
                transition(&rt, tolerant, AudioEvent::TrackStarted { track: 0 }),
                Ok(Some(Live::Capturing))
            );
        }
    }

    #[test]
    fn lost_device_fails_a_manual_recording_but_not_a_replay() {
        let rt = track(None, false);
        assert!(transition(&rt, false, lost("unplugged")).is_err());
        assert_eq!(
            transition(&rt, true, lost("unplugged")),
            Ok(Some(Live::Waiting("unplugged".into())))
        );
        // A dead stream fails either way.
        assert!(transition(&rt, false, failed("gone")).is_err());
        assert!(transition(&rt, true, failed("gone")).is_err());
    }

    #[test]
    fn desktop_remainder_loss_is_routine() {
        let rt = track(None, true);
        assert!(rt.loss_is_routine());
        assert_eq!(
            transition(&rt, false, lost("target exited")),
            Ok(Some(Live::Waiting("target exited".into())))
        );
        assert!(!track(None, false).loss_is_routine());
    }

    #[test]
    fn counter_events_do_not_change_state() {
        let rt = track(None, false);
        for event in [
            AudioEvent::Overflow {
                track: 0,
                frames: 9,
            },
            AudioEvent::DeviceChanged {
                track: 0,
                device: "x".into(),
            },
        ] {
            assert_eq!(transition(&rt, false, event), Ok(None));
        }
    }

    #[test]
    fn meter_is_sent_only_when_levels_change() {
        let level = |peak| vec![SourceLevel { source: 1, peak }];
        assert!(levels_changed(&[], &level(0.0)));
        assert!(!levels_changed(&level(0.0), &level(0.0)));
        assert!(!levels_changed(&level(0.4), &level(0.4)));
        assert!(levels_changed(&level(0.4), &level(0.0)));
        // ~30 Hz: smooth meters, bounded event traffic.
        assert!((Duration::from_millis(16)..=Duration::from_millis(50)).contains(&METER_INTERVAL));
    }

    #[test]
    fn no_sources_do_not_start_audio_workers() {
        let (wake, _) = std::sync::mpsc::sync_channel(1);
        let plan = AudioPlan::for_session(&options(Vec::new()), None);
        assert!(RecordingAudio::start(plan, wake).unwrap().is_none());
    }

    #[test]
    fn unavailable_explicit_devices_fail_before_recording_starts() {
        let (wake, _) = std::sync::mpsc::sync_channel(1);
        let sources = options(vec![source(
            1,
            AudioSourceTarget::Desktop(Some("clipper-nonexistent-test-endpoint".into())),
        )]);
        let plan = AudioPlan::for_session(&sources, None);
        let result = RecordingAudio::start(plan, wake);
        assert!(matches!(result, Err(message) if message.contains("unavailable")));
    }
}
