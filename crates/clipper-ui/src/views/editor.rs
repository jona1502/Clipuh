//! Clip editor: preview playback, in/out trim points and stream-copy export.
//! The source file is only ever read; exports go to a new file next to it.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clipper_editor::{AudioTrack, EditSession, format_timestamp, parse_timestamp};
use clipper_media::audio::WaveformSet;
use clipper_media::player::{Player, PlayerEvent, PlayerFrame};
use clipper_media::trim::{self, TrimOutcome};
use clipper_platform::windows::shell;
use gpui::prelude::*;
use gpui::{
    Bounds, Context, Div, Entity, EventEmitter, FocusHandle, FontWeight, MouseButton, ObjectFit,
    Pixels, RenderImage, SharedString, Task, Window, actions, canvas, div, img, px, relative,
};

use crate::components::{
    ButtonVariant, TextInput, TextInputEvent, button, empty_state, kbd, panel, shortcut,
};
use crate::theme::{ActiveTheme, Theme};

actions!(clip_editor, [TogglePlayback, SetIn, SetOut]);

pub(super) const CONTEXT: &str = "ClipEditor";
/// Preview frames are decoded at most this wide; the preview never shows more.
const PREVIEW_MAX_WIDTH: u32 = 1280;

const CANCELLED: &str = "cancelled";
/// Shortest selection the exporter accepts.
const MIN_SELECTION: Duration = Duration::from_millis(100);

/// The selected range of the clip. Always `start + MIN_SELECTION <= end <= duration`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Trim {
    pub start: Duration,
    pub end: Duration,
}

impl Trim {
    pub fn full(duration: Duration) -> Self {
        Self {
            start: Duration::ZERO,
            end: duration,
        }
    }

    pub fn is_full(&self, duration: Duration) -> bool {
        *self == Self::full(duration)
    }

    pub fn set_in(&mut self, at: Duration) -> Result<(), &'static str> {
        if at + MIN_SELECTION > self.end {
            return Err("The in-point must be before the out-point.");
        }
        self.start = at;
        Ok(())
    }

    pub fn set_out(&mut self, at: Duration, duration: Duration) -> Result<(), &'static str> {
        let at = at.min(duration);
        if at < self.start + MIN_SELECTION {
            return Err("The out-point must be after the in-point.");
        }
        self.end = at;
        Ok(())
    }
}

enum Export {
    Idle,
    Running {
        progress: f32,
        cancel: Arc<AtomicBool>,
        _task: Task<()>,
    },
    Done(TrimOutcome),
    Failed(String),
}

enum ExportMessage {
    Progress(f32),
    Finished(Result<TrimOutcome, String>),
}

pub struct ClipEditor {
    path: PathBuf,
    focus: FocusHandle,
    /// Dropped while the Editor page is not shown, so decoding and audio stop.
    player: Option<Player>,
    _player_events: Option<Task<()>>,
    /// Freed from the GPU atlas as soon as it is replaced, even if the window
    /// is not rendering (minimized), so playback never piles up images.
    frame: Option<Arc<RenderImage>>,
    duration: Option<Duration>,
    /// The current player has loaded and accepts commands.
    ready: bool,
    /// Authoritative position: last value reported by the player or set by
    /// a seek. Edits (set in/out) and the time readout use it.
    position: Duration,
    /// Drawn playhead: `position` interpolated between player updates.
    playhead: playhead::PlayheadClock,
    /// Playhead x (px from the timeline origin) in the last rendered frame.
    playhead_drawn_x: Cell<f32>,
    /// A vsync check for the playhead is queued (at most one at a time).
    playhead_frame_queued: Cell<bool>,
    playing: bool,
    ended: bool,
    error: Option<String>,
    trim: Option<Trim>,
    notice: Option<SharedString>,
    export: Export,
    timeline: Rc<Cell<Bounds<Pixels>>>,
    scrubbing: bool,
    edits: Option<EditSession>,
    start_input: Entity<TextInput>,
    end_input: Entity<TextInput>,
    tab: usize,
    zoom: f32,
    dragging: Option<bool>,
    drag_origin: Option<(Pixels, Trim)>,
    thumbnails: Vec<Arc<RenderImage>>,
    thumbnail_task: Option<Task<()>>,
    audio_tracks: usize,
    audio_metadata: Option<Vec<AudioTrack>>,
    last_audio_mix: Option<Vec<AudioTrack>>,
    waveforms: Option<Arc<WaveformSet>>,
    /// Column peaks of the drawn waveforms (see `audio::PeakCache`).
    waveform_peaks: Rc<std::cell::RefCell<audio::PeakCache>>,
    waveform_task: Option<Task<()>>,
    waveform_error: Option<String>,
    audio_start_input: Entity<TextInput>,
    audio_end_input: Entity<TextInput>,
    audio_slider_focus: Vec<(FocusHandle, FocusHandle)>,
    audio_dragging: Option<(bool, Pixels, clipper_editor::Segment)>,
    pending_seek: Option<Duration>,
    media_duration: Option<Duration>,
    pub(super) preview_fullscreen: bool,
    fullscreen_before_preview: bool,
}

