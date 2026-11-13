//! Clip editor: preview playback, in/out trim points and stream-copy export.
//! The source file is only ever read; exports go to a new file next to it.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clipper_media::player::{Player, PlayerEvent, PlayerFrame};
use clipper_media::trim::{self, TrimOutcome};
use clipper_platform::windows::shell;
use gpui::prelude::*;
use gpui::{
    Bounds, Context, Div, FocusHandle, FontWeight, MouseButton, ObjectFit, Pixels, RenderImage,
    SharedString, Task, Window, actions, canvas, div, img, px, relative, rgb, rgba,
};

use crate::components::{ButtonVariant, button, empty_state, kbd, panel, shortcut};
use crate::state::format_elapsed;
use crate::theme;

actions!(clip_editor, [TogglePlayback, SetIn, SetOut]);

pub(super) const CONTEXT: &str = "ClipEditor";
/// Preview frames are decoded at most this wide; the preview never shows more.
const PREVIEW_MAX_WIDTH: u32 = 1280;
const PREVIEW_HEIGHT: f32 = 360.0;
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
    position: Duration,
    playing: bool,
    ended: bool,
    error: Option<String>,
    trim: Option<Trim>,
    notice: Option<SharedString>,
    export: Export,
    timeline: Rc<Cell<Bounds<Pixels>>>,
    scrubbing: bool,
}

