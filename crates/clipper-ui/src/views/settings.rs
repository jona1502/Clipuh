//! The Settings page: a section list beside a two-column grid of cards
//! (Video, Audio, Replay, Hotkeys, Storage, Appearance, General, Advanced).
//!
//! Every control edits the one `AppConfig` the rest of the app uses; values
//! shared with the capture bar (resolution, frame rate, replay length, Save
//! Clip hotkey) go through the same apply paths as the bar, so both always
//! show the same thing. Settings with no working backend are not shown.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use clipper_core::hotkey::{Hotkey, HotkeyError, Key, Modifiers};
use clipper_platform::windows::hotkey::RegisterError;
use clipper_platform::windows::{shell, startup};
use clipper_storage::{
    ACCENT_PRESETS, AppearanceSettings, DEFAULT_ACCENT, NOTIFICATION_DURATION_SECONDS,
    NotificationSettings, Theme as ThemeMode, UiDensity, config_path,
    default_replay_cache_directory, format_hex_color, parse_hex_color,
};
use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Div, ElementId, Entity, FocusHandle, FontWeight, KeyDownEvent,
    ModifiersChangedEvent, PathPromptOptions, SharedString, Stateful, Subscription, Window, div,
    px, relative,
};

use super::{
    CaptureMenu, ClipperWindow, Confirm, Dismiss, HOTKEY_CONTEXT, Page, SelectNext, SelectPrevious,
};
use crate::components::{
    Activate, ButtonVariant, DropdownOption, Icon, TextInput, TextInputEvent, button, chip,
    dropdown, icon, icon_tile, kbd, step, tooltip,
};
use crate::state::ReplayPhase;
use crate::storage_status::{self, StorageStatus};
use crate::theme::{ActiveTheme, Theme};

/// The Save Clip hotkey slot in the capture bar is as wide as its keycaps,
/// within these bounds (the recording prompt needs a little room; very long
/// combinations are cut off with an ellipsis instead of widening the bar).
const HOTKEY_SLOT_MIN_WIDTH: f32 = 64.0;
const HOTKEY_SLOT_MAX_WIDTH: f32 = 168.0;
/// RAM budgets offered for the replay buffer, in MiB.
const REPLAY_MEMORY_PRESETS_MB: [u32; 5] = [32, 64, 128, 256, 512];
/// Disk cache limits offered for the replay buffer, in MiB.
const REPLAY_DISK_PRESETS_MB: [u32; 6] = [1024, 2048, 4096, 8192, 16384, 32768];
/// Narrowest a card gets before its row wraps to one column.
const CARD_MIN_WIDTH: f32 = 380.0;
/// Below this window width the section list is hidden; the cards still scroll.
mod hex;

const NAV_MIN_VIEWPORT: f32 = 1040.0;
/// Free space below this on the chosen clip folder gets a warning.
const LOW_SPACE_BYTES: u64 = 5 * 1024 * 1024 * 1024;

// Tab order follows the screen: shell and Capture page use 0-11, then the
// section list, then the cards row by row.
const TAB_NAV: isize = 12;
const TAB_MENUS: isize = 20;
const TAB_AUDIO: isize = 27;
const TAB_AUTOSTART: isize = 29;
const TAB_HOTKEY: isize = 30;
const TAB_STORAGE: isize = 31;
const TAB_THEME: isize = 33;
const TAB_ACCENT_SWATCH: isize = 36;
pub(super) const TAB_ACCENT_INPUT: isize = 46;
const TAB_DENSITY: isize = 47;
const TAB_STARTUP: isize = 50;
pub(super) const TAB_NOTIFY: isize = 51;
const TAB_ADVANCED: isize = 55;
/// Diagnostics controls (logging toggle, verbosity, open logs, export).
const TAB_DIAGNOSTICS: isize = 56;

// Tab ranges must not overlap: nav, 7 dropdowns, audio pair, notify toggles.
const _: () = {
    assert!(TAB_NAV + SECTIONS.len() as isize <= TAB_MENUS);
    assert!(TAB_MENUS + 7 <= TAB_AUDIO);
    assert!(TAB_NOTIFY + 4 <= TAB_ADVANCED);
    assert!(TAB_ACCENT_SWATCH + ACCENT_PRESETS.len() as isize <= TAB_ACCENT_INPUT);
    assert!(TAB_ADVANCED < TAB_DIAGNOSTICS);
};

/// The section list, in page order: (glyph, name, subtitle).
pub(super) const SECTIONS: [(Icon, &str, &str); 8] = [
    (
        Icon::Video,
        "Video",
        "Capture quality and encoding settings.",
    ),
    (Icon::Volume, "Audio", "Recorded audio and track layout."),
    (
        Icon::History,
        "Replay",
        "Instant replay and buffer settings.",
    ),
    (Icon::Keyboard, "Hotkeys", "Global shortcuts for clipping."),
    (
        Icon::Drive,
        "Storage",
        "Where clips are saved and disk usage.",
    ),
    (
        Icon::Palette,
        "Appearance",
        "Customize the look and feel of clipuh.",
    ),
    (Icon::Settings, "General", "Startup and notifications."),
    (
        Icon::Settings,
        "Advanced",
        "Settings file, engine details and diagnostics.",
    ),
];

fn size_label(megabytes: u32) -> String {
    if megabytes >= 1024 && megabytes.is_multiple_of(1024) {
        format!("{} GB", megabytes / 1024)
    } else {
        format!("{megabytes} MB")
    }
}

// ---------------------------------------------------------------------------
// Save Clip hotkey recorder (shared by the capture bar and the Hotkeys card)
// ---------------------------------------------------------------------------

/// Where a hotkey recording was started; only that control shows the recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HotkeyHost {
    Bar,
    Settings,
}

