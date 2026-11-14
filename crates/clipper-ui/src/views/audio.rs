//! Audio Mixer card of the Capture page. Device and application enumeration run off the
//! UI thread; state shown per row comes only from engine events.

use std::{cell::Cell, rc::Rc, time::Duration};

use clipper_audio::DeviceFlow;
use clipper_audio::windows::{
    AudioApplication, capabilities, list_audio_applications, list_devices,
};
use clipper_core::recording::{
    AudioSourceState, AudioSourceStatus, MAX_AUDIO_GAIN_PERCENT, MAX_RECORDED_AUDIO_SOURCES,
    RecorderCommand, SourceLevel,
};
use clipper_storage::{AudioSourceSettings, AudioSourceType, MAX_AUDIO_SOURCES};
use gpui::prelude::*;
use gpui::{
    Bounds, Context, Div, FocusHandle, MouseButton, Pixels, SharedString, Window, anchored, canvas,
    deferred, div, px, relative,
};

use super::{ClipperWindow, Confirm, Dismiss, SelectNext, SelectPrevious};
use crate::components::{
    ButtonVariant, DropdownOption, Tone, button, card, dropdown, slider, status_dot, step, toggle,
    tooltip,
};
use crate::theme::{ActiveTheme, Theme};

/// Key context of the Add Source popup; Escape closes it.
pub(super) const ADD_CONTEXT: &str = "AudioAdd";
/// Focus-map key of the "Separate tracks" toggle (not a source id).
const TRACKS_KEY: u32 = u32::MAX;
/// Meter redraws are limited to 10 Hz.
const LEVEL_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// Smaller peak changes are invisible on a 6 px bar.
const LEVEL_EPSILON: f32 = 0.02;
const NO_LOOPBACK: &str = "Needs Windows 11 or Windows 10 build 20348+";

/// Keyboard focus of one mixer row.
#[derive(Clone)]
pub(super) struct RowFocus {
    menu: FocusHandle,
    slider: FocusHandle,
    toggle: FocusHandle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AddStep {
    Category,
    Targets(AudioSourceType),
}

// ---- pure helpers -------------------------------------------------------------------

/// Dot tone, text shown instead of a meter, and whether a meter is shown.
fn row_status(
    enabled: bool,
    status: Option<&AudioSourceStatus>,
    unavailable: Option<&str>,
) -> (Tone, Option<String>, bool) {
    if !enabled {
        return (Tone::Neutral, Some("Off".into()), false);
    }
    if let Some(reason) = unavailable {
        return (Tone::Warning, Some(reason.into()), false);
    }
    match status {
        Some(AudioSourceStatus::Capturing { .. }) => (Tone::Success, None, true),
        Some(AudioSourceStatus::Starting) => (Tone::Warning, Some("Starting…".into()), false),
        Some(AudioSourceStatus::Waiting(m)) => (Tone::Warning, Some(m.clone()), false),
        Some(AudioSourceStatus::Skipped(m)) => (Tone::Neutral, Some(m.clone()), false),
        Some(AudioSourceStatus::Failed(m)) => (Tone::Danger, Some(m.clone()), false),
        None => (Tone::Neutral, Some("Not capturing".into()), false),
    }
}

/// Why a source cannot be captured on this machine right now, if known.
/// `apps` is `None` until the application list has loaded.
fn row_unavailable(
    kind: AudioSourceType,
    target: Option<&str>,
    loopback: bool,
    apps: Option<&[AudioApplication]>,
    device_known: Option<bool>,
    has_engine_state: bool,
) -> Option<&'static str> {
    match kind {
        AudioSourceType::Game if !loopback => Some(NO_LOOPBACK),
        AudioSourceType::Application if !loopback => Some(NO_LOOPBACK),
        AudioSourceType::Application if !has_engine_state => {
            let (apps, target) = (apps?, target?);
            (!apps
                .iter()
                .any(|a| a.executable.eq_ignore_ascii_case(target)))
            .then_some("Application not running")
        }
        AudioSourceType::Desktop | AudioSourceType::Microphone
            if target.is_some() && device_known == Some(false) =>
        {
            Some("Device not available")
        }
        _ => None,
    }
}

/// Whether a meter update is worth a redraw: the source set changed, a peak moved by at
/// least `LEVEL_EPSILON`, and at least `LEVEL_MIN_INTERVAL` passed since the last redraw.
pub(super) fn levels_need_redraw(
    shown: &[SourceLevel],
    next: &[SourceLevel],
    since_last: Option<Duration>,
) -> bool {
    let sources_differ =
        shown.len() != next.len() || shown.iter().zip(next).any(|(a, b)| a.source != b.source);
    if sources_differ {
        return true;
    }
    if since_last.is_some_and(|d| d < LEVEL_MIN_INTERVAL) {
        return false;
    }
    shown
        .iter()
        .zip(next)
        .any(|(a, b)| (a.peak - b.peak).abs() >= LEVEL_EPSILON)
}