impl ClipEditor {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let mut this = Self {
            path,
            focus,
            player: None,
            _player_events: None,
            frame: None,
            duration: None,
            ready: false,
            position: Duration::ZERO,
            playing: false,
            ended: false,
            error: None,
            trim: None,
            notice: None,
            export: Export::Idle,
            timeline: Rc::default(),
            scrubbing: false,
        };
        this.resume(window, cx);
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
        self._player_events = None;
        release(self.player.take());
        self.ready = false;
        self.playing = false;
        self.release_images(window);
        cx.notify();
    }

    /// Frees the preview image from the GPU atlas; call before dropping the editor.
    pub fn release_images(&mut self, window: &mut Window) {
        if let Some(image) = self.frame.take() {
            let _ = window.drop_image(image);
        }
    }

    fn on_player(&mut self, event: PlayerEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PlayerEvent::Loaded { duration, .. } => {
                self.ready = true;
                self.duration = Some(duration);
                let trim = self.trim.get_or_insert(Trim::full(duration));
                trim.end = trim.end.min(duration);
                // The player ignores commands until it has loaded; return to where we were.
                if let Some(player) = &self.player
                    && self.position > Duration::ZERO
                {
                    self.position = self.position.min(duration);
                    player.seek(self.position);
                }
            }
            PlayerEvent::Frame(frame) => {
                self.position = frame.position;
                if let Some(image) = render_image(frame)
                    && let Some(old) = self.frame.replace(image)
                {
                    let _ = window.drop_image(old);
                }
            }
            PlayerEvent::Position(position) => self.position = position,
            PlayerEvent::Playing(playing) => {
                self.playing = playing;
                if playing {
                    self.ended = false;
                }
            }
            PlayerEvent::Ended => {
                self.playing = false;
                self.ended = true;
            }
            PlayerEvent::Error(message) => {
                self.ready = false;
                self.error = Some(message);
                self.playing = false;
            }
        }
        cx.notify();
    }

    fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        let Some(player) = self.player.as_ref().filter(|_| self.ready) else {
            return;
        };
        if self.playing {
            player.pause();
        } else {
            if self.ended {
                player.seek(Duration::ZERO);
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
        player.seek(position);
        self.position = position;
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
        self.edit_trim(cx, |trim, duration| {
            *trim = Trim::full(duration);
            Ok(())
        });
    }

    fn edit_trim(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Trim, Duration) -> Result<(), &'static str>,
    ) {
        let (Some(trim), Some(duration)) = (self.trim.as_mut(), self.duration) else {
            return;
        };
        self.notice = edit(trim, duration).err().map(SharedString::from);
        cx.notify();
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        if matches!(self.export, Export::Running { .. }) {
            return;
        }
        let Some(trim) = self.trim else { return };
        let source = self.path.clone();
        let destination = trim::export_path(&source, trim.start, trim.end);
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
                let result = trim::trim(
                    &source,
                    &destination,
                    trim.start,
                    trim.end,
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
                        ExportMessage::Finished(Ok(outcome)) => this.export = Export::Done(outcome),
                        ExportMessage::Finished(Err(message)) if message == CANCELLED => {
                            this.export = Export::Idle
                        }
                        ExportMessage::Finished(Err(message)) => {
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

fn position_fraction(x: Pixels, bounds: Bounds<Pixels>) -> f32 {
    let width = f32::from(bounds.size.width);
    if width <= 0.0 {
        return 0.0;
    }
    (f32::from(x - bounds.origin.x) / width).clamp(0.0, 1.0)
}

fn fraction(at: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        0.0
    } else {
        (at.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
    }
}

fn muted() -> Div {
    div().text_xs().text_color(rgb(theme::TEXT_TERTIARY))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The Editor page when no clip is open.
pub(super) fn empty() -> Div {
    panel().child(empty_state(
        "◫",
        "No clip open",
        "Open the Clips page and choose Edit on a saved clip to play, trim and export it.",
    ))
}

impl Render for ClipEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loaded = self.ready;
        let duration = self.duration.unwrap_or_default();
        let exporting = matches!(self.export, Export::Running { .. });

        let header = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(file_name(&self.path)),
            )
            .child(
                button(
                    "editor-export",
                    "Export Selection",
                    ButtonVariant::Primary,
                    None,
                    loaded && !exporting,
                )
                .when(loaded && !exporting, |this| {
                    this.on_click(cx.listener(|this, _, _, cx| this.export(cx)))
                }),
            );

        let preview = div()
            .id("editor-preview")
            .relative()
            .w_full()
            .h(px(PREVIEW_HEIGHT))
            .rounded_lg()
            .overflow_hidden()
            .bg(rgb(0x000000))
            .flex()
            .items_center()
            .justify_center()
            .when(loaded, |this| {
                this.cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx)))
            })
            .map(|this| match (&self.error, &self.frame) {
                (Some(error), _) => this.child(
                    div()
                        .max_w(px(420.))
                        .text_sm()
                        .text_center()
                        .text_color(rgb(theme::DANGER))
                        .child(error.clone()),
                ),
                (None, Some(frame)) => this.child(
                    img(frame.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                ),
                (None, None) => this.child(muted().child("Loading clip…")),
            });

        let play_label = if self.playing { "Pause" } else { "Play" };
        let controls = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                button(
                    "editor-play",
                    play_label,
                    ButtonVariant::Secondary,
                    Some(&["Space"]),
                    loaded,
                )
                .when(loaded, |this| {
                    this.on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx)))
                }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(format!(
                        "{} / {}",
                        format_elapsed(self.position),
                        if loaded {
                            format_elapsed(duration)
                        } else {
                            "--:--".into()
                        }
                    )),
            );

        let timeline = self.timeline(duration, loaded, cx);

        let trim = self.trim.unwrap_or(Trim::full(duration));
        let value = |label: &'static str, text: String| {
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .child(muted().child(label))
                .child(div().text_sm().child(text))
        };
        let trim_row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_6()
            .gap_y_3()
            .child(value("Trim start", format_elapsed(trim.start)))
            .child(value("Trim end", format_elapsed(trim.end)))
            .child(value("Length", format_elapsed(trim.end - trim.start)))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "editor-in",
                            "Set In",
                            ButtonVariant::Secondary,
                            Some(&["I"]),
                            loaded,
                        )
                        .when(loaded, |this| {
                            this.on_click(cx.listener(|this, _, _, cx| this.set_in(cx)))
                        }),
                    )
                    .child(
                        button(
                            "editor-out",
                            "Set Out",
                            ButtonVariant::Secondary,
                            Some(&["O"]),
                            loaded,
                        )
                        .when(loaded, |this| {
                            this.on_click(cx.listener(|this, _, _, cx| this.set_out(cx)))
                        }),
                    )
                    .child({
                        let enabled = loaded && !trim.is_full(duration);
                        button(
                            "editor-reset",
                            "Reset Trim",
                            ButtonVariant::Ghost,
                            None,
                            enabled,
                        )
                        .when(enabled, |this| {
                            this.on_click(cx.listener(|this, _, _, cx| this.reset_trim(cx)))
                        })
                    }),
            );

        div()
            .id("clip-editor")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &TogglePlayback, _, cx| this.toggle_playback(cx)))
            .on_action(cx.listener(|this, _: &SetIn, _, cx| this.set_in(cx)))
            .on_action(cx.listener(|this, _: &SetOut, _, cx| this.set_out(cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| window.focus(&this.focus)),
            )
            .flex()
            .flex_col()
            .gap_4()
            .child(header)
            .child(
                panel()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(preview)
                    .child(controls),
            )
            .child(
                panel()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(timeline)
                    .child(trim_row),
            )
            .children(self.notice.clone().map(|notice| {
                div()
                    .px_1()
                    .text_xs()
                    .text_color(rgb(theme::WARNING))
                    .child(notice)
            }))
            .children(self.export_status(cx))
    }
}