/// Recording mode for the save-replay shortcut.
pub(super) struct HotkeyRecorder {
    host: HotkeyHost,
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

fn keycaps(t: &Theme, text: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .children(text.split('+').map(|key| kbd(t, key.to_owned())))
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

    fn begin_hotkey_recording(
        &mut self,
        host: HotkeyHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.hotkey_recorder.is_some() {
            return;
        }
        let focus = cx.focus_handle();
        window.focus(&focus);
        let blur = cx.on_blur(&focus, window, |this, _, cx| {
            this.leave_hotkey_recording(cx)
        });
        self.hotkey_recorder = Some(HotkeyRecorder {
            host,
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
                    // Put the previous working combo back right away, so a failed
                    // change never leaves Clipper without a Save Clip hotkey. The
                    // recorder stays open for another try; the next attempt
                    // replaces this registration again.
                    let _ = this.hotkeys.set(Some(this.hotkey));
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

    /// The Save Clip shortcut control for the capture bar.
    pub(super) fn hotkey_bar_control(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.hotkey_control(t, HotkeyHost::Bar, window, cx)
    }

    /// The Save Clip shortcut as one compact control: idle keycaps, a
    /// recording prompt, a transient "Saving…", and a danger border plus
    /// tooltip on error (never taller than `control_height`). Both hosts edit
    /// the same hotkey; only the host that started recording shows the recorder.
    /// The previous binding is released while recording (so its keys reach the
    /// recorder) and registered again on cancel or after a failed attempt.
    fn hotkey_control(
        &self,
        t: &Theme,
        host: HotkeyHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let in_bar = host == HotkeyHost::Bar;
        // In the bar the slot hugs its content (bounded); it always keeps one row.
        let shell = |id: &'static str, border| {
            div()
                .id(id)
                .flex_none()
                .when(in_bar, |this| {
                    this.min_w(px(HOTKEY_SLOT_MIN_WIDTH))
                        .max_w(px(HOTKEY_SLOT_MAX_WIDTH))
                })
                .when(!in_bar, |this| this.w_full())
                .overflow_hidden()
                .h(t.metrics.control_height)
                .px(if in_bar {
                    px(8.)
                } else {
                    t.metrics.control_padding
                })
                .flex()
                .items_center()
                .gap(t.metrics.gap)
                .rounded_md()
                .border_1()
                .border_color(border)
                .bg(t.colors.surface_raised)
        };
        let line = |color, text: String| {
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_xs()
                .text_color(color)
                .child(text)
        };

        let Some(recorder) = self.hotkey_recorder.as_ref().filter(|r| r.host == host) else {
            let (key, tab, id) = if in_bar {
                // After the bar's replay-length menu (6), before its replay switch (8).
                ("hotkey-bar-focus", 7, "hotkey-bar")
            } else {
                ("hotkey-settings-focus", TAB_HOTKEY, "hotkey-settings")
            };
            let focus = window
                .use_keyed_state(key, cx, move |_, cx| {
                    cx.focus_handle().tab_index(tab).tab_stop(true)
                })
                .read(cx)
                .clone();
            let error = self.hotkey_error.clone();
            let border = if error.is_some() {
                t.colors.danger
            } else if focus.is_focused(window) {
                t.colors.focus_ring
            } else {
                t.colors.border
            };
            return shell(id, border)
                .cursor_pointer()
                .key_context("Toggle")
                .track_focus(&focus)
                .tooltip(tooltip(
                    error.unwrap_or_else(|| "Change the Save Clip hotkey".to_owned()),
                ))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.begin_hotkey_recording(host, window, cx)
                }))
                .on_action(cx.listener(move |this, _: &Activate, window, cx| {
                    this.begin_hotkey_recording(host, window, cx)
                }))
                .child(keycaps(t, &self.hotkey.to_string()))
                .when(!in_bar, |this| {
                    this.child(div().flex_1())
                        .child(div().text_sm().text_color(t.colors.text_muted).child("✎"))
                })
                .into_any_element();
        };

        let error = recorder.error.clone();
        let prompt = if recorder.pending {
            line(t.colors.text_muted, "Saving…".to_owned())
        } else if let Some(error) = &error {
            line(t.colors.danger, error.clone())
        } else if recorder.held.is_empty() {
            line(t.colors.text_muted, "Press keys…".to_owned())
        } else {
            keycaps(t, &modifier_names(recorder.held))
                .child(div().text_color(t.colors.text_muted).child("+ …"))
        };
        let border = if error.is_some() {
            t.colors.danger
        } else {
            t.colors.focus_ring
        };
        let link = |id: &'static str, label: &'static str, hint: &'static str| {
            div()
                .id(id)
                .flex_none()
                .cursor_pointer()
                .text_xs()
                .text_color(t.colors.text_muted)
                .hover(|this| this.text_color(t.colors.text))
                .tooltip(tooltip(hint))
                .child(label)
        };
        let cancel = (!recorder.pending).then(|| {
            link("hotkey-cancel", "×", "Cancel (Esc)").on_click(
                cx.listener(|this, _, window, cx| this.cancel_hotkey_recording(window, cx)),
            )
        });
        let default = Hotkey::default_save_replay();
        let reset = (!recorder.pending && self.hotkey != default).then(|| {
            link("hotkey-default", "Default", "Use the default shortcut").on_click(
                cx.listener(move |this, _, window, cx| this.apply_hotkey(default, window, cx)),
            )
        });
        shell("hotkey-recorder", border)
            .key_context(HOTKEY_CONTEXT)
            .track_focus(&recorder.focus)
            .when_some(error, |this, error| this.tooltip(tooltip(error)))
            .on_key_down(cx.listener(Self::hotkey_key_down))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                if let Some(recorder) = &mut this.hotkey_recorder {
                    recorder.held = core_modifiers(event.modifiers);
                    cx.notify();
                }
            }))
            .child(prompt)
            .children(reset)
            .children(cancel)
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Settings dropdowns
// ---------------------------------------------------------------------------

/// A dropdown on the Settings page. The first five edit the same values as the
/// capture bar and Capture page and go through their apply path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SettingsMenu {
    Resolution,
    FrameRate,
    Quality,
    Bitrate,
    ReplayDuration,
    MemoryBudget,
    DiskLimit,
}

impl SettingsMenu {
    fn capture(self) -> Option<CaptureMenu> {
        Some(match self {
            Self::Resolution => CaptureMenu::Resolution,
            Self::FrameRate => CaptureMenu::FrameRate,
            Self::Quality => CaptureMenu::Quality,
            Self::Bitrate => CaptureMenu::Bitrate,
            Self::ReplayDuration => CaptureMenu::ReplayDuration,
            Self::MemoryBudget | Self::DiskLimit => return None,
        })
    }
}

/// Preset options plus, when `current` is not a preset (hand-edited config),
/// one extra row for it so the trigger never shows a wrong value.
fn preset_options(presets: &[u32], current: u32) -> (Vec<DropdownOption>, usize) {
    let option = |megabytes: u32, detail: Option<&'static str>| DropdownOption {
        label: size_label(megabytes).into(),
        detail: detail.map(SharedString::from),
        enabled: true,
        reason: None,
    };
    let mut options: Vec<_> = presets.iter().map(|mb| option(*mb, None)).collect();
    let selected = match presets.iter().position(|mb| *mb == current) {
        Some(index) => index,
        None => {
            options.push(option(current, Some("from settings file")));
            options.len() - 1
        }
    };
    (options, selected)
}