/// Name and subtitle of a row. `resolved` is the friendly application name or the device name.
fn row_labels(
    kind: AudioSourceType,
    target: Option<&str>,
    resolved: Option<&str>,
) -> (String, String) {
    match kind {
        AudioSourceType::Game => ("Game Audio".into(), "Follows the capture source".into()),
        AudioSourceType::Application => {
            let exe = target.unwrap_or("Unknown application");
            (resolved.unwrap_or(exe).into(), exe.into())
        }
        AudioSourceType::Desktop => (
            "Desktop Audio".into(),
            device_label(target, resolved, "Default output"),
        ),
        AudioSourceType::Microphone => (
            "Microphone".into(),
            device_label(target, resolved, "Default input"),
        ),
    }
}

fn device_label(target: Option<&str>, resolved: Option<&str>, default: &str) -> String {
    match (target, resolved) {
        (None, _) => default.into(),
        (Some(_), Some(name)) => name.into(),
        (Some(_), None) => "Unavailable device".into(),
    }
}

/// Routing note for the desktop row; `names` are the enabled game/application sources.
fn overlap_note(names: &[String]) -> Option<String> {
    match names {
        [] => None,
        [one] => Some(format!(
            "Records desktop audio without {one} to avoid double audio"
        )),
        _ => Some("Skipped while 2+ apps are recorded: Windows can exclude only one app".into()),
    }
}

/// Whether the enabled set differs from what the running session captures.
fn topology_pending(enabled_ids: &[u32], running: &[AudioSourceState]) -> bool {
    if running.is_empty() {
        return false;
    }
    let mut a = enabled_ids.to_vec();
    let mut b: Vec<u32> = running.iter().map(|s| s.source).collect();
    a.sort_unstable();
    b.sort_unstable();
    a != b
}

fn gain_to_value(percent: u16) -> f32 {
    f32::from(percent) / f32::from(MAX_AUDIO_GAIN_PERCENT)
}

fn value_to_gain(value: f32) -> u16 {
    (value.clamp(0.0, 1.0) * f32::from(MAX_AUDIO_GAIN_PERCENT)).round() as u16
}

fn kind_label(kind: AudioSourceType) -> &'static str {
    match kind {
        AudioSourceType::Game => "Game Audio",
        AudioSourceType::Application => "Application",
        AudioSourceType::Desktop => "Desktop Audio",
        AudioSourceType::Microphone => "Microphone",
    }
}

// ---- state ----------------------------------------------------------------------------