impl ClipEditor {
    fn timeline(&self, duration: Duration, loaded: bool, cx: &mut Context<Self>) -> Div {
        let trim = self.trim.unwrap_or(Trim::full(duration));
        let (start, end) = (fraction(trim.start, duration), fraction(trim.end, duration));
        let head = fraction(self.position, duration);
        let bounds_out = self.timeline.clone();
        let marker = |at: f32, label: &'static str| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(at))
                .w(px(2.))
                .bg(rgb(theme::ACCENT))
                .child(
                    div()
                        .absolute()
                        .bottom(px(-16.))
                        .text_size(px(10.))
                        .text_color(rgb(theme::TEXT_TERTIARY))
                        .child(label),
                )
        };
        let track = div()
            .id("editor-timeline")
            .relative()
            .flex_1()
            .h(px(36.))
            .mb_4()
            .rounded_md()
            .bg(rgb(theme::SURFACE_RAISED))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .child(
                canvas(move |bounds, _, _| bounds_out.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(start))
                    .w(relative(end - start))
                    .bg(rgba(theme::ACCENT_SUBTLE)),
            )
            .child(marker(start, "IN"))
            .child(marker(end, "OUT"))
            .child(
                div()
                    .absolute()
                    .top(px(-3.))
                    .bottom(px(-3.))
                    .left(relative(head))
                    .w(px(2.))
                    .rounded_full()
                    .bg(rgb(theme::TEXT)),
            )
            .when(loaded, |this| {
                this.cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                            this.scrubbing = true;
                            this.seek_to_x(event.position.x, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                        if this.scrubbing && event.pressed_button == Some(MouseButton::Left) {
                            this.seek_to_x(event.position.x, cx);
                        } else {
                            this.scrubbing = false;
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.scrubbing = false),
                    )
            });

        div()
            .flex()
            .items_start()
            .gap_3()
            .child(muted().pt_2().child(format_elapsed(self.position)))
            .child(track)
            .child(muted().pt_2().child(if loaded {
                format_elapsed(duration)
            } else {
                "--:--".into()
            }))
    }

    fn export_status(&self, cx: &mut Context<Self>) -> Option<Div> {
        let line = div()
            .flex()
            .items_center()
            .gap_3()
            .px_1()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY));
        match &self.export {
            Export::Idle => Some(
                line.child(muted().child(
                    "Exports a new MP4 next to the original. The original stays unchanged.",
                ))
                .child(div().flex_1())
                .child(shortcut(&["Space"]))
                .child(muted().child("Play"))
                .child(kbd("I"))
                .child(muted().child("In"))
                .child(kbd("O"))
                .child(muted().child("Out")),
            ),
            Export::Running { progress, .. } => Some(
                line.child(format!("Exporting… {:.0}%", progress * 100.0))
                    .child(
                        div()
                            .flex_1()
                            .h(px(4.))
                            .rounded_full()
                            .bg(rgb(theme::SURFACE_RAISED))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(progress.clamp(0.0, 1.0)))
                                    .rounded_full()
                                    .bg(rgb(theme::ACCENT)),
                            ),
                    )
                    .child(
                        button(
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
                            .text_color(rgb(theme::SUCCESS))
                            .child(format!(
                                "Exported {} ({}){}",
                                file_name(&outcome.path),
                                format_elapsed(outcome.duration),
                                // Stream copy starts on a keyframe; say so when it moved.
                                match self.trim {
                                    Some(trim) if outcome.start + MIN_SELECTION < trim.start => {
                                        format!(
                                            " · starts at {}, the nearest keyframe",
                                            format_elapsed(outcome.start)
                                        )
                                    }
                                    _ => String::new(),
                                }
                            )),
                    )
                    .child(
                        button(
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
                        .text_color(rgb(theme::DANGER))
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