impl ClipperWindow {
    fn settings_options(&self, menu: SettingsMenu) -> (Vec<DropdownOption>, usize) {
        let replay = &self.config.replay;
        match menu {
            SettingsMenu::MemoryBudget => {
                preset_options(&REPLAY_MEMORY_PRESETS_MB, replay.memory_limit_megabytes)
            }
            SettingsMenu::DiskLimit => {
                preset_options(&REPLAY_DISK_PRESETS_MB, replay.disk_limit_megabytes)
            }
            _ => {
                let capture = menu.capture().expect("capture-backed menu");
                (
                    self.capture_options(capture),
                    self.capture_selected(capture),
                )
            }
        }
    }

    fn settings_toggle(&mut self, menu: SettingsMenu, cx: &mut Context<Self>) {
        if matches!(self.settings_menu, Some((open, _)) if open == menu) {
            self.settings_close(cx);
        } else {
            self.settings_menu = Some((menu, self.settings_options(menu).1));
            cx.notify();
        }
    }

    pub(super) fn settings_close(&mut self, cx: &mut Context<Self>) {
        if self.settings_menu.take().is_some() {
            cx.notify();
        }
    }

    fn settings_apply(&mut self, menu: SettingsMenu, index: usize, cx: &mut Context<Self>) {
        self.settings_menu = None;
        if let Some(capture) = menu.capture() {
            self.capture_apply(capture, index, cx);
            return;
        }
        let (presets, field) = match menu {
            SettingsMenu::MemoryBudget => (
                &REPLAY_MEMORY_PRESETS_MB[..],
                &mut self.config.replay.memory_limit_megabytes,
            ),
            _ => (
                &REPLAY_DISK_PRESETS_MB[..],
                &mut self.config.replay.disk_limit_megabytes,
            ),
        };
        // The extra "from settings file" row keeps the current value.
        match presets.get(index) {
            Some(megabytes) if *field != *megabytes => {
                *field = *megabytes;
                self.persist_config(cx);
            }
            _ => cx.notify(),
        }
    }

    fn settings_confirm(&mut self, menu: SettingsMenu, cx: &mut Context<Self>) {
        match self.settings_menu {
            Some((open, highlighted)) if open == menu => self.settings_apply(menu, highlighted, cx),
            _ => self.settings_toggle(menu, cx),
        }
    }

    fn settings_step(&mut self, menu: SettingsMenu, delta: isize, cx: &mut Context<Self>) {
        let Some((_, from)) = self.settings_menu.filter(|(open, _)| *open == menu) else {
            self.settings_toggle(menu, cx);
            return;
        };
        let next = step(&self.settings_options(menu).0, from, delta);
        if next != from {
            self.settings_menu = Some((menu, next));
            cx.notify();
        }
    }

