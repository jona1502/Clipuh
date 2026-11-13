use clipper_core::hotkey::{Hotkey, HotkeyError, Key, Modifiers};
use clipper_core::recording::{QualityProfile, REPLAY_DURATIONS_SECONDS, RecorderCommand};
use clipper_platform::windows::hotkey::RegisterError;
use clipper_storage::{
    NOTIFICATION_DURATION_SECONDS, NotificationSettings,
    config::{EncoderPreference, ReplayMode, Theme, VideoCodec},
    config_path, default_replay_cache_directory,
};
use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Div, FocusHandle, KeyDownEvent, ModifiersChangedEvent, SharedString,
    Subscription, Window, div, px, rgb, rgba,
};

use super::{ClipperWindow, HOTKEY_CONTEXT};
use crate::components::{
    ButtonVariant, button, chip, kbd, panel, property_row, section_header, switch, value_text,
};
use crate::state::ReplayPhase;
use crate::theme;

/// RAM budgets offered for the replay buffer, in MiB.
const REPLAY_MEMORY_PRESETS_MB: [u32; 5] = [32, 64, 128, 256, 512];
/// Disk cache limits offered for the replay buffer, in MiB.
const REPLAY_DISK_PRESETS_MB: [u32; 6] = [1024, 2048, 4096, 8192, 16384, 32768];

fn size_label(megabytes: u32) -> String {
    if megabytes >= 1024 && megabytes.is_multiple_of(1024) {
        format!("{} GB", megabytes / 1024)
    } else {
        format!("{megabytes} MB")
    }
}

fn duration_label(seconds: u32) -> String {
    if seconds >= 60 && seconds.is_multiple_of(60) {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} s")
    }
}

/// Recording mode for the save-replay shortcut.
pub(super) struct HotkeyRecorder {
    focus: FocusHandle,
    /// Modifiers currently held, for the live preview.
    held: Modifiers,
    error: Option<String>,
    /// A registration request is in flight; input is ignored until it answers.
    pending: bool,
    _blur: Subscription,
}

/// Maps a gpui keystroke to a hotkey. `None` for modifier-only keystrokes.
fn hotkey_from_keystroke(key: &str, modifiers: Modifiers) -> Option<Result<Hotkey, HotkeyError>> {
    if matches!(
        key,
        "control" | "shift" | "alt" | "platform" | "super" | "win" | "function"
    ) {
        return None;
    }
    Some(match Key::from_name(key) {
        Some(key) => Hotkey::new(modifiers, key),
        None => Err(HotkeyError::UnknownKey(key.to_owned())),
    })
}

fn core_modifiers(modifiers: gpui::Modifiers) -> Modifiers {
    Modifiers {
        ctrl: modifiers.control,
        alt: modifiers.alt,
        shift: modifiers.shift,
        win: modifiers.platform,
    }
}

fn keycaps(text: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .children(text.split('+').map(|key| kbd(key.to_owned())))
}

fn modifier_names(modifiers: Modifiers) -> String {
    [
        (modifiers.ctrl, "Ctrl"),
        (modifiers.alt, "Alt"),
        (modifiers.shift, "Shift"),
        (modifiers.win, "Win"),
    ]
    .into_iter()
    .filter_map(|(held, name)| held.then_some(name))
    .collect::<Vec<_>>()
    .join("+")
}