impl ClipperWindow {
    fn row_focus(&self, id: u32, cx: &mut Context<Self>) -> RowFocus {
        self.audio_focus
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| RowFocus {
                menu: cx.focus_handle().tab_index(12).tab_stop(true),
                slider: cx.focus_handle().tab_index(12).tab_stop(true),
                toggle: cx.focus_handle().tab_index(12).tab_stop(true),
            })
            .clone()
    }

    pub(super) fn snapshot_audio_focus(&self, window: &Window) {
        let mut set = self.audio_focused.borrow_mut();
        set.clear();
        for (id, f) in self.audio_focus.borrow().iter() {
            for (slot, h) in [&f.menu, &f.slider, &f.toggle].into_iter().enumerate() {
                if h.is_focused(window) {
                    set.insert((*id, slot as u8));
                }
            }
        }
    }

    /// `slot` is 0 menu, 1 slider, 2 toggle (the `RowFocus` fields).
    fn is_focused(&self, row: u32, slot: u8) -> bool {
        self.audio_focused.borrow().contains(&(row, slot))
    }

    fn audio_source(&self, id: u32) -> Option<&AudioSourceSettings> {
        self.config.audio.sources.iter().find(|s| s.id == id)
    }

    fn audio_enabled_count(&self) -> usize {
        self.config
            .audio
            .sources
            .iter()
            .filter(|s| s.enabled)
            .count()
    }

    /// Loads devices and (where supported) audio applications off-thread.
    pub(super) fn refresh_audio_lists(&mut self, cx: &mut Context<Self>) {
        if self.audio_loading {
            return;
        }
        self.audio_loading = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async {
                    let caps = capabilities();
                    let mut devices =
                        list_devices(DeviceFlow::Capture).map_err(|e| e.to_string())?;
                    devices.extend(list_devices(DeviceFlow::Render).map_err(|e| e.to_string())?);
                    let apps = if caps.process_loopback {
                        list_audio_applications().unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    Ok::<_, String>((caps, devices, apps))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.audio_loading = false;
                match result {
                    Ok((caps, devices, apps)) => {
                        this.audio_capabilities = Some(caps);
                        this.audio_devices = devices;
                        this.audio_apps = apps;
                    }
                    Err(message) => this.recording.fail(message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn process_loopback(&self) -> bool {
        self.audio_capabilities
            .as_ref()
            .is_none_or(|c| c.process_loopback)
    }

    fn resolved_name(&self, source: &AudioSourceSettings) -> Option<String> {
        let target = source.target.as_deref()?;
        match source.kind {
            AudioSourceType::Application => self
                .audio_apps
                .iter()
                .find(|a| a.executable.eq_ignore_ascii_case(target))
                .map(|a| a.name.clone()),
            AudioSourceType::Desktop | AudioSourceType::Microphone => self
                .audio_devices
                .iter()
                .find(|d| d.id == target)
                .map(|d| d.name.clone()),
            AudioSourceType::Game => None,
        }
    }

    // gain

    fn audio_set_gain(&mut self, id: u32, value: f32, cx: &mut Context<Self>) {
        let gain_percent = value_to_gain(value);
        let Some(source) = self.config.audio.sources.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if source.gain_percent == gain_percent {
            return;
        }
        source.gain_percent = gain_percent;
        self.audio_gain_dirty = true;
        self.send(RecorderCommand::SetAudioGain {
            source: id,
            gain_percent,
        });
        cx.notify();
    }

    /// Saves a changed gain once the drag or key press is over.
    fn audio_commit_gain(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.audio_gain_dirty) {
            self.persist_config(cx);
        }
    }

    // rows

    fn audio_toggle(&mut self, id: u32, on: bool, cx: &mut Context<Self>) {
        if on && self.audio_enabled_count() >= MAX_RECORDED_AUDIO_SOURCES {
            self.audio_notice = Some(format!(
                "At most {MAX_RECORDED_AUDIO_SOURCES} sources can be recorded at once. Turn another one off first."
            ));
            cx.notify();
            return;
        }
        if let Some(source) = self.config.audio.sources.iter_mut().find(|s| s.id == id) {
            source.enabled = on;
            self.audio_notice = None;
            self.persist_config(cx);
        }
    }

    fn audio_remove(&mut self, id: u32, cx: &mut Context<Self>) {
        self.config.audio.sources.retain(|s| s.id != id);
        self.audio_focus.borrow_mut().remove(&id);
        if self.audio_menu.is_some_and(|(open, _)| open == id) {
            self.audio_menu = None;
        }
        self.audio_notice = None;
        self.persist_config(cx);
    }

    // target dropdown

    /// Dropdown rows, the target each row stands for and the selected row.
    /// `None` for rows without a target choice (Game).
    fn audio_target_options(
        &self,
        id: u32,
    ) -> Option<(Vec<DropdownOption>, Vec<Option<String>>, usize)> {
        let source = self.audio_source(id)?;
        let plain = |label: String, detail: Option<String>| DropdownOption {
            label: label.into(),
            detail: detail.map(Into::into),
            enabled: true,
            reason: None,
        };
        let mut options = Vec::new();
        let mut values: Vec<Option<String>> = Vec::new();
        match source.kind {
            AudioSourceType::Game => return None,
            AudioSourceType::Desktop | AudioSourceType::Microphone => {
                let (flow, default) = if source.kind == AudioSourceType::Desktop {
                    (DeviceFlow::Render, "Default output")
                } else {
                    (DeviceFlow::Capture, "Default input")
                };
                options.push(plain(default.into(), None));
                values.push(None);
                for d in self.audio_devices.iter().filter(|d| d.flow == flow) {
                    options.push(plain(d.name.clone(), None));
                    values.push(Some(d.id.clone()));
                }
                if let Some(t) = &source.target
                    && !values.contains(&source.target)
                {
                    options.push(plain("Unavailable device".into(), Some(t.clone())));
                    values.push(source.target.clone());
                }
            }
            AudioSourceType::Application => {
                for a in &self.audio_apps {
                    options.push(plain(a.name.clone(), Some(a.executable.clone())));
                    values.push(Some(a.executable.clone()));
                }
                if let Some(t) = &source.target
                    && !self
                        .audio_apps
                        .iter()
                        .any(|a| a.executable.eq_ignore_ascii_case(t))
                {
                    options.push(plain(format!("{t} (not running)"), None));
                    values.push(Some(t.clone()));
                }
            }
        }
        let selected = values
            .iter()
            .position(|v| match (v, &source.target) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                (None, None) => true,
                _ => false,
            })
            .unwrap_or(0);
        Some((options, values, selected))
    }

    fn audio_menu_toggle(&mut self, id: u32, cx: &mut Context<Self>) {
        if self.audio_menu.is_some_and(|(open, _)| open == id) {
            self.audio_menu = None;
        } else if let Some((_, _, selected)) = self.audio_target_options(id) {
            self.audio_menu = Some((id, selected));
            self.refresh_audio_lists(cx);
        }
        cx.notify();
    }

    fn audio_menu_close(&mut self, cx: &mut Context<Self>) {
        if self.audio_menu.take().is_some() {
            cx.notify();
        }
    }

    fn audio_menu_apply(&mut self, id: u32, index: usize, cx: &mut Context<Self>) {
        self.audio_menu = None;
        let Some((_, values, _)) = self.audio_target_options(id) else {
            return;
        };
        if let Some(value) = values.get(index).cloned()
            && let Some(source) = self.config.audio.sources.iter_mut().find(|s| s.id == id)
        {
            source.target = value;
            self.persist_config(cx);
        }
        cx.notify();
    }

    fn audio_menu_confirm(&mut self, id: u32, cx: &mut Context<Self>) {
        match self.audio_menu {
            Some((open, highlighted)) if open == id => self.audio_menu_apply(id, highlighted, cx),
            _ => self.audio_menu_toggle(id, cx),
        }
    }

    fn audio_menu_step(&mut self, id: u32, delta: isize, cx: &mut Context<Self>) {
        let Some((open, from)) = self.audio_menu.filter(|(open, _)| *open == id) else {
            self.audio_menu_toggle(id, cx);
            return;
        };
        if let Some((options, _, _)) = self.audio_target_options(open) {
            let next = step(&options, from, delta);
            if next != from {
                self.audio_menu = Some((open, next));
                cx.notify();
            }
        }
    }

    // add popup

    fn audio_add_toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.audio_add.take().is_none() {
            self.audio_add = Some(AddStep::Category);
            self.audio_add_note = None;
            self.audio_menu = None;
            window.focus(&self.audio_add_focus);
            self.refresh_audio_lists(cx);
        }
        cx.notify();
    }

    fn audio_add_close(&mut self, cx: &mut Context<Self>) {
        if self.audio_add.take().is_some() {
            cx.notify();
        }
    }

    fn audio_add_source(
        &mut self,
        kind: AudioSourceType,
        target: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.config.audio.sources.len() >= MAX_AUDIO_SOURCES {
            self.audio_add_note = Some(format!(
                "The mixer holds at most {MAX_AUDIO_SOURCES} sources"
            ));
            cx.notify();
            return;
        }
        if self
            .config
            .audio
            .duplicate_of(kind, target.as_deref())
            .is_some()
        {
            self.audio_add_note = Some("Already in the mixer".into());
            cx.notify();
            return;
        }
        let enabled = self.audio_enabled_count() < MAX_RECORDED_AUDIO_SOURCES;
        self.audio_notice = (!enabled).then(|| {
            format!(
                "Added turned off: at most {MAX_RECORDED_AUDIO_SOURCES} sources can be recorded at once."
            )
        });
        let id = self.config.audio.next_source_id();
        self.config.audio.sources.push(AudioSourceSettings {
            id,
            kind,
            target,
            enabled,
            gain_percent: 100,
        });
        self.audio_add = None;
        self.persist_config(cx);
    }

    fn start_audio_test(&mut self, cx: &mut Context<Self>) {
        let options = match self.config.recording_options() {
            Ok(options) => options,
            Err(error) => {
                self.recording.fail(error.to_string());
                cx.notify();
                return;
            }
        };
        if self.send(RecorderCommand::StartAudioTest {
            options: options.audio,
            output_directory: options.output_directory,
        }) {
            self.audio_testing = true;
            self.audio_test_path = None;
            cx.notify();
        }
    }
}

// ---- view -----------------------------------------------------------------------------

fn muted(t: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .text_color(t.colors.text_muted)
        .child(text.into())
}

fn popup_row(
    t: &Theme,
    id: (&'static str, usize),
    label: impl Into<SharedString>,
    detail: Option<String>,
    reason: Option<&'static str>,
) -> gpui::Stateful<Div> {
    let enabled = reason.is_none();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_2()
        .py_1()
        .rounded_md()
        .text_sm()
        .text_color(if enabled {
            t.colors.text
        } else {
            t.colors.text_muted
        })
        .child(div().child(label.into()))
        .children(
            detail
                .or(reason.map(str::to_owned))
                .map(|d| div().text_xs().text_color(t.colors.text_muted).child(d)),
        )
        .when(enabled, |r| {
            let hover = t.colors.surface_hover;
            r.cursor_pointer().hover(move |s| s.bg(hover))
        })
}

pub(super) fn content(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let t = *cx.theme();
    let entity = cx.entity();
    let sources = &window.config.audio.sources;
    let at_max = sources.len() >= MAX_AUDIO_SOURCES;

    // Add Source button + popup.
    let trigger_bounds: Rc<Cell<Bounds<Pixels>>> = Rc::default();
    let add_open = window.audio_add.is_some();
    let add_button = button(
        &t,
        "audio-add",
        "Add Source",
        ButtonVariant::Primary,
        None,
        !at_max,
    )
    .when(!at_max, |b| {
        b.on_click(cx.listener(|this, _, window, cx| this.audio_add_toggle(window, cx)))
    });
    let popup = window.audio_add.map(|step| {
        let out = trigger_bounds.clone();
        let mut list = div()
            .id("audio-add-menu")
            .occlude()
            .flex()
            .flex_col()
            .gap_0p5()
            .w(px(280.))
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(t.colors.border_strong)
            .bg(t.colors.surface)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(move |this, _, window, cx| {
                if !out.get().contains(&window.mouse_position()) {
                    this.audio_add_close(cx);
                }
            }));
        match step {
            AddStep::Category => {
                let loopback = window.process_loopback();
                for (i, kind) in [
                    AudioSourceType::Game,
                    AudioSourceType::Application,
                    AudioSourceType::Desktop,
                    AudioSourceType::Microphone,
                ]
                .into_iter()
                .enumerate()
                {
                    let needs =
                        matches!(kind, AudioSourceType::Game | AudioSourceType::Application);
                    let reason = (needs && !loopback).then_some(NO_LOOPBACK);
                    let row = popup_row(&t, ("audio-cat", i), kind_label(kind), None, reason);
                    list = list.child(row.when(reason.is_none(), |r| {
                        r.on_click(cx.listener(move |this, _, _, cx| {
                            if kind == AudioSourceType::Game {
                                this.audio_add_source(kind, None, cx);
                            } else {
                                this.audio_add = Some(AddStep::Targets(kind));
                                this.audio_add_note = None;
                                cx.notify();
                            }
                        }))
                    }));
                }
            }
            AddStep::Targets(kind) => {
                let header = div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        button(
                            &t,
                            "audio-add-back",
                            "‹ Back",
                            ButtonVariant::Ghost,
                            None,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.audio_add = Some(AddStep::Category);
                            this.audio_add_note = None;
                            cx.notify();
                        })),
                    )
                    .child(
                        button(
                            &t,
                            "audio-add-refresh",
                            if window.audio_loading {
                                "Loading…"
                            } else {
                                "Refresh"
                            },
                            ButtonVariant::Ghost,
                            None,
                            !window.audio_loading,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.refresh_audio_lists(cx);
                            cx.notify();
                        })),
                    );
                list = list.child(header);
                let targets: Vec<(String, Option<String>, Option<String>)> = match kind {
                    AudioSourceType::Application => window
                        .audio_apps
                        .iter()
                        .map(|a: &AudioApplication| {
                            (
                                a.name.clone(),
                                Some(a.executable.clone()),
                                Some(a.executable.clone()),
                            )
                        })
                        .collect(),
                    flow_kind => {
                        let (flow, default) = if flow_kind == AudioSourceType::Desktop {
                            (DeviceFlow::Render, "Default output")
                        } else {
                            (DeviceFlow::Capture, "Default input")
                        };
                        std::iter::once((default.to_owned(), None, None))
                            .chain(
                                window
                                    .audio_devices
                                    .iter()
                                    .filter(|d| d.flow == flow)
                                    .map(|d| (d.name.clone(), None, Some(d.id.clone()))),
                            )
                            .collect()
                    }
                };
                if window.audio_loading && targets.len() <= 1 {
                    list = list.child(muted(&t, "Loading…").px_2().py_1());
                } else if targets.is_empty() {
                    list = list.child(
                        muted(&t, "No applications are playing audio right now.")
                            .px_2()
                            .py_1(),
                    );
                }
                for (i, (label, detail, target)) in targets.into_iter().enumerate() {
                    list = list.child(
                        popup_row(&t, ("audio-target", i), label, detail, None).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.audio_add_source(kind, target.clone(), cx)
                            }),
                        ),
                    );
                }
            }
        }
        if let Some(note) = &window.audio_add_note {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(t.colors.warning)
                    .child(note.clone()),
            );
        }
        deferred(anchored().snap_to_window_with_margin(px(8.)).child(list))
    });
    let add = div()
        .id("audio-add-wrap")
        .relative()
        .track_focus(&window.audio_add_focus)
        .key_context(ADD_CONTEXT)
        .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.audio_add_close(cx)))
        .child(
            div().relative().child(add_button).child(
                canvas(
                    {
                        let out = trigger_bounds.clone();
                        move |b, _, _| out.set(b)
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            ),
        )
        .when(add_open, |this| {
            this.children(popup.map(|p| {
                div()
                    .absolute()
                    .top(t.metrics.control_height + px(4.))
                    .right_0()
                    .size_0()
                    .child(p)
            }))
        });

    // Rows.
    let overlap_names: Vec<String> = sources
        .iter()
        .filter(|s| {
            s.enabled && matches!(s.kind, AudioSourceType::Game | AudioSourceType::Application)
        })
        .map(|s| match s.kind {
            AudioSourceType::Application => {
                row_labels(
                    s.kind,
                    s.target.as_deref(),
                    window.resolved_name(s).as_deref(),
                )
                .0
            }
            _ => "the game".into(),
        })
        .collect();
    let enabled_ids: Vec<u32> = sources.iter().filter(|s| s.enabled).map(|s| s.id).collect();
    let pending = topology_pending(&enabled_ids, &window.audio_sources);

    let at_enabled_max = enabled_ids.len() >= MAX_RECORDED_AUDIO_SOURCES;
    let mut rows = div().flex().flex_col();
    for (index, source) in sources.iter().enumerate() {
        let id = source.id;
        let focus = window.row_focus(id, cx);
        let resolved = window.resolved_name(source);
        let (name, subtitle) =
            row_labels(source.kind, source.target.as_deref(), resolved.as_deref());
        let state = window.audio_sources.iter().find(|s| s.source == id);
        let lists_loaded = window.audio_capabilities.is_some() && !window.audio_loading;
        let device_known = (lists_loaded
            && matches!(
                source.kind,
                AudioSourceType::Desktop | AudioSourceType::Microphone
            ))
        .then(|| {
            source
                .target
                .as_deref()
                .is_none_or(|t| window.audio_devices.iter().any(|d| d.id == t))
        });
        let unavailable = row_unavailable(
            source.kind,
            source.target.as_deref(),
            window.process_loopback(),
            lists_loaded.then_some(window.audio_apps.as_slice()),
            device_known,
            state.is_some(),
        );
        let (tone, text, meter) = row_status(source.enabled, state.map(|s| &s.status), unavailable);
        let peak = window
            .audio_levels
            .iter()
            .find(|l| l.source == id)
            .map_or(0.0, |l| l.peak.clamp(0.0, 1.0));
        let overlap = (source.kind == AudioSourceType::Desktop && source.enabled)
            .then(|| overlap_note(&overlap_names))
            .flatten();

        let target_cell = match window.audio_target_options(id) {
            Some((options, _, selected)) => {
                let (a, b, c) = (entity.clone(), entity.clone(), entity.clone());
                div()
                    .on_action(
                        cx.listener(move |this, _: &Confirm, _, cx| {
                            this.audio_menu_confirm(id, cx)
                        }),
                    )
                    .on_action(cx.listener(move |this, _: &SelectNext, _, cx| {
                        this.audio_menu_step(id, 1, cx)
                    }))
                    .on_action(cx.listener(move |this, _: &SelectPrevious, _, cx| {
                        this.audio_menu_step(id, -1, cx)
                    }))
                    .on_action(
                        cx.listener(move |this, _: &Dismiss, _, cx| this.audio_menu_close(cx)),
                    )
                    .child(dropdown(
                        &t,
                        ("audio-target-menu", id as usize),
                        &options,
                        selected,
                        window
                            .audio_menu
                            .filter(|(open, _)| *open == id)
                            .map(|(_, h)| h),
                        &focus.menu,
                        window.is_focused(id, 0),
                        true,
                        move |_, app| a.update(app, |this, cx| this.audio_menu_toggle(id, cx)),
                        move |i, _, app| b.update(app, |this, cx| this.audio_menu_apply(id, i, cx)),
                        move |_, app| c.update(app, |this, cx| this.audio_menu_close(cx)),
                    ))
                    .into_any_element()
            }
            None => muted(&t, "Follows the capture source").into_any_element(),
        };

        let status_cell = if meter {
            div()
                .w_full()
                .h(px(6.))
                .rounded_full()
                .bg(t.colors.border)
                .child(
                    div()
                        .h_full()
                        .rounded_full()
                        .w(relative(peak))
                        .bg(t.colors.accent),
                )
                .into_any_element()
        } else {
            let text = text.unwrap_or_default();
            div()
                .id(("audio-status", id as usize))
                .w_full()
                .truncate()
                .text_xs()
                .text_color(if tone == Tone::Danger {
                    t.colors.danger
                } else {
                    t.colors.text_muted
                })
                .tooltip(tooltip(text.clone()))
                .child(text)
                .into_any_element()
        };

        let (e1, e2, e3) = (entity.clone(), entity.clone(), entity.clone());
        let gain = source.gain_percent;
        let slider_cell = div()
            .flex()
            .items_center()
            .gap_2()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.audio_commit_gain(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.audio_commit_gain(cx)),
            )
            .on_key_up(cx.listener(|this, _, _, cx| this.audio_commit_gain(cx)))
            .child(div().w(px(110.)).child(slider(
                &t,
                ("audio-gain", id as usize),
                gain_to_value(gain),
                true,
                &focus.slider,
                window.is_focused(id, 1),
                move |v, _, app| e1.update(app, |this, cx| this.audio_set_gain(id, v, cx)),
            )))
            .child(
                div()
                    .w(px(44.))
                    .text_xs()
                    .text_color(t.colors.text_secondary)
                    .child(format!("{gain} %")),
            );

        rows = rows.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_h(px(40.))
                .py_1()
                .when(index > 0, |r| r.border_t_1().border_color(t.colors.border))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w(px(120.))
                        .child(status_dot(&t, tone))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(div().text_sm().truncate().child(name))
                                .child(muted(&t, subtitle).truncate())
                                .children(overlap.map(|note| {
                                    div().text_xs().text_color(t.colors.warning).child(note)
                                })),
                        ),
                )
                .child(div().w(px(180.)).flex_none().child(target_cell))
                .child(div().w(px(100.)).flex_none().child(status_cell))
                .child(slider_cell)
                .child(div().flex_none().child(toggle(
                    &t,
                    ("audio-on", id as usize),
                    source.enabled,
                    source.enabled || (!at_enabled_max && unavailable.is_none()),
                    &focus.toggle,
                    window.is_focused(id, 2),
                    move |on, _, app| e2.update(app, |this, cx| this.audio_toggle(id, on, cx)),
                )))
                .child(
                    button(
                        &t,
                        ("audio-remove", id as usize),
                        "×",
                        ButtonVariant::Ghost,
                        None,
                        true,
                    )
                    .tooltip(tooltip("Remove source"))
                    .on_click(move |_, _, app| {
                        e3.update(app, |this, cx| this.audio_remove(id, cx))
                    }),
                ),
        );
    }
    if sources.is_empty() {
        rows = rows.child(muted(&t, "No audio sources. Add one to record sound.").py_2());
    }

    // Footer: separate tracks + audio test.
    let tracks_focus = window.row_focus(TRACKS_KEY, cx);
    let e = entity.clone();
    let can_test =
        window.recording.settings_enabled() && !window.audio_testing && !enabled_ids.is_empty();
    let test = if window.audio_testing {
        button(
            &t,
            "stop-audio-test",
            "Stop audio test",
            ButtonVariant::Danger,
            None,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.send(RecorderCommand::StopAudioTest);
            cx.notify();
        }))
    } else {
        button(
            &t,
            "audio-test",
            "Test 30 s",
            ButtonVariant::Secondary,
            None,
            can_test,
        )
        .when(can_test, |b| {
            b.on_click(cx.listener(|this, _, _, cx| this.start_audio_test(cx)))
        })
    };
    let footer = div()
        .flex()
        .items_center()
        .gap_3()
        .pt_2()
        .border_t_1()
        .border_color(t.colors.border)
        .child(toggle(
            &t,
            "audio-separate",
            window.config.audio.separate_tracks,
            true,
            &tracks_focus.toggle,
            window.is_focused(TRACKS_KEY, 2),
            move |on, _, app| {
                e.update(app, |this, cx| {
                    this.config.audio.separate_tracks = on;
                    this.persist_config(cx);
                })
            },
        ))
        .child(div().text_sm().child("Separate tracks"))
        .child(
            div()
                .flex_1()
                .child(muted(&t, "Mix + one track per source")),
        )
        .child(test);

    card(&t, Some("Audio Mixer".into()), Some(add.into_any_element()))
        .child(muted(&t, "Manage your recording sources"))
        .children(pending.then(|| {
            muted(
                &t,
                "Applies to the next recording or replay.",
            )
        }))
        .children((at_enabled_max && sources.len() > enabled_ids.len()).then(|| {
            muted(
                &t,
                format!(
                    "{MAX_RECORDED_AUDIO_SOURCES} sources are on, the maximum. Turn one off to enable another."
                ),
            )
        }))
        .children(window.audio_notice.as_ref().map(|n| {
            div()
                .text_xs()
                .text_color(t.colors.warning)
                .child(n.clone())
        }))
        .child(rows)
        .child(footer)
        .children(
            window
                .audio_test_path
                .as_ref()
                .map(|path| muted(&t, format!("Test WAV saved: {path}"))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        use AudioSourceStatus::*;
        assert_eq!(
            row_status(false, Some(&Capturing { track: None }), None)
                .1
                .as_deref(),
            Some("Off")
        );
        assert_eq!(
            row_status(true, None, None),
            (Tone::Neutral, Some("Not capturing".into()), false)
        );
        assert_eq!(
            row_status(true, Some(&Capturing { track: Some(1) }), None),
            (Tone::Success, None, true)
        );
        assert_eq!(row_status(true, Some(&Starting), None).0, Tone::Warning);
        assert_eq!(
            row_status(true, Some(&Waiting("x".into())), None).0,
            Tone::Warning
        );
        assert_eq!(
            row_status(true, Some(&Skipped("x".into())), None).0,
            Tone::Neutral
        );
        assert_eq!(
            row_status(true, Some(&Failed("x".into())), None),
            (Tone::Danger, Some("x".into()), false)
        );
    }

    #[test]
    fn unavailable_overrides_status() {
        let r = row_status(true, Some(&AudioSourceStatus::Starting), Some("nope"));
        assert_eq!(r, (Tone::Warning, Some("nope".into()), false));
        assert_eq!(
            row_status(false, None, Some("nope")).1.as_deref(),
            Some("Off")
        );
    }

    #[test]
    fn unavailability_reasons() {
        use AudioSourceType::*;
        assert_eq!(
            row_unavailable(Game, None, false, None, None, false),
            Some(NO_LOOPBACK)
        );
        assert_eq!(row_unavailable(Game, None, true, None, None, false), None);
        assert_eq!(
            row_unavailable(Application, Some("a.exe"), false, None, None, false),
            Some(NO_LOOPBACK)
        );
        // List not loaded yet: no claim either way.
        assert_eq!(
            row_unavailable(Application, Some("a.exe"), true, None, None, false),
            None
        );
        assert_eq!(
            row_unavailable(Application, Some("a.exe"), true, Some(&[]), None, false),
            Some("Application not running")
        );
        // Engine state wins over the list.
        assert_eq!(
            row_unavailable(Application, Some("a.exe"), true, Some(&[]), None, true),
            None
        );
        assert_eq!(
            row_unavailable(Microphone, Some("id"), true, None, Some(false), false),
            Some("Device not available")
        );
        assert_eq!(
            row_unavailable(Microphone, None, true, None, Some(false), false),
            None
        );
    }

    #[test]
    fn level_redraw_throttle() {
        let l = |source, peak| SourceLevel { source, peak };
        let ms = |n| Some(Duration::from_millis(n));
        assert!(!levels_need_redraw(&[], &[], ms(1000)));
        assert!(levels_need_redraw(&[], &[l(1, 0.0)], ms(1)));
        assert!(levels_need_redraw(&[l(1, 0.0)], &[l(1, 0.5)], None));
        assert!(levels_need_redraw(&[l(1, 0.0)], &[l(1, 0.5)], ms(100)));
        assert!(!levels_need_redraw(&[l(1, 0.0)], &[l(1, 0.5)], ms(99)));
        assert!(!levels_need_redraw(&[l(1, 0.50)], &[l(1, 0.51)], ms(500)));
        assert!(levels_need_redraw(&[l(1, 0.5)], &[l(2, 0.5)], ms(0)));
    }

    #[test]
    fn overlap_messages() {
        assert_eq!(overlap_note(&[]), None);
        assert_eq!(
            overlap_note(&["Game.exe".into()]).unwrap(),
            "Records desktop audio without Game.exe to avoid double audio"
        );
        assert!(
            overlap_note(&["a".into(), "b".into()])
                .unwrap()
                .starts_with("Skipped while 2+")
        );
    }

    #[test]
    fn topology_compares_sets() {
        let st = |source| AudioSourceState {
            source,
            status: AudioSourceStatus::Starting,
        };
        assert!(!topology_pending(&[1, 2], &[]));
        assert!(!topology_pending(&[2, 1], &[st(1), st(2)]));
        assert!(topology_pending(&[1], &[st(1), st(2)]));
    }

    #[test]
    fn gain_maps_round_trip() {
        assert_eq!(value_to_gain(0.5), 100);
        assert_eq!(value_to_gain(1.0), MAX_AUDIO_GAIN_PERCENT);
        assert_eq!(value_to_gain(gain_to_value(137)), 137);
    }

    #[test]
    fn labels() {
        assert_eq!(
            row_labels(AudioSourceType::Desktop, None, None).1,
            "Default output"
        );
        assert_eq!(
            row_labels(AudioSourceType::Microphone, Some("id"), None).1,
            "Unavailable device"
        );
        assert_eq!(
            row_labels(AudioSourceType::Application, Some("a.exe"), Some("App")),
            ("App".into(), "a.exe".into())
        );
    }
}