pub(super) enum EditorEvent {
    Exported,
    PreviewChanged,
}
impl EventEmitter<EditorEvent> for ClipEditor {}

impl ClipEditor {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let start_input = cx.new(|cx| TextInput::new(cx).placeholder("00:00.000").tab_index(30));
        let end_input = cx.new(|cx| TextInput::new(cx).placeholder("00:00.000").tab_index(31));
        for (input, start) in [(&start_input, true), (&end_input, false)] {
            cx.subscribe_in(
                input,
                window,
                move |this, input, event, window, cx| match event {
                    TextInputEvent::Submitted(text) => {
                        let result = parse_timestamp(text).and_then(|at| {
                            let trim = this.trim.ok_or("Select a segment first.")?;
                            let (a, b) = if start {
                                (at, trim.end)
                            } else {
                                (trim.start, at)
                            };
                            this.edits.as_mut().ok_or("Clip is loading.")?.trim(a, b)
                        });
                        if let Err(error) = result {
                            input.update(cx, |input, cx| input.set_error(Some(error.into()), cx));
                        } else {
                            this.sync_trim(cx);
                            window.focus(&this.focus);
                        }
                    }
                    TextInputEvent::Dismissed => {
                        this.sync_trim(cx);
                        window.focus(&this.focus);
                    }
                    _ => {}
                },
            )
            .detach();
        }
        let audio_start_input =
            cx.new(|cx| TextInput::new(cx).placeholder("00:00.000").tab_index(32));
        let audio_end_input =
            cx.new(|cx| TextInput::new(cx).placeholder("00:00.000").tab_index(33));
        for (input, start) in [(&audio_start_input, true), (&audio_end_input, false)] {
            cx.subscribe_in(
                input,
                window,
                move |this, input, event, window, cx| match event {
                    TextInputEvent::Submitted(text) => {
                        let result = parse_timestamp(text).and_then(|at| {
                            let edits = this.edits.as_mut().ok_or("Clip is loading.")?;
                            let track = edits
                                .selected_audio()
                                .and_then(|i| edits.audio_tracks().get(i))
                                .ok_or("Select an audio track first.")?;
                            let segment = track
                                .selected_segment
                                .and_then(|i| track.segments.get(i))
                                .copied()
                                .ok_or("Select an audio segment first.")?;
                            edits.trim_audio(
                                if start { at } else { segment.start },
                                if start { segment.end } else { at },
                            )
                        });
                        if let Err(error) = result {
                            input.update(cx, |input, cx| input.set_error(Some(error.into()), cx));
                        } else {
                            this.sync_audio(cx);
                            window.focus(&this.focus);
                        }
                    }
                    TextInputEvent::Dismissed => {
                        this.sync_audio(cx);
                        window.focus(&this.focus);
                    }
                    _ => {}
                },
            )
            .detach();
        }
        let mut this = Self {
            path,
            focus,
            player: None,
            _player_events: None,
            frame: None,
            duration: None,
            ready: false,
            position: Duration::ZERO,
            playhead: playhead::PlayheadClock::default(),
            playhead_drawn_x: Cell::new(0.),
            playhead_frame_queued: Cell::new(false),
            playing: false,
            ended: false,
            error: None,
            trim: None,
            notice: None,
            export: Export::Idle,
            timeline: Rc::default(),
            scrubbing: false,
            edits: None,
            start_input,
            end_input,
            tab: 0,
            zoom: 1.,
            dragging: None,
            drag_origin: None,
            thumbnails: Vec::new(),
            thumbnail_task: None,
            audio_tracks: 0,
            audio_metadata: None,
            last_audio_mix: None,
            waveforms: None,
            waveform_peaks: Rc::default(),
            waveform_task: None,
            waveform_error: None,
            audio_start_input,
            audio_end_input,
            audio_slider_focus: Vec::new(),
            audio_dragging: None,
            pending_seek: None,
            media_duration: None,
            preview_fullscreen: false,
            fullscreen_before_preview: false,
        };
        this.resume(window, cx);
        let path = this.path.clone();
        cx.spawn(async move |this, cx| {
            let info = cx
                .background_spawn(async move {
                    let info = trim::probe(&path)?;
                    let tracks =
                        clipper_media::audio::discover(&path).map_err(trim::TrimError::Media)?;
                    Ok::<_, trim::TrimError>((info, tracks))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match info {
                    Ok((info, tracks)) => {
                        this.audio_slider_focus = tracks
                            .iter()
                            .enumerate()
                            .map(|(i, _)| {
                                (
                                    cx.focus_handle()
                                        .tab_stop(true)
                                        .tab_index(40 + i as isize * 2),
                                    cx.focus_handle()
                                        .tab_stop(true)
                                        .tab_index(41 + i as isize * 2),
                                )
                            })
                            .collect();
                        this.audio_metadata = Some(tracks);
                        this.audio_tracks = info.audio_tracks;
                        this.media_duration = Some(info.duration);
                        if let Some(duration) = this.duration {
                            let duration = duration.min(info.duration);
                            this.duration = Some(duration);
                            this.edits.get_or_insert_with(|| EditSession::new(duration));
                            this.sync_trim(cx);
                        }
                    }
                    Err(error) => {
                        this.error = Some(error.to_string());
                        this.ready = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opens the player (again) and returns to the last position.
    pub fn resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        if self.player.is_some() {
            return;
        }
        let (player, events) = Player::open(&self.path, PREVIEW_MAX_WIDTH);
        self.player = Some(player);
        self.ready = false;
        self.error = None;
        self.ended = false;
        self.pending_seek = None;
        self.last_audio_mix = None;
        self.sync_audio(cx);
        if self.waveforms.is_none() && self.waveform_task.is_none() {
            let receiver = clipper_media::audio::waveform(&self.path);
            self.waveform_task = Some(cx.spawn(async move |this, cx| {
                while let Ok(result) = receiver.recv().await {
                    if this
                        .update(cx, |this, cx| {
                            match result {
                                Ok(waveforms) => this.waveforms = Some(waveforms),
                                Err(error) => {
                                    this.waveform_error = Some(error.clone());
                                    this.notice =
                                        Some(format!("Waveform extraction failed: {error}").into());
                                }
                            }
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        if self.thumbnails.is_empty() && self.thumbnail_task.is_none() {
            let frames = clipper_media::thumbnail::strip(&self.path, 12, 160);
            self.thumbnail_task = Some(cx.spawn_in(window, async move |this, cx| {
                while let Ok(frame) = frames.recv().await {
                    let (_, frame) = match frame {
                        Ok(frame) => frame,
                        Err(error) => {
                            let _ = this.update_in(cx, |this, _, cx| {
                                this.notice = Some(
                                    format!("Timeline thumbnails unavailable: {error}").into(),
                                );
                                cx.notify();
                            });
                            break;
                        }
                    };
                    if this
                        .update_in(cx, |this, _, cx| {
                            if this.thumbnails.len() < 12 {
                                let frame = Arc::new(PlayerFrame {
                                    width: frame.width,
                                    height: frame.height,
                                    bgra: frame.bgra,
                                    position: Duration::ZERO,
                                });
                                if let Some(image) = render_image(frame) {
                                    this.thumbnails.push(image);
                                }
                                cx.notify();
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        self._player_events = Some(cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.on_player(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Releases the decoder, audio output and preview frame; keeps trim state.
    pub fn suspend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_drag(cx);
        if let Some(edits) = &mut self.edits {
            edits.finish_audio_gesture();
        }
        self._player_events = None;
        self.thumbnail_task = None;
        self.waveform_task = None;
        release(self.player.take());
        self.ready = false;
        self.playing = false;
        self.release_images(window);
        cx.notify();
    }

    /// Frees the preview image from the GPU atlas; call before dropping the editor.
    pub fn release_images(&mut self, window: &mut Window) {
        for image in self.thumbnails.drain(..) {
            let _ = window.drop_image(image);
        }
        if let Some(image) = self.frame.take() {
            let _ = window.drop_image(image);
        }
    }

    fn on_player(&mut self, event: PlayerEvent, window: &mut Window, cx: &mut Context<Self>) {
        let at = match &event {
            PlayerEvent::Frame(frame) => Some(frame.position),
            PlayerEvent::Position(position) => Some(*position),
            _ => None,
        };
        if self.ended && !self.playing && at.is_some() {
            return;
        }
        if self.playing
            && let Some(at) = at
        {
            if let Some(target) = self.pending_seek {
                if at.abs_diff(target) > Duration::from_millis(200) {
                    return;
                }
                self.pending_seek = None;
            }
            if let Some(edits) = &self.edits {
                match playback_step(edits, at) {
                    PlaybackStep::Continue => {}
                    PlaybackStep::Seek(target) => {
                        self.pending_seek = Some(target);
                        // Show the jump now instead of freezing until the
                        // first frame after the seek arrives.
                        self.playhead.jump(target, Instant::now());
                        if let Some(player) = &self.player {
                            player.seek(target);
                        }
                        return;
                    }
                    PlaybackStep::End(end) => {
                        if let Some(player) = &self.player {
                            player.pause();
                        }
                        self.playing = false;
                        self.ended = true;
                        self.position = end;
                        self.playhead.stop(end);
                        cx.notify();
                        return;
                    }
                }
            }
        }
        match event {
            PlayerEvent::Loaded { duration, .. } => {
                self.ready = true;
                let duration = self
                    .media_duration
                    .map_or(duration, |actual| duration.min(actual));
                self.duration = Some(duration);
                if self.media_duration.is_some() && self.edits.is_none() {
                    self.edits = Some(EditSession::new(duration));
                }
                let trim = self.trim.get_or_insert(Trim::full(duration));
                trim.end = trim.end.min(duration);
                self.sync_trim(cx);
                // The player ignores commands until it has loaded; return to where we were.
                if let Some(player) = &self.player
                    && self.position > Duration::ZERO
                {
                    self.position = self.position.min(duration);
                    player.seek(self.position);
                }
                self.playhead.stop(self.position);
            }
            PlayerEvent::Frame(frame) => {
                self.set_reported_position(frame.position);
                if let Some(image) = render_image(frame)
                    && let Some(old) = self.frame.replace(image)
                {
                    let _ = window.drop_image(old);
                }
            }
            PlayerEvent::Position(position) => self.set_reported_position(position),
            PlayerEvent::Playing(playing) => {
                self.playing = playing;
                if playing {
                    self.ended = false;
                } else {
                    self.playhead.stop(self.position);
                }
            }
            PlayerEvent::Ended => {
                self.playing = false;
                self.ended = true;
                self.playhead.stop(self.position);
            }
            PlayerEvent::AudioError(message) => {
                self.notice = Some(format!("Audio preview: {message}").into());
            }
            PlayerEvent::Error(message) => {
                self.ready = false;
                self.error = Some(message);
                self.playing = false;
                self.playhead.stop(self.position);
            }
        }
        cx.notify();
    }

    /// A frame or position report from the player (the authoritative clock).
    fn set_reported_position(&mut self, position: Duration) {
        self.position = position;
        if self.playing {
            self.playhead.sync(position, Instant::now());
        } else {
            self.playhead.stop(position);
        }
    }

    /// Playhead position to draw now (see [`playhead::PlayheadClock`]).
    /// Records the drawn x for [`Self::queue_playhead_frame`].
    pub(super) fn playhead_position(&self) -> Duration {
        let position = self.playhead_at(false);
        if let Some(duration) = self.duration {
            self.playhead_drawn_x
                .set(self.playhead_x(position, duration));
        }
        position
    }

    fn playhead_x(&self, position: Duration, duration: Duration) -> f32 {
        f32::from(self.timeline.get().size.width) * fraction(position, duration)
    }

    fn playhead_at(&self, peek: bool) -> Duration {
        let Some(duration) = self.duration else {
            return self.position;
        };
        if !self.playing {
            return self.position.min(duration);
        }
        let limit = self
            .edits
            .as_ref()
            .and_then(|edits| {
                edits
                    .segments()
                    .iter()
                    .find(|s| self.position >= s.start && self.position < s.end)
                    .map(|s| s.end)
            })
            .unwrap_or(duration)
            .min(duration);
        if peek {
            self.playhead.peek(Instant::now(), limit)
        } else {
            self.playhead.display(Instant::now(), limit)
        }
    }

    /// While playing, checks once per display refresh whether the playhead
    /// moved far enough to be visible (half a pixel) and only then redraws.
    /// At low zoom the line moves a fraction of a pixel per refresh, so most
    /// refreshes cost nothing; at high zoom it redraws every refresh.
    pub(super) fn queue_playhead_frame(&self, window: &Window, cx: &mut Context<Self>) {
        if !self.playhead_animating() || self.playhead_frame_queued.replace(true) {
            return;
        }
        let editor = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| playhead_frame(editor, window, cx));
    }

    /// `Some(true)`: redraw now; `Some(false)`: check again next refresh;
    /// `None`: not playing any more.
    fn playhead_moved(&self) -> Option<bool> {
        if !self.playhead_animating() {
            return None;
        }
        let duration = self.duration?;
        let x = self.playhead_x(self.playhead_at(true), duration);
        Some((x - self.playhead_drawn_x.get()).abs() >= PLAYHEAD_MIN_STEP_PX)
    }

    /// While playing, the playhead moves every display refresh.
    pub(super) fn playhead_animating(&self) -> bool {
        self.playing && self.ready && self.playhead.is_running()
    }

    fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        if self.edits.as_ref().is_none_or(|e| e.segments().is_empty()) {
            return;
        }
        let Some(player) = self.player.as_ref().filter(|_| self.ready) else {
            return;
        };
        if self.playing {
            self.pending_seek = None;
            player.pause();
            self.playhead.stop(self.position);
        } else {
            if let Some(edits) = &self.edits {
                let start = if self.ended {
                    edits.segments().first().map(|s| s.start)
                } else if edits
                    .segments()
                    .iter()
                    .any(|s| self.position >= s.start && self.position < s.end)
                {
                    None
                } else {
                    edits
                        .segments()
                        .iter()
                        .find(|s| s.start > self.position)
                        .or_else(|| edits.segments().first())
                        .map(|s| s.start)
                };
                if let Some(start) = start {
                    self.pending_seek = Some(start);
                    player.seek(start);
                    self.position = start;
                    self.playhead.stop(start);
                }
            }
            player.play();
        }
        // Optimistic; the player confirms with `Playing`.
        self.playing = !self.playing;
        cx.notify();
    }

    fn seek(&mut self, position: Duration, cx: &mut Context<Self>) {
        let (Some(player), Some(duration)) =
            (self.player.as_ref().filter(|_| self.ready), self.duration)
        else {
            return;
        };
        let position = position.min(duration);
        self.pending_seek = self.playing.then_some(position);
        player.seek(position);
        self.position = position;
        if self.playing {
            self.playhead.jump(position, Instant::now());
        } else {
            self.playhead.stop(position);
        }
        self.ended = false;
        cx.notify();
    }

    fn seek_to_x(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(duration) = self.duration else {
            return;
        };
        let bounds = self.timeline.get();
        let fraction = position_fraction(x, bounds);
        self.seek(duration.mul_f32(fraction), cx);
    }

    fn set_in(&mut self, cx: &mut Context<Self>) {
        let position = self.position;
        self.edit_trim(cx, |trim, _| trim.set_in(position));
    }

    fn set_out(&mut self, cx: &mut Context<Self>) {
        let position = self.position;
        self.edit_trim(cx, |trim, duration| trim.set_out(position, duration));
    }

    fn reset_trim(&mut self, cx: &mut Context<Self>) {
        if let Some(edits) = &mut self.edits
            && let Some(i) = edits.selected()
        {
            let start = i
                .checked_sub(1)
                .map_or(Duration::ZERO, |i| edits.segments()[i].end);
            let end = edits
                .segments()
                .get(i + 1)
                .map_or(edits.source_duration(), |s| s.start);
            self.notice = edits.trim(start, end).err().map(Into::into);
        }
        self.sync_trim(cx);
    }

    fn edit_trim(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Trim, Duration) -> Result<(), &'static str>,
    ) {
        let (Some(trim), Some(duration)) = (self.trim.as_mut(), self.duration) else {
            return;
        };
        let mut candidate = *trim;
        let result = edit(&mut candidate, duration).and_then(|()| {
            self.edits
                .as_mut()
                .ok_or("Clip is loading.")?
                .trim(candidate.start, candidate.end)
        });
        self.notice = result.err().map(SharedString::from);
        self.sync_trim(cx);
    }

    fn export_to(&mut self, destination: PathBuf, cx: &mut Context<Self>) {
        if matches!(self.export, Export::Running { .. }) {
            return;
        }
        let ranges = self.edits.as_ref().map(|e| e.ranges()).unwrap_or_default();
        if ranges.is_empty() {
            return;
        }
        if !destination
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
        {
            self.notice = Some("Choose an .mp4 output file.".into());
            cx.notify();
            return;
        }
        let audio_tracks = self
            .edits
            .as_ref()
            .map(|e| e.audio_tracks().to_vec())
            .unwrap_or_default();
        let source = self.path.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = async_channel::bounded(4);
        let thread_cancel = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("clip-export".into())
            .spawn(move || {
                let progress_sender = sender.clone();
                let mut progress = |value: f32| {
                    // Dropped when the UI is behind; the next update catches up.
                    let _ = progress_sender.try_send(ExportMessage::Progress(value));
                };
                let result = clipper_media::exact::export_audio(
                    &source,
                    &destination,
                    &ranges,
                    &audio_tracks,
                    &mut progress,
                    &thread_cancel,
                )
                .map_err(|error| match error {
                    trim::TrimError::Cancelled => CANCELLED.to_owned(),
                    error => error.to_string(),
                });
                let _ = sender.send_blocking(ExportMessage::Finished(result));
            });
        if let Err(error) = spawned {
            clipper_diagnostics::log::error!("editor export could not start: {error}");
            self.export = Export::Failed(format!("Could not start the export: {error}"));
            cx.notify();
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            while let Ok(message) = receiver.recv().await {
                let done = matches!(message, ExportMessage::Finished(_));
                let updated = this.update(cx, |this, cx| {
                    match message {
                        ExportMessage::Progress(value) => {
                            if let Export::Running { progress, .. } = &mut this.export {
                                *progress = value;
                            }
                        }
                        ExportMessage::Finished(Ok(outcome)) => {
                            clipper_diagnostics::log::info!("editor export completed");
                            this.export = Export::Done(outcome);
                            cx.emit(EditorEvent::Exported);
                        }
                        ExportMessage::Finished(Err(message)) if message == CANCELLED => {
                            this.export = Export::Idle
                        }
                        ExportMessage::Finished(Err(message)) => {
                            clipper_diagnostics::log::error!("editor export failed: {message}");
                            this.export = Export::Failed(message)
                        }
                    }
                    cx.notify();
                });
                if done || updated.is_err() {
                    break;
                }
            }
        });
        self.export = Export::Running {
            progress: 0.0,
            cancel,
            _task: task,
        };
        cx.notify();
    }

    /// Shows a failed shell action (e.g. the export was moved) under the editor.
    fn report(&mut self, result: std::io::Result<()>, cx: &mut Context<Self>) {
        if let Err(error) = result {
            self.notice = Some(error.to_string().into());
            cx.notify();
        }
    }

    fn cancel_export(&mut self, cx: &mut Context<Self>) {
        if let Export::Running { cancel, .. } = &self.export {
            cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }
}

impl Drop for ClipEditor {
    fn drop(&mut self) {
        // A running export finishes or notices the closed channel; cancel it instead.
        if let Export::Running { cancel, .. } = &self.export {
            cancel.store(true, Ordering::Relaxed);
        }
        release(self.player.take());
    }
}

/// Shutting the media engine down takes ~200 ms; keep that off the UI thread.
fn release(player: Option<Player>) {
    if let Some(player) = player {
        let _ = std::thread::Builder::new()
            .name("clip-player-release".into())
            .spawn(move || drop(player));
    }
}

/// Wraps a decoded BGRA frame for GPUI (which stores image data as BGRA).
fn render_image(frame: Arc<PlayerFrame>) -> Option<Arc<RenderImage>> {
    let (width, height) = (frame.width, frame.height);
    let bytes = Arc::try_unwrap(frame)
        .map(|frame| frame.bgra)
        .unwrap_or_else(|frame| frame.bgra.clone());
    let buffer = image::RgbaImage::from_raw(width, height, bytes)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

#[derive(Debug, PartialEq, Eq)]
enum PlaybackStep {
    Continue,
    Seek(Duration),
    End(Duration),
}

/// Uses source timestamps from the player; removed source spans never create a second clock.
fn playback_step(edits: &EditSession, at: Duration) -> PlaybackStep {
    if edits.segments().iter().any(|s| at >= s.start && at < s.end) {
        PlaybackStep::Continue
    } else if let Some(next) = edits.segments().iter().find(|s| s.start > at) {
        PlaybackStep::Seek(next.start)
    } else {
        PlaybackStep::End(edits.segments().last().map_or(Duration::ZERO, |s| s.end))
    }
}

fn position_fraction(x: Pixels, bounds: Bounds<Pixels>) -> f32 {
    let width = f32::from(bounds.size.width);
    if width <= 0.0 {
        return 0.0;
    }
    (f32::from(x - bounds.origin.x) / width).clamp(0.0, 1.0)
}

/// Smallest playhead movement worth a redraw, in logical pixels.
const PLAYHEAD_MIN_STEP_PX: f32 = 0.5;

fn playhead_frame(editor: gpui::WeakEntity<ClipEditor>, window: &mut Window, cx: &mut gpui::App) {
    let Some(editor) = editor.upgrade() else {
        return;
    };
    let moved = editor.update(cx, |this, _| {
        this.playhead_frame_queued.set(false);
        this.playhead_moved()
    });
    match moved {
        // Rendering queues the next check.
        Some(true) => editor.update(cx, |_, cx| cx.notify()),
        Some(false) => editor.update(cx, |this, cx| this.queue_playhead_frame(window, cx)),
        None => {}
    }
}

fn fraction(at: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        0.0
    } else {
        (at.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
    }
}

fn muted(t: &Theme) -> Div {
    div().text_xs().text_color(t.colors.text_muted)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The Editor page when no clip is open.
pub(super) fn empty(t: &Theme) -> Div {
    panel(t)
        .w_full()
        .max_w(px(440.))
        .mx_auto()
        .mt_8()
        .p_6()
        .flex()
        .flex_col()
        .items_center()
        .gap_4()
        .child(empty_state(
            t,
            "◫",
            "No clip open",
            "Open the Clips page and choose Edit on a saved clip to play, trim and export it.",
        ))
}

mod audio;
mod playhead;
mod presentation;
impl ClipEditor {
    fn export_status(&self, cx: &mut Context<Self>) -> Option<Div> {
        let t = *cx.theme();
        let line = div()
            .flex()
            .items_center()
            .gap_3()
            .px_1()
            .text_xs()
            .text_color(t.colors.text_secondary);
        match &self.export {
            Export::Idle => Some(
                line.child(
                    muted(&t).child("Choose an MP4 destination. Exact cuts preserve the original."),
                )
                .child(div().flex_1())
                .child(shortcut(&t, &["Space"]))
                .child(muted(&t).child("Play"))
                .child(kbd(&t, "I"))
                .child(muted(&t).child("In"))
                .child(kbd(&t, "O"))
                .child(muted(&t).child("Out")),
            ),
            Export::Running { progress, .. } => Some(
                line.child(format!("Exporting… {:.0}%", progress * 100.0))
                    .child(
                        div()
                            .flex_1()
                            .h(px(4.))
                            .rounded_full()
                            .bg(t.colors.surface_raised)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(progress.clamp(0.0, 1.0)))
                                    .rounded_full()
                                    .bg(t.colors.accent),
                            ),
                    )
                    .child(
                        button(
                            &t,
                            "editor-export-cancel",
                            "Cancel",
                            ButtonVariant::Ghost,
                            None,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_export(cx))),
                    ),
            ),
            Export::Done(outcome) => {
                let open = outcome.path.clone();
                let reveal = outcome.path.clone();
                Some(
                    line.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(t.colors.success)
                            .child(format!(
                                "Exported {} ({})",
                                file_name(&outcome.path),
                                format_timestamp(outcome.duration),
                                // Previous behavior: Stream copy starts on a keyframe; say so when it moved.
                                // Exact export now reports length without keyframe diagnostics.
                            )),
                    )
                    .child(
                        button(
                            &t,
                            "editor-export-open",
                            "Open",
                            ButtonVariant::Secondary,
                            None,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.report(shell::open_path(&open), cx)
                        })),
                    )
                    .child(
                        button(
                            &t,
                            "editor-export-folder",
                            "Open Folder",
                            ButtonVariant::Ghost,
                            None,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.report(shell::reveal_in_folder(&reveal), cx)
                        })),
                    ),
                )
            }
            Export::Failed(message) => Some(
                line.child(
                    div()
                        .flex_1()
                        .text_color(t.colors.danger)
                        .child(format!("Export failed: {message}")),
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f32) -> Duration {
        Duration::from_secs_f32(s)
    }

    #[test]
    fn trim_rejects_crossed_points() {
        let duration = secs(60.0);
        let mut trim = Trim::full(duration);
        trim.set_in(secs(12.0)).unwrap();
        trim.set_out(secs(45.0), duration).unwrap();
        assert_eq!(
            trim,
            Trim {
                start: secs(12.0),
                end: secs(45.0)
            }
        );
        assert!(trim.set_in(secs(50.0)).is_err());
        assert!(trim.set_out(secs(10.0), duration).is_err());
        assert!(trim.set_in(secs(44.95)).is_err(), "selection under 100 ms");
        assert_eq!(
            trim,
            Trim {
                start: secs(12.0),
                end: secs(45.0)
            }
        );
    }

    #[test]
    fn out_point_is_clamped_to_duration() {
        let duration = secs(30.0);
        let mut trim = Trim::full(duration);
        trim.set_out(secs(99.0), duration).unwrap();
        assert_eq!(trim.end, duration);
        assert!(trim.is_full(duration));
    }

    #[test]
    fn timeline_maps_clicks_and_positions() {
        let bounds = Bounds::new(gpui::point(px(100.), px(0.)), gpui::size(px(200.), px(36.)));
        assert_eq!(position_fraction(px(100.), bounds), 0.0);
        assert_eq!(position_fraction(px(200.), bounds), 0.5);
        assert_eq!(position_fraction(px(400.), bounds), 1.0);
        assert_eq!(position_fraction(px(50.), bounds), 0.0);
        assert_eq!(fraction(secs(15.0), secs(60.0)), 0.25);
        assert_eq!(fraction(secs(1.0), Duration::ZERO), 0.0);
    }
}

#[cfg(test)]
mod playback_tests {
    use super::*;
    #[test]
    fn source_timeline_skips_deleted_ranges_and_stops_at_edited_end() {
        let mut edits = EditSession::new(Duration::from_secs(10));
        edits.split(Duration::from_secs(4)).unwrap();
        edits.select(0);
        edits.delete_selected();
        assert_eq!(
            playback_step(&edits, Duration::from_secs(2)),
            PlaybackStep::Seek(Duration::from_secs(4))
        );
        assert_eq!(
            playback_step(&edits, Duration::from_secs(5)),
            PlaybackStep::Continue
        );
        edits
            .trim(Duration::from_secs(6), Duration::from_secs(8))
            .unwrap();
        assert_eq!(
            playback_step(&edits, Duration::from_secs(5)),
            PlaybackStep::Seek(Duration::from_secs(6))
        );
        assert_eq!(
            playback_step(&edits, Duration::from_secs(8)),
            PlaybackStep::End(Duration::from_secs(8))
        );
        edits.delete_selected();
        assert_eq!(
            playback_step(&edits, Duration::from_secs(2)),
            PlaybackStep::End(Duration::ZERO)
        );
    }
    #[test]
    fn scrolled_zoom_bounds_share_source_time_mapping() {
        let bounds = Bounds::new(
            gpui::point(px(-100.), px(0.)),
            gpui::size(px(1000.), px(52.)),
        );
        assert_eq!(position_fraction(px(150.), bounds), 0.25);
        assert_eq!(position_fraction(px(900.), bounds), 1.);
    }
}