impl ClipperWindow {
    /// Registers the configured hotkey; a failure is shown in Settings and the banner.
    pub(super) fn register_hotkey(&mut self, cx: &mut Context<Self>) {
        let hotkey = self.hotkey;
        let reply = self.hotkeys.set(Some(hotkey));
        cx.spawn(async move |this, cx| {
            let result = reply.recv().await;
            this.update(cx, |this, cx| {
                this.hotkey_error = None;
                let failure = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(_) => Some(RegisterError::Stopped),
                };
                if let Some(error) = failure {
                    this.hotkey_error = Some(error.to_string());
                    this.recording
                        .fail(format!("Global hotkey {hotkey} is unavailable: {error}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn begin_hotkey_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.hotkey_recorder.is_some() {
            return;
        }
        let focus = cx.focus_handle();
        window.focus(&focus);
        let blur = cx.on_blur(&focus, window, |this, _, cx| {
            this.leave_hotkey_recording(cx)
        });
        self.hotkey_recorder = Some(HotkeyRecorder {
            focus,
            held: Modifiers::default(),
            error: None,
            pending: true,
            _blur: blur,
        });
        // Release the global combo so pressing it reaches the app, not Windows.
        let reply = self.hotkeys.set(None);
        cx.spawn_in(window, async move |this, cx| {
            let _ = reply.recv().await;
            this.update_in(cx, |this, window, cx| {
                this.finish_hotkey_request(None, window, cx)
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Leaves recording mode and re-registers the configured hotkey.
    fn leave_hotkey_recording(&mut self, cx: &mut Context<Self>) {
        if self.hotkey_recorder.as_ref().is_none_or(|r| r.pending) {
            return;
        }
        self.hotkey_recorder = None;
        self.register_hotkey(cx);
        cx.notify();
    }

    fn cancel_hotkey_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_hotkey_recording(cx);
        window.focus(&self.focus);
    }

    fn apply_hotkey(&mut self, hotkey: Hotkey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(recorder) = &mut self.hotkey_recorder else {
            return;
        };
        recorder.pending = true;
        recorder.error = None;
        let reply = self.hotkeys.set(Some(hotkey));
        cx.spawn_in(window, async move |this, cx| {
            let result = reply.recv().await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(Ok(())) => {
                    this.hotkey = hotkey;
                    this.hotkey_error = None;
                    this.config.replay.hotkey = hotkey.to_string();
                    this.hotkey_recorder = None;
                    window.focus(&this.focus);
                    this.persist_config(cx);
                }
                failure => {
                    // The old combo stays released until the user leaves recording mode.
                    let message = match failure {
                        Ok(Err(error)) => error.to_string(),
                        _ => "The hotkey service is not running.".to_owned(),
                    };
                    this.finish_hotkey_request(Some(message), window, cx);
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Ends a pending registration request. Blur is ignored while a request is
    /// pending, so a recorder that lost focus meanwhile is closed here; otherwise
    /// it would stay open with no hotkey registered.
    fn finish_hotkey_request(
        &mut self,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(recorder) = &mut self.hotkey_recorder else {
            return;
        };
        recorder.pending = false;
        recorder.error = error;
        if !recorder.focus.is_focused(window) {
            self.leave_hotkey_recording(cx);
        }
        cx.notify();
    }

    fn hotkey_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(recorder) = &mut self.hotkey_recorder else {
            return;
        };
        cx.stop_propagation();
        let keystroke = &event.keystroke;
        let modifiers = core_modifiers(keystroke.modifiers);
        if recorder.pending {
            return;
        }
        if keystroke.key == "escape" && modifiers.is_empty() {
            self.cancel_hotkey_recording(window, cx);
            return;
        }
        match hotkey_from_keystroke(&keystroke.key, modifiers) {
            None => {}
            Some(Err(error)) => recorder.error = Some(error.to_string()),
            Some(Ok(hotkey)) => self.apply_hotkey(hotkey, window, cx),
        }
        cx.notify();
    }

    fn hotkey_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let error = |text: String| {
            div()
                .text_xs()
                .text_color(rgb(theme::RECORDING))
                .child(text)
        };
        let column = || div().flex().flex_col().items_end().gap_1();
        let row = || div().flex().items_center().gap_3();

        let Some(recorder) = &self.hotkey_recorder else {
            let change = button(
                "hotkey-change",
                "Change",
                ButtonVariant::Secondary,
                None,
                true,
            )
            .on_click(cx.listener(|this, _, window, cx| this.begin_hotkey_recording(window, cx)));
            return column()
                .child(row().child(keycaps(&self.hotkey.to_string())).child(change))
                .children(self.hotkey_error.clone().map(error))
                .into_any_element();
        };

        let prompt = if recorder.held.is_empty() {
            div()
                .text_sm()
                .text_color(rgb(theme::TEXT_TERTIARY))
                .child("Press your shortcut…")
        } else {
            keycaps(&modifier_names(recorder.held))
                .child(div().text_color(rgb(theme::TEXT_TERTIARY)).child("+ …"))
        };
        let enabled = !recorder.pending;
        let reset = button(
            "hotkey-reset",
            "Reset to Default",
            ButtonVariant::Ghost,
            None,
            enabled,
        )
        .when(enabled, |this| {
            this.on_click(cx.listener(|this, _, window, cx| {
                this.apply_hotkey(Hotkey::default_save_replay(), window, cx)
            }))
        });
        let cancel = button(
            "hotkey-cancel",
            "Cancel",
            ButtonVariant::Secondary,
            None,
            enabled,
        )
        .when(enabled, |this| {
            this.on_click(
                cx.listener(|this, _, window, cx| this.cancel_hotkey_recording(window, cx)),
            )
        });
        let capture = div()
            .id("hotkey-recorder")
            .key_context(HOTKEY_CONTEXT)
            .track_focus(&recorder.focus)
            .on_key_down(cx.listener(Self::hotkey_key_down))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                if let Some(recorder) = &mut this.hotkey_recorder {
                    recorder.held = core_modifiers(event.modifiers);
                    cx.notify();
                }
            }))
            .h(px(30.))
            .min_w(px(180.))
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(rgba(theme::FOCUS_RING))
            .bg(rgb(theme::SURFACE_RAISED))
            .child(prompt);
        column()
            .child(row().child(capture).child(reset).child(cancel))
            .children(recorder.error.clone().map(error))
            .into_any_element()
    }
}

/// Clickable switch; when `enabled` is false it is dimmed and inert.
fn notification_switch(
    id: &'static str,
    on: bool,
    enabled: bool,
    field: fn(&mut NotificationSettings) -> &mut bool,
    cx: &mut Context<ClipperWindow>,
) -> AnyElement {
    div()
        .id(id)
        .when(!enabled, |this| this.opacity(0.45))
        .when(enabled, |this| {
            this.cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    let value = field(&mut this.config.notifications);
                    *value = !*value;
                    this.persist_config(cx);
                }))
        })
        .child(switch(on))
        .into_any_element()
}

type Row = (&'static str, Option<&'static str>, AnyElement);

/// One group: header plus a single panel of rows; the first row has no divider.
fn group(title: &'static str, rows: Vec<Row>) -> Div {
    let mut body = panel();
    for (i, (label, description, value)) in rows.into_iter().enumerate() {
        let description = description.map(SharedString::from);
        body = body.child(property_row(label, description, value, i == 0));
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(section_header(title, None))
        .child(body)
}

fn text(value: impl Into<SharedString>) -> AnyElement {
    value_text(value).into_any_element()
}

fn path_text(path: String) -> AnyElement {
    div()
        .max_w(px(420.))
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(value_text(path))
        .into_any_element()
}

fn toggle(on: bool) -> AnyElement {
    switch(on).into_any_element()
}

pub(super) fn content(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let config = &window.config;
    let video = &config.video;
    let replay = &config.replay;

    let app = &config.app;
    let perf = &config.performance;
    let notifications = &config.notifications;
    let notify_on = notifications.enabled;

    let codec = match video.codec {
        VideoCodec::H264 => "H.264",
    };
    let encoder = match video.encoder {
        EncoderPreference::Auto => "Auto",
    };
    let mode = match replay.mode {
        ReplayMode::Hybrid => "Hybrid",
    };
    let theme_name = match app.theme {
        Theme::Dark => "Dark",
        Theme::Light => "Light",
        Theme::System => "System",
    };

    let durations = [2u32, 3, 4, 5, 6, 8, 10]
        .into_iter()
        .filter(|seconds| NOTIFICATION_DURATION_SECONDS.contains(seconds))
        .map(|seconds| {
            chip(
                ("notify-duration", seconds as usize),
                format!("{seconds} s"),
                notifications.duration_seconds == seconds,
                notify_on,
            )
            .when(notify_on, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.config.notifications.duration_seconds = seconds;
                    this.persist_config(cx);
                }))
            })
        })
        .collect::<Vec<_>>();
    let duration_chips = div()
        .flex()
        .gap_1p5()
        .when(!notify_on, |this| this.opacity(0.45))
        .children(durations)
        .into_any_element();

    let replay_durations = REPLAY_DURATIONS_SECONDS
        .into_iter()
        .map(|seconds| {
            chip(
                ("replay-duration", seconds as usize),
                duration_label(seconds),
                replay.duration_seconds == seconds,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.config.replay.duration_seconds = seconds;
                // Applied live; the buffer keeps capturing.
                if matches!(
                    this.recording.replay.phase,
                    ReplayPhase::Starting | ReplayPhase::Running { .. }
                ) {
                    this.send(RecorderCommand::SetReplayDuration(
                        std::time::Duration::from_secs(u64::from(seconds)),
                    ));
                }
                this.persist_config(cx);
            }))
        })
        .collect::<Vec<_>>();
    let replay_memory = REPLAY_MEMORY_PRESETS_MB
        .into_iter()
        .map(|megabytes| {
            chip(
                ("replay-memory", megabytes as usize),
                size_label(megabytes),
                replay.memory_limit_megabytes == megabytes,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.config.replay.memory_limit_megabytes = megabytes;
                this.persist_config(cx);
            }))
        })
        .collect::<Vec<_>>();
    let replay_disk = REPLAY_DISK_PRESETS_MB
        .into_iter()
        .map(|megabytes| {
            chip(
                ("replay-disk", megabytes as usize),
                size_label(megabytes),
                replay.disk_limit_megabytes == megabytes,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.config.replay.disk_limit_megabytes = megabytes;
                this.persist_config(cx);
            }))
        })
        .collect::<Vec<_>>();
    let chips =
        |items: Vec<gpui::Stateful<Div>>| div().flex().gap_1p5().children(items).into_any_element();
    let replay_on = matches!(
        window.recording.replay.phase,
        ReplayPhase::Starting | ReplayPhase::Running { .. }
    ) || (window.recording.replay.is_off() && replay.enabled);
    let replay_toggle = div()
        .id("replay-enabled")
        .cursor_pointer()
        .on_click(cx.listener(|this, _, _, cx| this.toggle_replay(cx)))
        .child(switch(replay_on))
        .into_any_element();
    let cache_directory = match &replay.cache_directory {
        Some(path) => path_text(path.display().to_string()),
        None => match default_replay_cache_directory() {
            Ok(path) => path_text(path.display().to_string()),
            Err(error) => text(error.to_string()),
        },
    };

    let mut root = div()
        .flex()
        .flex_col()
        .gap_6()
        .child(group(
            "Hotkeys",
            vec![("Save Instant Replay", None, window.hotkey_control(cx))],
        ))
        .child(group(
            "Notifications",
            vec![
                (
                    "Show clip notifications",
                    None,
                    notification_switch("notify-enabled", notify_on, true, |n| &mut n.enabled, cx),
                ),
                ("Notification duration", None, duration_chips),
                (
                    "Play notification sound",
                    None,
                    notification_switch(
                        "notify-sound",
                        notifications.sound,
                        notify_on,
                        |n| &mut n.sound,
                        cx,
                    ),
                ),
                (
                    "Open Clip action",
                    None,
                    notification_switch(
                        "notify-open",
                        notifications.open_clip_action,
                        notify_on,
                        |n| &mut n.open_clip_action,
                        cx,
                    ),
                ),
                (
                    "Notify on failure",
                    None,
                    notification_switch(
                        "notify-failure",
                        notifications.notify_on_failure,
                        notify_on,
                        |n| &mut n.notify_on_failure,
                        cx,
                    ),
                ),
            ],
        ))
        .child(group(
            "Video",
            vec![
                (
                    "Resolution",
                    Some("Resolution, frame rate and quality change on the Dashboard."),
                    text(format!("{}×{}", video.width, video.height)),
                ),
                ("Frame rate", None, text(format!("{} fps", video.fps))),
                (
                    "Quality",
                    None,
                    text(QualityProfile::from(config.recording.quality).label()),
                ),
                ("Codec", None, text(codec)),
                ("Encoder", None, text(encoder)),
                (
                    "Base bitrate",
                    Some("The quality preset adjusts the encoded target used by estimates."),
                    text(format!("{} Mbps", video.bitrate_mbps)),
                ),
            ],
        ))
        .child(group(
            "Replay",
            vec![
                (
                    "Instant replay",
                    Some("Starts automatically with Clipper. Needs a source on the Dashboard."),
                    replay_toggle,
                ),
                ("Mode", None, text(mode)),
                (
                    "Duration",
                    Some("Applies immediately, even while replay is running."),
                    chips(replay_durations),
                ),
                (
                    "Memory budget",
                    Some("Older footage spills to disk. Applies the next time replay starts."),
                    chips(replay_memory),
                ),
                (
                    "Disk cache limit",
                    Some("Temporary SSD storage. Applies the next time replay starts."),
                    chips(replay_disk),
                ),
                (
                    "Cache folder",
                    Some("Temporary files, cleaned up automatically."),
                    cache_directory,
                ),
            ],
        ))
        .child(super::audio::content(window, cx));

    let recordings = match config.recording_options() {
        Ok(options) => path_text(options.output_directory.display().to_string()),
        Err(error) => text(error.to_string()),
    };
    let settings_file = match config_path() {
        Ok(path) => path_text(path.display().to_string()),
        Err(_) => text("Unavailable"),
    };
    root = root.child(group(
        "Storage",
        vec![
            ("Recordings folder", None, recordings),
            ("Settings file", None, settings_file),
        ],
    ));

    root.child(group(
        "App",
        vec![
            ("Theme", None, text(theme_name)),
            (
                "Automatic game detection",
                None,
                toggle(app.automatic_detection),
            ),
            ("Manual game selection", None, toggle(app.manual_detection)),
            ("Start on login", None, toggle(app.start_on_login)),
            ("Tray icon", None, toggle(app.tray_icon)),
            ("Minimize to tray", None, toggle(app.minimize_to_tray)),
        ],
    ))
    .child(group(
        "Performance",
        vec![
            (
                "Command queue capacity",
                Some("UI to worker commands."),
                text(perf.command_queue_capacity.to_string()),
            ),
            (
                "Event queue capacity",
                Some("Worker to UI events."),
                text(perf.event_queue_capacity.to_string()),
            ),
        ],
    ))
    .child(div().text_xs().text_color(rgb(theme::TEXT_TERTIARY)).child(
        "Other settings are read from settings.toml; editing them in the app is coming later.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
    };

    #[test]
    fn modifier_only_keys_are_ignored() {
        assert_eq!(hotkey_from_keystroke("control", NONE), None);
        assert_eq!(hotkey_from_keystroke("platform", NONE), None);
    }

    #[test]
    fn maps_combo_and_reports_errors() {
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            ..NONE
        };
        assert_eq!(
            hotkey_from_keystroke("f10", ctrl_shift),
            Some(Ok(Hotkey::default_save_replay()))
        );
        assert_eq!(
            hotkey_from_keystroke("a", NONE),
            Some(Err(HotkeyError::NeedsModifier))
        );
        let ctrl = Modifiers { ctrl: true, ..NONE };
        assert!(matches!(
            hotkey_from_keystroke("tab", ctrl),
            Some(Err(HotkeyError::UnknownKey(_)))
        ));
    }
}