    fn settings_dropdown(
        &self,
        t: &Theme,
        menu: SettingsMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = focus_handle(TAB_MENUS + menu as isize, window, cx);
        let (options, selected) = self.settings_options(menu);
        let highlighted = self
            .settings_menu
            .filter(|(open, _)| *open == menu)
            .map(|(_, highlighted)| highlighted);
        let entity = cx.entity();
        let (on_toggle, on_select, on_dismiss) = (entity.clone(), entity.clone(), entity);
        let trigger = dropdown(
            t,
            ("settings-menu", menu as usize),
            &options,
            selected,
            highlighted,
            &focus,
            focus.is_focused(window),
            true,
            move |_, app| on_toggle.update(app, |this, cx| this.settings_toggle(menu, cx)),
            move |index, _, app| {
                on_select.update(app, |this, cx| this.settings_apply(menu, index, cx))
            },
            move |_, app| on_dismiss.update(app, |this, cx| this.settings_close(cx)),
        );
        div()
            .id(("settings-slot", menu as usize))
            .relative()
            .w_full()
            .child(trigger)
            .on_action(cx.listener(move |this, _: &Confirm, _, cx| this.settings_confirm(menu, cx)))
            .on_action(
                cx.listener(move |this, _: &SelectNext, _, cx| this.settings_step(menu, 1, cx)),
            )
            .on_action(
                cx.listener(move |this, _: &SelectPrevious, _, cx| {
                    this.settings_step(menu, -1, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.settings_close(cx)))
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Runtime actions: appearance, launch at sign-in, clip folder
// ---------------------------------------------------------------------------

/// The custom accent field; submitting a valid `#RRGGBB` applies it.
pub(super) fn accent_input(
    config: &clipper_storage::AppConfig,
    window: &mut Window,
    cx: &mut Context<ClipperWindow>,
) -> Entity<TextInput> {
    let input = cx.new(|cx| {
        let mut input = TextInput::new(cx)
            .placeholder(DEFAULT_ACCENT)
            .allowed_chars(hex::is_hex_input_char)
            .max_len(hex::MAX_INPUT_CHARS)
            .tab_index(TAB_ACCENT_INPUT);
        input.set_text(config.appearance.accent_color.clone(), cx);
        input
    });
    cx.subscribe_in(
        &input,
        window,
        |this, input, event: &TextInputEvent, window, cx| match event {
            TextInputEvent::Dismissed => window.focus(&this.focus),
            TextInputEvent::Changed(_) => input.update(cx, |input, cx| input.set_error(None, cx)),
            // Applied (and saved) only on Enter with a complete colour; an
            // invalid entry keeps the last valid accent.
            TextInputEvent::Submitted(text) => match hex::normalize(text) {
                Ok(hex) => {
                    input.update(cx, |input, cx| input.set_text(hex.clone(), cx));
                    this.set_appearance(|appearance| appearance.accent_color = hex, cx);
                }
                Err(error) => input.update(cx, |input, cx| {
                    input.set_error(Some(error.message().into()), cx)
                }),
            },
        },
    )
    .detach();
    input
}

/// Creates (or reuses) a valid, writable clip folder. Blocking.
fn prepare_clip_directory(path: &Path) -> Result<StorageStatus, String> {
    if !path.is_absolute() {
        return Err("Choose a full folder path, such as D:\\Clips.".into());
    }
    std::fs::create_dir_all(path)
        .map_err(|error| format!("Cannot create {}: {error}", path.display()))?;
    let probe = path.join(".clipuh-write-test");
    std::fs::write(&probe, b"")
        .map_err(|error| format!("Cannot write to {}: {error}", path.display()))?;
    let _ = std::fs::remove_file(&probe);
    match storage_status::load(path) {
        StorageStatus::Unavailable(error) => Err(format!(
            "Cannot read free space on {}: {error}",
            path.display()
        )),
        status => Ok(status),
    }
}

impl ClipperWindow {
    /// Applies an appearance change to every window at once, then saves it.
    fn set_appearance(
        &mut self,
        edit: impl FnOnce(&mut AppearanceSettings),
        cx: &mut Context<Self>,
    ) {
        let before = self.config.appearance.clone();
        edit(&mut self.config.appearance);
        if self.config.appearance == before {
            return;
        }
        crate::theme::apply(&self.config.appearance, cx);
        self.persist_config(cx);
    }

    /// Reads whether clipuh is registered to launch at sign-in (the registry is
    /// the source of truth, not the saved flag). `on` also changes it first.
    pub(super) fn sync_startup(&mut self, on: Option<bool>, cx: &mut Context<Self>) {
        self.startup = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (changed, state) = cx
                .background_spawn(async move {
                    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
                    let changed = on.map(|on| startup::set_enabled(&exe, on));
                    let state = startup::is_enabled(&exe).map_err(|error| error.to_string());
                    Ok::<_, String>((changed, state))
                })
                .await
                .unwrap_or_else(|error| (None, Err(error)));
            this.update(cx, |this, cx| {
                this.startup_error = match (&changed, &state) {
                    (Some(Err(error)), _) => Some(format!("Windows refused the change: {error}")),
                    (_, Err(error)) => Some(format!("Cannot read the startup setting: {error}")),
                    _ => None,
                };
                this.startup = state.ok();
                if let Some(on) = this.startup
                    && this.config.app.start_on_login != on
                {
                    this.config.app.start_on_login = on;
                    this.persist_config(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_clip_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.recording.settings_enabled() {
            self.settings_notice =
                Some("Turn off replay and stop recording to change the clip folder.".into());
            cx.notify();
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match paths.await {
                Ok(Ok(Some(mut paths))) => paths.pop(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| {
                        this.settings_notice = Some(format!("Folder picker failed: {error}"));
                        cx.notify();
                    })
                    .ok();
                    None
                }
            };
            let Some(path) = picked else { return };
            let check = path.clone();
            let result = cx
                .background_spawn(async move { prepare_clip_directory(&check) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.finish_clip_folder(path, result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish_clip_folder(
        &mut self,
        path: PathBuf,
        result: Result<StorageStatus, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.notify();
        let status = match result {
            Ok(status) => status,
            Err(error) => {
                self.settings_notice = Some(error);
                return;
            }
        };
        // Replay may have started while the picker was open.
        if !self.recording.settings_enabled() {
            self.settings_notice =
                Some("Turn off replay and stop recording to change the clip folder.".into());
            return;
        }
        let previous = self.config.recording.output_directory.replace(path);
        if let Err(error) = self.config.validate() {
            self.config.recording.output_directory = previous;
            self.settings_notice = Some(error.to_string());
            return;
        }
        self.settings_notice = match status {
            StorageStatus::Ready(usage) if usage.free_bytes < LOW_SPACE_BYTES => {
                Some(format!("Low disk space: {}.", usage.free_label()))
            }
            _ => None,
        };
        self.persist_config(cx);
        self.refresh_storage(cx);
        self.refresh_recent_clips(window, cx);
    }

    fn open_folder(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.settings_notice = match path.map(|path| shell::open_path(&path)) {
            Some(Ok(())) => None,
            Some(Err(error)) => Some(error.to_string()),
            None => Some("The folder is unavailable.".into()),
        };
        cx.notify();
    }

    /// Detailed (Debug/Trace) logging on or off; applies immediately.
    fn set_detailed_logging(&mut self, on: bool, cx: &mut Context<Self>) {
        let now = crate::diagnostics::now_unix();
        self.config.diagnostics.set_detailed(on, now);
        crate::diagnostics::apply_log_settings(&self.config.diagnostics);
        self.persist_config(cx);
    }

    fn set_detailed_level(
        &mut self,
        level: clipper_storage::DetailedLogLevel,
        cx: &mut Context<Self>,
    ) {
        self.config.diagnostics.detailed_level = level;
        crate::diagnostics::apply_log_settings(&self.config.diagnostics);
        self.persist_config(cx);
    }

    /// Asks where to save, then writes the redacted ZIP off the UI thread.
    /// Success is reported only once the archive is complete.
    fn export_debug_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.diagnostics_exporting {
            return;
        }
        let name = crate::diagnostics::export_file_name(crate::diagnostics::now_unix());
        let destination =
            cx.prompt_for_new_path(&crate::diagnostics::export_directory(), Some(&name));
        let info = crate::diagnostics::system_info(
            &self.config,
            self.recording.encoder.as_deref(),
            self.recording.error.as_deref(),
        );
        self.diagnostics_exporting = true;
        self.diagnostics_notice = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let picked = match destination.await {
                Ok(Ok(Some(path))) => Some(Ok(path)),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => Some(Err(format!("Save dialog failed: {error}"))),
            };
            let result = match picked {
                None => None,
                Some(Err(error)) => Some(Err(error)),
                Some(Ok(path)) => {
                    let path = if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                    {
                        path
                    } else {
                        path.with_extension("zip")
                    };
                    let shown = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let outcome = cx
                        .background_spawn(async move { crate::diagnostics::export(&path, &info) })
                        .await;
                    Some(outcome.map(|summary| (shown, summary)))
                }
            };
            this.update(cx, |this, cx| {
                this.diagnostics_exporting = false;
                this.diagnostics_notice = result.map(|result| match result {
                    Ok((name, summary)) => {
                        clipper_diagnostics::log::info!(
                            "debug logs exported ({} files, {} KiB)",
                            summary.files,
                            summary.bytes / 1024
                        );
                        (
                            false,
                            format!(
                                "Saved {name} ({} files). Nothing was uploaded.",
                                summary.files
                            ),
                        )
                    }
                    Err(error) => {
                        clipper_diagnostics::log::error!("debug log export failed: {error}");
                        (true, format!("Export failed: {error}"))
                    }
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Shows only category `index`; the content starts at its top.
    fn select_section(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= SECTIONS.len() {
            return;
        }
        if self.settings_section != index {
            self.settings_section = index;
            self.settings_scroll.set_offset(gpui::Point::default());
        }
        cx.notify();
    }

    /// The card of the selected category (the only one rendered).
    fn section_card(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        match self.settings_section {
            0 => self.settings_video_card(t, window, cx),
            1 => self.settings_audio_card(t, window, cx),
            2 => self.settings_replay_card(t, window, cx),
            3 => self.settings_hotkeys_card(t, window, cx),
            4 => self.settings_storage_card(t, window, cx),
            5 => self.settings_appearance_card(t, window, cx),
            6 => self.settings_general_card(t, window, cx),
            _ => self.settings_advanced_card(t, window, cx),
        }
    }
}

// ---------------------------------------------------------------------------
// Building blocks
// ---------------------------------------------------------------------------

/// Focus handle for a Settings control, kept across renders by tab index.
fn focus_handle(tab: isize, window: &mut Window, cx: &mut Context<ClipperWindow>) -> FocusHandle {
    window
        .use_keyed_state(
            ElementId::NamedInteger("settings-focus".into(), tab as u64),
            cx,
            move |_, cx| cx.focus_handle().tab_index(tab).tab_stop(true),
        )
        .read(cx)
        .clone()
}

type Handler = Rc<dyn Fn(&mut ClipperWindow, &mut Window, &mut Context<ClipperWindow>)>;

/// Click, or Space/Enter while focused, runs `on`; draws a focus ring.
fn activatable(
    element: Stateful<Div>,
    t: &Theme,
    tab: isize,
    window: &mut Window,
    cx: &mut Context<ClipperWindow>,
    on: impl Fn(&mut ClipperWindow, &mut Window, &mut Context<ClipperWindow>) + 'static,
) -> Stateful<Div> {
    let focus = focus_handle(tab, window, cx);
    let focused = focus.is_focused(window);
    let on: Handler = Rc::new(on);
    let on_key = on.clone();
    let focus_click = focus.clone();
    element
        .key_context("Toggle")
        .track_focus(&focus)
        .when(focused, |this| this.border_color(t.colors.focus_ring))
        .on_click(cx.listener(move |this, _, window, cx| {
            window.focus(&focus_click);
            on(this, window, cx)
        }))
        .on_action(cx.listener(move |this, _: &Activate, window, cx| on_key(this, window, cx)))
}

/// A labelled checkbox; `on_change` gets the new value.
#[allow(clippy::too_many_arguments)]
fn checkbox(
    t: &Theme,
    id: &'static str,
    label: &'static str,
    hint: Option<&'static str>,
    on: bool,
    enabled: bool,
    tab: isize,
    window: &mut Window,
    cx: &mut Context<ClipperWindow>,
    on_change: impl Fn(&mut ClipperWindow, bool, &mut Context<ClipperWindow>) + 'static,
) -> AnyElement {
    let mark = div()
        .flex_none()
        .size(px(18.))
        .rounded(px(4.))
        .border_1()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(t.colors.accent_foreground)
        .map(|this| {
            if on {
                this.border_color(t.colors.accent)
                    .bg(t.colors.accent)
                    .child("✓")
            } else {
                this.border_color(t.colors.border_strong)
                    .bg(t.colors.surface_raised)
            }
        });
    let row = div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .px_1()
        .py_0p5()
        .rounded_md()
        .border_1()
        .border_color(gpui::transparent_black())
        .child(mark)
        .child(div().text_sm().child(label))
        .children(hint.map(|hint| {
            div()
                .id((id, 1usize))
                .text_xs()
                .text_color(t.colors.text_muted)
                .tooltip(tooltip(hint))
                .child("ⓘ")
        }));
    if !enabled {
        return row.opacity(0.45).into_any_element();
    }
    activatable(
        row.cursor_pointer(),
        t,
        tab,
        window,
        cx,
        move |this, _, cx| on_change(this, !on, cx),
    )
    .into_any_element()
}

/// Read-only value drawn like a field, for settings with a single option.
fn field(t: &Theme, value: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .h(t.metrics.control_height)
        .px(t.metrics.control_padding)
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(t.colors.border)
        .bg(t.colors.surface)
        .text_sm()
        .text_color(t.colors.text_secondary)
        .overflow_hidden()
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(value.into()),
        )
}

fn note(t: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .text_color(t.colors.text_muted)
        .child(text.into())
}

fn error_note(t: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .text_color(t.colors.danger)
        .child(text.into())
}

/// Label on the left, control in the right column.
fn row(t: &Theme, label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(t.colors.text)
                .child(label.into()),
        )
        .child(div().flex_none().w(relative(0.56)).child(control))
}

/// A section card with the glyph, title and subtitle header of `SECTIONS[index]`.
fn card(t: &Theme, index: usize) -> Stateful<Div> {
    let (glyph, title, subtitle) = SECTIONS[index];
    div()
        .id(("settings-card", index))
        .flex_1()
        .flex_basis(px(0.))
        .min_w(px(CARD_MIN_WIDTH))
        .flex()
        .flex_col()
        .gap_3()
        .p(px(16.))
        .rounded_lg()
        .border_1()
        .border_color(t.colors.border)
        .bg(t.colors.surface)
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .pb_1()
                .child(icon_tile(t, glyph, px(36.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(note(t, subtitle)),
                ),
        )
}

// ---------------------------------------------------------------------------
// Cards
// ---------------------------------------------------------------------------

impl ClipperWindow {
    fn settings_video_card(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        // The engine reports the encoder it picked; before any capture it is automatic.
        let encoder = match &self.recording.replay.phase {
            ReplayPhase::Running { encoder, .. } => encoder.clone(),
            _ => self
                .recording
                .encoder
                .clone()
                .unwrap_or_else(|| "Auto (hardware first)".to_owned()),
        };
        let pending = self.capture_pending(CaptureMenu::Resolution)
            || self.capture_pending(CaptureMenu::FrameRate);
        card(t, 0)
            .child(row(
                t,
                "Resolution",
                self.settings_dropdown(t, SettingsMenu::Resolution, window, cx),
            ))
            .child(row(
                t,
                "Frame Rate",
                self.settings_dropdown(t, SettingsMenu::FrameRate, window, cx),
            ))
            .child(row(
                t,
                "Quality",
                self.settings_dropdown(t, SettingsMenu::Quality, window, cx),
            ))
            .child(row(
                t,
                "Bitrate",
                self.settings_dropdown(t, SettingsMenu::Bitrate, window, cx),
            ))
            .child(row(t, "Encoder", field(t, format!("H.264 · {encoder}"))))
            .child(note(
                t,
                if pending {
                    "Resolution and frame rate apply when replay or recording restarts."
                } else {
                    "Shared with the capture bar. Changes apply to the next capture."
                },
            ))
    }

    fn settings_audio_card(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let sources = &self.config.audio.sources;
        let mut body = card(t, 1);
        if sources.is_empty() {
            body = body.child(note(t, "No audio sources. Add them in the mixer."));
        }
        for source in sources {
            let resolved = self.resolved_name(source);
            let (name, subtitle) = super::audio::row_labels(
                source.kind,
                source.target.as_deref(),
                resolved.as_deref(),
            );
            let value = if source.enabled {
                subtitle
            } else {
                format!("{subtitle} · off")
            };
            body = body.child(row(t, name, field(t, value)));
        }
        let mixer = activatable(
            button(
                t,
                "settings-open-mixer",
                "Manage in mixer",
                ButtonVariant::Secondary,
                None,
                true,
            ),
            t,
            TAB_AUDIO + 1,
            window,
            cx,
            |this, window, cx| this.navigate(Page::Dashboard, window, cx),
        );
        body.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(checkbox(
                    t,
                    "settings-separate-tracks",
                    "Separate audio tracks",
                    Some("Save the mix plus one track per source, for editing later."),
                    self.config.audio.separate_tracks,
                    true,
                    TAB_AUDIO,
                    window,
                    cx,
                    |this, on, cx| {
                        this.config.audio.separate_tracks = on;
                        this.persist_config(cx);
                    },
                ))
                .child(mixer),
        )
        .child(note(
            t,
            "Sources, devices and levels are managed in the mixer on the Capture page.",
        ))
    }

    fn settings_replay_card(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let running = !self.recording.replay.is_off();
        let cache = match &self.config.replay.cache_directory {
            Some(path) => path.display().to_string(),
            None => default_replay_cache_directory()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|error| error.to_string()),
        };
        card(t, 2)
            .child(row(
                t,
                "Replay Duration",
                self.settings_dropdown(t, SettingsMenu::ReplayDuration, window, cx),
            ))
            .child(row(t, "Buffer Location", field(t, "RAM + Disk (Hybrid)")))
            .child(row(
                t,
                "Memory Budget",
                self.settings_dropdown(t, SettingsMenu::MemoryBudget, window, cx),
            ))
            .child(row(
                t,
                "Disk Cache Limit",
                self.settings_dropdown(t, SettingsMenu::DiskLimit, window, cx),
            ))
            .child(row(t, "Cache Folder", field(t, cache)))
            .child(checkbox(
                t,
                "settings-replay-autostart",
                "Start replay automatically",
                Some("Turns replay on when clipuh opens. Follows the replay switch."),
                self.config.replay.enabled,
                true,
                TAB_AUTOSTART,
                window,
                cx,
                |this, on, cx| {
                    this.config.replay.enabled = on;
                    this.persist_config(cx);
                },
            ))
            .when(running, |this| {
                this.child(note(
                    t,
                    "Memory and disk limits apply the next time replay starts; duration applies now.",
                ))
            })
    }

    fn settings_hotkeys_card(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        card(t, 3)
            .child(row(
                t,
                "Save Clip (Replay)",
                self.hotkey_control(t, HotkeyHost::Settings, window, cx),
            ))
            .children(self.hotkey_error.clone().map(|error| error_note(t, error)))
            .child(note(
                t,
                "Click the shortcut, then press the new keys. Esc cancels. Also editable in the capture bar.",
            ))
    }

    fn settings_storage_card(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let directory = self.config.recording_options().map(|o| o.output_directory);
        let editable = self.recording.settings_enabled();
        let path = match &directory {
            Ok(path) => path.display().to_string(),
            Err(error) => error.to_string(),
        };
        let browse = activatable(
            button(
                t,
                "settings-clip-folder",
                "Change…",
                ButtonVariant::Secondary,
                None,
                editable,
            ),
            t,
            TAB_STORAGE,
            window,
            cx,
            |this, window, cx| this.choose_clip_folder(window, cx),
        );
        let open_target = directory.as_ref().ok().cloned();
        let open = activatable(
            button(
                t,
                "settings-open-clips",
                "Open",
                ButtonVariant::Ghost,
                None,
                true,
            ),
            t,
            TAB_STORAGE + 1,
            window,
            cx,
            move |this, _, cx| this.open_folder(open_target.clone(), cx),
        );
        let usage = match &self.storage {
            StorageStatus::Ready(usage) => {
                format!("{} · {}", usage.used_label(), usage.free_label())
            }
            StorageStatus::Unknown => "Checking…".into(),
            StorageStatus::Unavailable(error) => error.clone(),
        };
        card(t, 4)
            .child(row(
                t,
                "Clip Location",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(field(t, path)))
                    .child(browse)
                    .child(open),
            ))
            .child(row(t, "Disk Usage", field(t, usage)))
            .children(
                self.settings_notice
                    .clone()
                    .map(|notice| error_note(t, notice)),
            )
            .child(note(
                t,
                if editable {
                    "New clips save here. Existing clips stay where they are."
                } else {
                    "Turn off replay and stop recording to change the folder."
                },
            ))
    }

    fn settings_appearance_card(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let appearance = &self.config.appearance;
        let modes = [
            (ThemeMode::Dark, "☾", "Dark"),
            (ThemeMode::Light, "☀", "Light"),
            (ThemeMode::System, "▭", "System"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (mode, glyph, label))| {
            let selected = appearance.mode == mode;
            let segment = div()
                .id(("theme-mode", index))
                .flex_1()
                .h(t.metrics.control_height)
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .text_sm()
                .cursor_pointer()
                .map(|this| {
                    if selected {
                        this.border_color(t.colors.accent)
                            .bg(t.colors.accent_subtle)
                            .text_color(t.colors.text)
                    } else {
                        this.border_color(t.colors.border)
                            .bg(t.colors.surface_raised)
                            .text_color(t.colors.text_secondary)
                            .hover(|style| style.bg(t.colors.surface_hover))
                    }
                })
                .child(glyph)
                .child(label);
            activatable(
                segment,
                t,
                TAB_THEME + index as isize,
                window,
                cx,
                move |this, _, cx| this.set_appearance(|a| a.mode = mode, cx),
            )
        })
        .collect::<Vec<_>>();
        let current = parse_hex_color(&appearance.accent_color);
        let swatches = ACCENT_PRESETS
            .into_iter()
            .enumerate()
            .filter_map(|(index, (name, hex))| {
                let rgb = parse_hex_color(hex)?;
                let selected = current == Some(rgb);
                let swatch = div()
                    .id(("accent", index))
                    .size(px(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_2()
                    // A thin outline keeps White on light and Black on dark
                    // surfaces visible; the selected swatch gets a strong ring.
                    .border_color(if selected {
                        t.colors.text
                    } else {
                        t.colors.border_strong
                    })
                    .bg(gpui::rgb(rgb))
                    .cursor_pointer()
                    .tooltip(tooltip(name))
                    .when(selected, |this| {
                        this.text_color(crate::theme::palette::foreground_for(gpui::rgb(rgb)))
                            .font_weight(FontWeight::BOLD)
                            .child("✓")
                    });
                Some(activatable(
                    swatch,
                    t,
                    TAB_ACCENT_SWATCH + index as isize,
                    window,
                    cx,
                    move |this, _, cx| {
                        let hex = format_hex_color(rgb);
                        this.accent_input
                            .update(cx, |input, cx| input.set_text(hex.clone(), cx));
                        this.set_appearance(|a| a.accent_color = hex, cx);
                    },
                ))
            })
            .collect::<Vec<_>>();
        let densities = [
            (UiDensity::Compact, "Compact"),
            (UiDensity::Default, "Default"),
            (UiDensity::Relaxed, "Relaxed"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (density, label))| {
            activatable(
                chip(
                    t,
                    ("density", index),
                    label,
                    appearance.density == density,
                    true,
                ),
                t,
                TAB_DENSITY + index as isize,
                window,
                cx,
                move |this, _, cx| this.set_appearance(|a| a.density = density, cx),
            )
        })
        .collect::<Vec<_>>();
        let label = |text: &'static str| div().text_sm().child(text);
        let controls = div()
            .flex_1()
            .min_w(px(260.))
            .flex()
            .flex_col()
            .gap_3()
            .child(label("Theme"))
            .child(div().flex().gap_2().children(modes))
            .child(label("Accent Color"))
            .child(div().flex().flex_wrap().gap_2().children(swatches))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().w(px(130.)).child(self.accent_input.clone()))
                    .child(note(t, "Custom hex, Enter to apply")),
            )
            .child(label("Density"))
            .child(div().flex().gap_1p5().children(densities));
        card(t, 5).child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .child(controls)
                .child(live_preview(t)),
        )
    }

    fn settings_general_card(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let notifications = &self.config.notifications;
        let notify_on = notifications.enabled;
        let durations = [2u32, 3, 4, 5, 6, 8, 10]
            .into_iter()
            .filter(|seconds| NOTIFICATION_DURATION_SECONDS.contains(seconds))
            .map(|seconds| {
                chip(
                    t,
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
        let notify = |id,
                      label,
                      field: fn(&mut NotificationSettings) -> &mut bool,
                      on,
                      enabled,
                      slot: isize,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
            checkbox(
                t,
                id,
                label,
                None,
                on,
                enabled,
                TAB_NOTIFY + slot,
                window,
                cx,
                move |this, value, cx| {
                    *field(&mut this.config.notifications) = value;
                    this.persist_config(cx);
                },
            )
        };
        let startup = checkbox(
            t,
            "settings-startup",
            "Launch clipuh when Windows starts",
            Some("Adds clipuh to your sign-in apps. No admin rights needed."),
            self.startup.unwrap_or(false),
            self.startup.is_some(),
            TAB_STARTUP,
            window,
            cx,
            |this, on, cx| this.sync_startup(Some(on), cx),
        );
        card(t, 6)
            .child(startup)
            .children(self.startup_error.clone().map(|error| error_note(t, error)))
            .child(div().h(px(1.)).bg(t.colors.border))
            .child(notify(
                "notify-enabled",
                "Show clip notifications",
                |n| &mut n.enabled,
                notify_on,
                true,
                0,
                window,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_1()
                    .child(div().text_sm().mr_2().child("Duration"))
                    .when(!notify_on, |this| this.opacity(0.45))
                    .children(durations),
            )
            .child(notify(
                "notify-sound",
                "Play notification sound",
                |n| &mut n.sound,
                notifications.sound,
                notify_on,
                1,
                window,
                cx,
            ))
            .child(notify(
                "notify-open",
                "Show Open Clip action",
                |n| &mut n.open_clip_action,
                notifications.open_clip_action,
                notify_on,
                2,
                window,
                cx,
            ))
            .child(notify(
                "notify-failure",
                "Notify when saving fails",
                |n| &mut n.notify_on_failure,
                notifications.notify_on_failure,
                notify_on,
                3,
                window,
                cx,
            ))
    }

    fn settings_advanced_card(
        &self,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let config_file = config_path().ok();
        let shown = config_file
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Unavailable".into());
        let folder = config_file.and_then(|path| path.parent().map(Path::to_path_buf));
        let perf = &self.config.performance;
        let open = activatable(
            button(
                t,
                "settings-open-config",
                "Open",
                ButtonVariant::Ghost,
                None,
                folder.is_some(),
            ),
            t,
            TAB_ADVANCED,
            window,
            cx,
            move |this, _, cx| this.open_folder(folder.clone(), cx),
        );
        card(t, 7)
            .child(row(
                t,
                "Settings File",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(field(t, shown)))
                    .child(open),
            ))
            .child(row(
                t,
                "Command Queue",
                field(t, format!("{} commands", perf.command_queue_capacity)),
            ))
            .child(row(
                t,
                "Event Queue",
                field(t, format!("{} events", perf.event_queue_capacity)),
            ))
            .child(note(
                t,
                "Queue sizes are read from settings.toml at launch.",
            ))
            .child(div().h(px(1.)).bg(t.colors.border))
            .child(self.diagnostics_section(t, window, cx))
    }

    /// Logging level, log folder and the debug-log export.
    fn diagnostics_section(&self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        use clipper_storage::DetailedLogLevel;
        let diagnostics = &self.config.diagnostics;
        let level = clipper_diagnostics::current_verbosity();
        let logging = level.is_some();
        // Reflects what is in effect (detailed logging expires on its own).
        let detailed = diagnostics
            .active_detailed_level(crate::diagnostics::now_unix())
            .is_some();
        let toggle = checkbox(
            t,
            "settings-detailed-logging",
            "Detailed logging",
            Some(
                "Adds Debug or Trace detail for troubleshooting. Turns itself off after 24 hours.",
            ),
            detailed,
            logging,
            TAB_DIAGNOSTICS,
            window,
            cx,
            |this, on, cx| this.set_detailed_logging(on, cx),
        );
        let levels = [
            (DetailedLogLevel::Debug, "Debug"),
            (DetailedLogLevel::Trace, "Trace"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (choice, label))| {
            activatable(
                chip(
                    t,
                    ("log-level", index),
                    label,
                    diagnostics.detailed_level == choice,
                    detailed,
                ),
                t,
                TAB_DIAGNOSTICS + 1 + index as isize,
                window,
                cx,
                move |this, _, cx| {
                    if this
                        .config
                        .diagnostics
                        .active_detailed_level(crate::diagnostics::now_unix())
                        .is_some()
                    {
                        this.set_detailed_level(choice, cx);
                    }
                },
            )
        })
        .collect::<Vec<_>>();
        let folder = crate::diagnostics::log_directory();
        let shown = folder.display().to_string();
        let open_logs = activatable(
            button(
                t,
                "settings-open-logs",
                "Open",
                ButtonVariant::Ghost,
                None,
                true,
            ),
            t,
            TAB_DIAGNOSTICS + 3,
            window,
            cx,
            move |this, _, cx| {
                // The folder exists once the logger started; create it for
                // the button even when logging is unavailable.
                let _ = std::fs::create_dir_all(&folder);
                this.open_folder(Some(folder.clone()), cx);
            },
        );
        let exporting = self.diagnostics_exporting;
        let export = activatable(
            button(
                t,
                "settings-export-logs",
                if exporting {
                    "Exporting…"
                } else {
                    "Export Debug Logs…"
                },
                ButtonVariant::Secondary,
                None,
                !exporting,
            ),
            t,
            TAB_DIAGNOSTICS + 4,
            window,
            cx,
            |this, window, cx| this.export_debug_logs(window, cx),
        );
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Diagnostics"))
            .child(toggle)
            .child(row(
                t,
                "Detail Level",
                div()
                    .flex()
                    .gap_1p5()
                    .when(!detailed, |this| this.opacity(0.45))
                    .children(levels),
            ))
            .child(row(
                t,
                "Current Level",
                field(
                    t,
                    match level {
                        Some(level) => level.label().to_owned(),
                        None => "File logging unavailable".to_owned(),
                    },
                ),
            ))
            .child(row(
                t,
                "Log Folder",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(field(t, shown)))
                    .child(open_logs),
            ))
            .child(div().flex().items_center().gap_3().child(export).children(
                self.diagnostics_notice.clone().map(|(error, message)| {
                    if error {
                        error_note(t, message).into_any_element()
                    } else {
                        note(t, message).into_any_element()
                    }
                }),
            ))
            .child(note(
                t,
                "Logs stay on this PC and rotate at 5 MB (6 files at most). Default level: Info,                  warnings and errors. Exports redact your user folder and name, contain no clips,                  and are never uploaded.",
            ))
    }

    /// Category list; `compact` shows icons only (narrow windows).
    fn section_nav(
        &self,
        t: &Theme,
        compact: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let items = SECTIONS
            .iter()
            .enumerate()
            .map(|(index, (glyph, title, _))| {
                let active = self.settings_section == index;
                let item = div()
                    .id(("settings-nav", index))
                    .flex()
                    .items_center()
                    .gap_3()
                    .h(px(48.))
                    .px_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .text_base()
                    .cursor_pointer()
                    .map(|this| {
                        if active {
                            // Same active treatment as the app sidebar.
                            this.bg(t.colors.accent.opacity(super::ACTIVE_NAV_ALPHA))
                                .text_color(t.colors.accent_foreground)
                        } else {
                            this.text_color(t.colors.text_secondary)
                                .hover(|style| style.bg(t.colors.surface_hover))
                        }
                    })
                    .child(icon(*glyph, px(18.)))
                    .when(compact, |this| {
                        this.justify_center().px_0().tooltip(tooltip(*title))
                    })
                    .when(!compact, |this| this.child(*title));
                activatable(
                    item,
                    t,
                    TAB_NAV + index as isize,
                    window,
                    cx,
                    move |this, _, cx| this.select_section(index, cx),
                )
            })
            .collect::<Vec<_>>();
        div()
            .flex_none()
            .w(px(if compact { 64. } else { 214. }))
            .h_full()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(t.colors.border)
            .bg(t.colors.surface)
            .children(items)
    }
}

/// A small static mock of the app in the current theme and accent. Plain
/// boxes, drawn only when Settings repaints.
fn live_preview(t: &Theme) -> Div {
    let bar = |width: f32, color| div().h(px(6.)).w(px(width)).rounded_full().bg(color);
    div()
        .flex_none()
        .w(px(240.))
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .rounded_lg()
        .border_1()
        .border_color(t.colors.accent)
        .bg(t.colors.background)
        .child(note(t, "Live Preview"))
        .child(
            div()
                .flex()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .pt_1()
                        .child(bar(36., t.colors.border_strong))
                        .child(bar(30., t.colors.border_strong))
                        .child(bar(44., t.colors.accent))
                        .child(bar(26., t.colors.border_strong)),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(76.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .bg(t.colors.surface_raised)
                        .text_color(t.colors.text)
                        .child("▶"),
                ),
        )
        .child(
            div()
                .flex()
                .gap_1()
                .child(bar(60., t.colors.border_strong))
                .child(bar(150., t.colors.accent)),
        )
}

pub(super) fn content(
    window: &ClipperWindow,
    win: &mut Window,
    cx: &mut Context<ClipperWindow>,
) -> Div {
    let t = *cx.theme();
    let compact_nav = win.viewport_size().width < px(NAV_MIN_VIEWPORT);
    let scroll = crate::components::smooth_scroll(
        div().id("settings-scroll"),
        "settings-smooth-scroll",
        &window.settings_scroll,
        win,
        cx,
    );
    // Only the selected category is built and laid out: switching shows it
    // alone, and scrolling does not rebuild the other seven cards.
    let grid = div()
        .flex()
        .flex_col()
        .gap(t.metrics.gap * 2.)
        .children(
            window
                .recording
                .error
                .clone()
                .map(|message| super::dashboard::error_banner(message, cx)),
        )
        .child(window.section_card(&t, win, cx));
    div().size_full().flex().flex_col().gap_4().child(
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(t.metrics.gap * 2.)
            .child(window.section_nav(&t, compact_nav, win, cx))
            .child(
                scroll
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(grid.pb_4()),
            ),
    )
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

    #[test]
    fn preset_options_select_matching_preset() {
        let (options, selected) = preset_options(&REPLAY_MEMORY_PRESETS_MB, 128);
        assert_eq!(options.len(), REPLAY_MEMORY_PRESETS_MB.len());
        assert_eq!(options[selected].label.as_ref(), "128 MB");
    }

    #[test]
    fn preset_options_keep_a_hand_edited_value() {
        let (options, selected) = preset_options(&REPLAY_DISK_PRESETS_MB, 3000);
        assert_eq!(selected, REPLAY_DISK_PRESETS_MB.len());
        assert_eq!(options[selected].label.as_ref(), "3000 MB");
        assert_eq!(size_label(4096), "4 GB");
    }

    #[test]
    fn clip_directory_must_be_absolute_and_writable() {
        assert!(prepare_clip_directory(Path::new("relative/clips")).is_err());
        let dir = std::env::temp_dir().join(format!("clipuh-settings-test-{}", std::process::id()));
        let nested = dir.join("a").join("b");
        assert!(prepare_clip_directory(&nested).is_ok());
        assert!(nested.is_dir());
        assert!(!nested.join(".clipuh-write-test").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clip_directory_accepts_spaces_and_unicode() {
        let dir = std::env::temp_dir().join(format!(
            "clipuh settings Ünïcode 片段 {}",
            std::process::id()
        ));
        assert!(prepare_clip_directory(&dir).is_ok());
        assert!(dir.is_dir());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clip_directory_rejects_files_and_unavailable_drives() {
        let file = std::env::temp_dir().join(format!("clipuh-not-a-folder-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let error = prepare_clip_directory(&file).unwrap_err();
        assert!(error.contains("Cannot"), "{error}");
        let _ = std::fs::remove_file(&file);
        // A drive letter with no volume behind it.
        let missing = ('D'..='Z')
            .map(|letter| PathBuf::from(format!("{letter}:\\")))
            .find(|root| !root.exists());
        if let Some(root) = missing {
            let error = prepare_clip_directory(&root.join("Clips")).unwrap_err();
            assert!(error.contains("Cannot create"), "{error}");
        }
    }
}
