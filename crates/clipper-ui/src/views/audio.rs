//! Audio Mixer card of the Capture page. Device and application enumeration run off the
//! UI thread; state shown per row comes only from engine events.

use std::{cell::Cell, rc::Rc, time::Duration};

use clipper_audio::DeviceFlow;
use clipper_audio::windows::{
    AudioApplication, capabilities, list_audio_applications, list_devices,
};
use clipper_core::recording::{
    AudioSourceState, AudioSourceStatus, MAX_AUDIO_GAIN_PERCENT, MAX_RECORDED_AUDIO_SOURCES,
    NoiseGateSettings, RecorderCommand, SourceLevel,
};
use clipper_platform::windows::tray;
use clipper_storage::{AudioSourceSettings, AudioSourceType, MAX_AUDIO_SOURCES};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Div, ElementId, Entity, FocusHandle, FontWeight, Hsla,
    MouseButton, Pixels, SharedString, Subscription, Window, anchored, canvas, deferred, div, fill,
    point, px, size,
};

use super::{ClipperWindow, Confirm, Dismiss, SelectNext, SelectPrevious};
use crate::components::{
    Activate, ButtonVariant, DropdownOption, Icon, TextInput, TextInputEvent, Tone, button, icon,
    icon_card, icon_tile, slider, status_dot, step, toggle, tooltip,
};
use crate::theme::{ActiveTheme, Theme};

/// Key context of the Add Source popup; Escape closes it.
pub(super) const ADD_CONTEXT: &str = "AudioAdd";
/// Focus-map key of the "Separate tracks" toggle (not a source id).
const TRACKS_KEY: u32 = u32::MAX;
/// Meter animation step while the mixer is visible and a meter moves
/// (~30 Hz, the engine's reporting rate; idle when everything is settled).
const METER_FRAME: Duration = Duration::from_millis(33);
/// Segments of a level meter (3 dB each on the 60 dB scale).
const METER_SEGMENTS: usize = 40;
const IDLE_APP_HINT: &str = "Starts capturing when the app plays audio";
const NO_LOOPBACK: &str = "Needs Windows 11 or Windows 10 build 20348+";

/// Keyboard focus of one mixer row.
#[derive(Clone)]
pub(super) struct RowFocus {
    menu: FocusHandle,
    slider: FocusHandle,
    toggle: FocusHandle,
    solo: FocusHandle,
    mute: FocusHandle,
    gate: FocusHandle,
    gate_fields: [FocusHandle; 4],
    gate_enabled: FocusHandle,
}

/// Only transient text lives here. The source configuration remains authoritative.
pub(super) struct GainEdit {
    source: u32,
    input: Entity<TextInput>,
    _events: Subscription,
    _blur: Subscription,
}

fn parse_gain_db(text: &str) -> Result<u16, &'static str> {
    let text = text.trim();
    let number = text.strip_prefix(['+', '-']).unwrap_or(text);
    if number.is_empty()
        || !number.chars().any(|c| c.is_ascii_digit())
        || number
            .chars()
            .any(|c| !c.is_ascii_digit() && c != '.' && c != ',')
        || number.chars().filter(|c| *c == '.' || *c == ',').count() > 1
    {
        return Err("Enter a number from -40 to +6.02 dB (decimal point or comma).");
    }
    let db: f64 = text
        .replace(',', ".")
        .parse()
        .map_err(|_| "Invalid gain value.")?;
    let maximum = 20.0 * (f64::from(MAX_AUDIO_GAIN_PERCENT) / 100.0).log10();
    if !db.is_finite() || db < -40.0 || db > maximum + 0.000001 {
        return Err("Gain must be between -40 and +6.02 dB. Use the slider for silence.");
    }
    Ok((100.0 * 10_f64.powf(db / 20.0))
        .round()
        .clamp(1.0, f64::from(MAX_AUDIO_GAIN_PERCENT)) as u16)
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
/// Whether an application is running is not decided here: the app list only holds apps
/// currently playing audio, so the engine's per-source status is the authority.
fn row_unavailable(
    kind: AudioSourceType,
    target: Option<&str>,
    loopback: bool,
    device_known: Option<bool>,
) -> Option<&'static str> {
    match kind {
        AudioSourceType::Game if !loopback => Some(NO_LOOPBACK),
        AudioSourceType::Application if !loopback => Some(NO_LOOPBACK),
        AudioSourceType::Desktop | AudioSourceType::Microphone
            if target.is_some() && device_known == Some(false) =>
        {
            Some("Device not available")
        }
        _ => None,
    }
}

/// Name and subtitle of a row. `resolved` is the friendly application name or the device name.
pub(super) fn row_labels(
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
                solo: cx.focus_handle().tab_index(12).tab_stop(true),
                mute: cx.focus_handle().tab_index(12).tab_stop(true),
                gate: cx.focus_handle().tab_index(12).tab_stop(true),
                gate_fields: std::array::from_fn(|_| {
                    cx.focus_handle().tab_index(12).tab_stop(true)
                }),
                gate_enabled: cx.focus_handle().tab_index(12).tab_stop(true),
            })
            .clone()
    }

    pub(super) fn snapshot_audio_focus(&self, window: &Window) {
        let mut set = self.audio_focused.borrow_mut();
        set.clear();
        for (id, f) in self.audio_focus.borrow().iter() {
            for (slot, h) in [&f.menu, &f.slider, &f.toggle, &f.solo, &f.mute, &f.gate]
                .into_iter()
                .chain(f.gate_fields.iter())
                .chain(std::iter::once(&f.gate_enabled))
                .enumerate()
            {
                if h.is_focused(window) {
                    set.insert((*id, slot as u8));
                }
            }
        }
    }

    /// `slot` is 0 menu, 1 slider, 2 on/off, 3 solo, 4 mute (`RowFocus` fields).
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

    pub(super) fn resolved_name(&self, source: &AudioSourceSettings) -> Option<String> {
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

    fn audio_edit_gain(&mut self, id: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.audio_source(id) else {
            return;
        };
        let text = if source.gain_percent == 0 {
            String::new()
        } else {
            format!(
                "{:.2}",
                20.0 * (f64::from(source.gain_percent) / 100.0).log10()
            )
        };
        self.audio_finish_gain_edit(true, cx);
        let input = cx.new(|cx| TextInput::new(cx).compact().max_len(24).placeholder("−∞"));
        input.update(cx, |input, cx| {
            input.set_text(text, cx);
            input.select_all(cx);
        });
        let focus = input.read(cx).focus_handle().clone();
        let events = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Changed(_) => {}
                TextInputEvent::Submitted(_) => {
                    this.audio_finish_gain_edit(true, cx);
                    window.focus(&this.focus);
                }
                TextInputEvent::Dismissed => {
                    this.audio_finish_gain_edit(false, cx);
                    window.focus(&this.focus);
                }
            },
        );
        let blur = cx.on_blur(&focus, window, |this, _, cx| {
            this.audio_finish_gain_edit(true, cx)
        });
        self.audio_gain_edit = Some(GainEdit {
            source: id,
            input,
            _events: events,
            _blur: blur,
        });
        window.focus(&focus);
        cx.notify();
    }

    fn audio_finish_gain_edit(&mut self, commit: bool, cx: &mut Context<Self>) {
        let Some(edit) = self.audio_gain_edit.take() else {
            return;
        };
        if commit && self.audio_source(edit.source).is_some() {
            let parsed = parse_gain_db(edit.input.read(cx).text());
            match parsed {
                Ok(gain) => {
                    self.audio_notice = None;
                    self.audio_set_gain(edit.source, gain_to_value(gain), cx);
                    self.audio_commit_gain(cx);
                }
                Err(message) => self.audio_notice = Some(message.into()),
            }
        }
        cx.notify();
    }

    fn audio_set_gate(&mut self, id: u32, settings: NoiseGateSettings, cx: &mut Context<Self>) {
        let Some(source) = self.config.audio.sources.iter_mut().find(|s| s.id == id) else {
            return;
        };
        let settings = settings.sanitized();
        source.noise_gate = settings;
        self.send(RecorderCommand::SetAudioNoiseGate {
            source: id,
            settings,
        });
        self.audio_gate_dirty = true;
        cx.notify();
    }

    fn audio_commit_gate(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.audio_gate_dirty) {
            self.persist_config(cx);
        }
    }

    fn audio_set_gain(&mut self, id: u32, value: f32, cx: &mut Context<Self>) {
        if self
            .audio_gain_edit
            .as_ref()
            .is_some_and(|edit| edit.source == id)
        {
            self.audio_gain_edit = None;
        }
        let gain_percent = value_to_gain(value);
        let Some(source) = self.config.audio.sources.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if source.gain_percent == gain_percent {
            return;
        }
        source.gain_percent = gain_percent;
        self.audio_gain_dirty = true;
        // A muted or solo-silenced source keeps sending 0: the slider edits
        // the setting, mute and solo decide what is heard.
        let gain_percent = self.config.audio.effective_gain_percent(id);
        self.send(RecorderCommand::SetAudioGain {
            source: id,
            gain_percent,
        });
        cx.notify();
    }

    /// Sends every source's effective gain (mute/solo changed). The engine
    /// ramps each change, so toggles never click; sources a running session
    /// does not capture are ignored by the recorder.
    fn audio_push_gains(&mut self) {
        let gains: Vec<(u32, u16)> = self
            .config
            .audio
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| (s.id, self.config.audio.effective_gain_percent(s.id)))
            .collect();
        for (source, gain_percent) in gains {
            self.send(RecorderCommand::SetAudioGain {
                source,
                gain_percent,
            });
        }
    }

    fn audio_set_flag(
        &mut self,
        id: u32,
        edit: impl FnOnce(&mut AudioSourceSettings),
        cx: &mut Context<Self>,
    ) {
        if let Some(source) = self.config.audio.sources.iter_mut().find(|s| s.id == id) {
            edit(source);
            self.persist_config(cx);
            self.audio_push_gains();
            cx.notify();
        }
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
            // A disabled soloed row no longer silences the others.
            self.audio_push_gains();
        }
    }

    fn audio_remove(&mut self, id: u32, cx: &mut Context<Self>) {
        if self
            .audio_gain_edit
            .as_ref()
            .is_some_and(|edit| edit.source == id)
        {
            self.audio_gain_edit = None;
        }
        if self.audio_gate_open == Some(id) {
            self.audio_gate_open = None;
        }
        self.config.audio.sources.retain(|s| s.id != id);
        self.audio_focus.borrow_mut().remove(&id);
        if self.audio_menu.is_some_and(|(open, _)| open == id) {
            self.audio_menu = None;
        }
        self.audio_notice = None;
        self.persist_config(cx);
        self.audio_push_gains();
    }

    // row menu (⋯): device or application choice, reset gain, remove

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

    /// Rows of the ⋯ menu and what each does: the source's device or
    /// application choices (none for Game), then Reset gain and Remove.
    fn row_menu_items(&self, id: u32) -> (Vec<DropdownOption>, Vec<RowAction>, usize) {
        let (mut options, mut actions, selected) = match self.audio_target_options(id) {
            Some((options, values, selected)) => (
                options,
                values.into_iter().map(RowAction::Target).collect(),
                selected,
            ),
            None => (Vec::new(), Vec::new(), 0),
        };
        let action = |label: &str| DropdownOption {
            label: label.to_owned().into(),
            detail: None,
            enabled: true,
            reason: None,
        };
        options.push(action("Reset gain to 0 dB"));
        actions.push(RowAction::ResetGain);
        options.push(action("Remove source"));
        actions.push(RowAction::Remove);
        (options, actions, selected)
    }

    fn audio_menu_toggle(&mut self, id: u32, cx: &mut Context<Self>) {
        if self.audio_menu.is_some_and(|(open, _)| open == id) {
            self.audio_menu = None;
        } else if self.audio_source(id).is_some() {
            let (_, _, selected) = self.row_menu_items(id);
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
        let (_, actions, _) = self.row_menu_items(id);
        match actions.into_iter().nth(index) {
            Some(RowAction::Target(value)) => {
                if let Some(source) = self.config.audio.sources.iter_mut().find(|s| s.id == id) {
                    source.target = value;
                    self.persist_config(cx);
                }
            }
            Some(RowAction::ResetGain) => {
                self.audio_set_flag(id, |s| s.gain_percent = 100, cx);
            }
            Some(RowAction::Remove) => self.audio_remove(id, cx),
            None => {}
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
        let (options, _, _) = self.row_menu_items(open);
        let next = step(&options, from, delta);
        if next != from {
            self.audio_menu = Some((open, next));
            cx.notify();
        }
    }

    // meters

    /// New engine readings (post-gain peaks). Only sets targets; the ticker
    /// animates and redraws, so readings never cause a redraw on their own.
    pub(super) fn apply_meter_levels(
        &mut self,
        levels: &[SourceLevel],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.meters
            .retain(|id, _| levels.iter().any(|level| level.source == *id));
        for level in levels {
            self.meters.entry(level.source).or_default().set(level.peak);
        }
        self.ensure_meter_ticker(window, cx);
    }

    /// The mixer is on screen: Capture page, window neither hidden nor minimized.
    fn meters_visible(&self) -> bool {
        self.active_page == super::Page::Dashboard
            && self.feedback.main_hwnd.is_none_or(tray::is_shown)
    }

    /// Runs the ~30 Hz meter animation only while it shows something moving;
    /// it stops itself when every meter settled or the mixer is hidden.
    /// Recording never depends on it.
    fn ensure_meter_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.meter_ticker.is_some()
            || !self.meters_visible()
            || self.meters.values().all(crate::meter::Ballistics::settled)
        {
            return;
        }
        self.meter_ticker = Some(cx.spawn_in(window, async move |this, cx| {
            let mut last = std::time::Instant::now();
            loop {
                cx.background_executor().timer(METER_FRAME).await;
                let now = std::time::Instant::now();
                let dt = now - last;
                last = now;
                let running = this
                    .update(cx, |this, cx| {
                        for meter in this.meters.values_mut() {
                            meter.advance(dt);
                        }
                        cx.notify();
                        let running = this.meters_visible()
                            && !this.meters.values().all(crate::meter::Ballistics::settled);
                        if !running {
                            this.meter_ticker = None;
                        }
                        running
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
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
            muted: false,
            solo: false,
            noise_gate: NoiseGateSettings::default(),
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

/// What a ⋯ menu row does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowAction {
    /// Use this device or application (`None` = Windows default).
    Target(Option<String>),
    ResetGain,
    Remove,
}

/// Mixer row layout (reference: tall rows, aligned columns).
const ROW_HEIGHT: f32 = 64.0;
const NAME_MIN_WIDTH: f32 = 100.0;
const NAME_MAX_WIDTH: f32 = 260.0;
const METER_MIN_WIDTH: f32 = 80.0;
const GAIN_LABEL_WIDTH: f32 = 72.0;
const SLIDER_WIDTH: f32 = 124.0;
const ROW_BUTTON: f32 = 34.0;

fn kind_icon(kind: AudioSourceType) -> Icon {
    match kind {
        AudioSourceType::Game => Icon::Game,
        AudioSourceType::Application => Icon::Volume,
        AudioSourceType::Desktop => Icon::Monitor,
        AudioSourceType::Microphone => Icon::Microphone,
    }
}

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

/// Segmented level meter (reference): dark segments light up to the smoothed
/// level in their dBFS zone colour; the peak-hold segment stays lit, the
/// last segment marks clipping. One canvas per row, no per-segment elements.
fn level_meter(t: &Theme, meter: crate::meter::Ballistics) -> impl IntoElement {
    use crate::meter::{Zone, zone};
    let (green, yellow, red, off) = (
        t.colors.success,
        t.colors.warning,
        t.colors.danger,
        t.colors.border,
    );
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let count = METER_SEGMENTS as f32;
            let gap = px(2.);
            let width = (bounds.size.width - gap * (count - 1.)) / count;
            if width <= px(0.) {
                return;
            }
            let lit = (meter.level * count).ceil() as usize;
            let peak =
                (meter.peak > 0.0).then(|| ((meter.peak * count).ceil() as usize).max(1) - 1);
            for index in 0..METER_SEGMENTS {
                let on = index < lit
                    || peak == Some(index)
                    || (meter.clipped && index + 1 == METER_SEGMENTS);
                let color = if !on {
                    off
                } else {
                    match zone(index, METER_SEGMENTS) {
                        Zone::Green => green,
                        Zone::Yellow => yellow,
                        Zone::Red => red,
                    }
                };
                let origin = point(
                    bounds.origin.x + (width + gap) * index as f32,
                    bounds.origin.y,
                );
                window.paint_quad(
                    fill(Bounds::new(origin, size(width, bounds.size.height)), color)
                        .corner_radii(px(1.)),
                );
            }
        },
    )
    .w_full()
    .h(px(10.))
}

/// Square row toggle (On/Off, S, M): tinted when active, Space/Enter work.
#[allow(clippy::too_many_arguments)]
fn row_button(
    t: &Theme,
    id: impl Into<ElementId>,
    content: AnyElement,
    active: bool,
    tint: Hsla,
    enabled: bool,
    focus: &FocusHandle,
    focused: bool,
    tip: &'static str,
    on_press: impl Fn(&mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    let on_press = Rc::new(on_press);
    let on_key = Rc::clone(&on_press);
    let hover = t.colors.surface_hover;
    div()
        .id(id)
        .track_focus(focus)
        .key_context("Toggle")
        .flex_none()
        .size(px(ROW_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .border_color(if focused {
            t.colors.focus_ring
        } else if active {
            tint
        } else {
            t.colors.border
        })
        .bg(if active {
            tint.opacity(0.22)
        } else {
            t.colors.surface
        })
        .text_color(if active {
            tint
        } else {
            t.colors.text_secondary
        })
        .tooltip(tooltip(tip))
        .child(content)
        .when(enabled, |this| {
            this.cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(move |_, window, cx| on_press(window, cx))
                .on_action(move |_: &Activate, window, cx| on_key(window, cx))
        })
        .when(!enabled, |this| this.opacity(0.45))
}

/// The ⋯ button and its menu: device or application choices, Reset gain,
/// Remove. Keyboard: Enter opens/applies, Up/Down move, Escape closes.
fn row_menu(
    t: &Theme,
    window: &ClipperWindow,
    id: u32,
    highlighted: Option<usize>,
    focus: &FocusHandle,
    cx: &mut Context<ClipperWindow>,
) -> AnyElement {
    let trigger_bounds: Rc<Cell<Bounds<Pixels>>> = Rc::default();
    let out = Rc::clone(&trigger_bounds);
    let focused = window.is_focused(id, 0);
    let trigger = div()
        .id(("audio-menu", id as usize))
        .track_focus(focus)
        .key_context("Dropdown")
        .relative()
        .flex_none()
        .size(px(ROW_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(if focused || highlighted.is_some() {
            t.colors.focus_ring
        } else {
            gpui::transparent_black()
        })
        .cursor_pointer()
        .text_color(t.colors.text_secondary)
        .hover(|style| style.bg(t.colors.surface_hover))
        .tooltip(tooltip("More: device, reset gain, remove"))
        .child(
            canvas(move |b, _, _| out.set(b), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .child(icon(Icon::More, px(16.)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| this.audio_menu_toggle(id, cx)),
        );
    let popup = highlighted.map(|highlighted| {
        let (options, actions, selected) = window.row_menu_items(id);
        let targets = actions
            .iter()
            .filter(|a| matches!(a, RowAction::Target(_)))
            .count();
        let mut list = div()
            .id(("audio-menu-list", id as usize))
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
                // Presses on the trigger toggle it themselves.
                if !trigger_bounds.get().contains(&window.mouse_position()) {
                    this.audio_menu_close(cx);
                }
            }));
        if targets > 0 {
            list = list.child(muted(t, "Device").px_2().pt_1());
        }
        for (index, option) in options.into_iter().enumerate() {
            if index == targets {
                list = list.child(div().h(px(1.)).my_1().bg(t.colors.border));
            }
            let remove = actions[index] == RowAction::Remove;
            let row = popup_row(
                t,
                ("audio-menu-row", id as usize * 64 + index),
                option.label,
                None,
                None,
            )
            .when(index == highlighted, |r| r.bg(t.colors.surface_hover))
            .when(remove, |r| r.text_color(t.colors.danger))
            .children(
                (index < targets && index == selected)
                    .then(|| div().text_color(t.colors.accent).child("✓")),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.audio_menu_apply(id, index, cx)));
            list = list.child(row);
        }
        deferred(anchored().snap_to_window_with_margin(px(8.)).child(list))
    });
    div()
        .relative()
        .flex_none()
        .on_action(cx.listener(move |this, _: &Confirm, _, cx| this.audio_menu_confirm(id, cx)))
        .on_action(cx.listener(move |this, _: &SelectNext, _, cx| this.audio_menu_step(id, 1, cx)))
        .on_action(
            cx.listener(move |this, _: &SelectPrevious, _, cx| this.audio_menu_step(id, -1, cx)),
        )
        .on_action(cx.listener(move |this, _: &Dismiss, _, cx| this.audio_menu_close(cx)))
        .child(trigger)
        .children(popup.map(|popup| {
            div()
                .absolute()
                .top(px(ROW_BUTTON + 4.))
                .right_0()
                .size_0()
                .child(popup)
        }))
        .into_any_element()
}

fn gate_control(
    t: &Theme,
    owner: &ClipperWindow,
    source: &AudioSourceSettings,
    focus: &RowFocus,
    cx: &mut Context<ClipperWindow>,
) -> AnyElement {
    let id = source.id;
    let open = owner.audio_gate_open == Some(id);
    let settings = source.noise_gate;
    let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::default();
    let out = bounds.clone();
    let entity = cx.entity();
    let trigger = row_button(
        t,
        ("audio-gate", id as usize),
        icon(Icon::Filter, px(16.)).into_any_element(),
        settings.enabled,
        t.colors.accent,
        true,
        &focus.gate,
        owner.is_focused(id, 5),
        "Noise Gate: configure threshold and timing",
        move |window, app| {
            entity.update(app, |this, cx| {
                this.audio_commit_gate(cx);
                this.audio_gate_open = if open { None } else { Some(id) };
                window.focus(&this.row_focus(id, cx).gate);
                cx.notify();
            })
        },
    );
    let popup = open.then(|| {
        let enabled_entity = cx.entity();
        let mut panel = div().id(("audio-gate-panel", id as usize)).occlude().w(px(280.)).p_3().flex().flex_col().gap_3()
            .rounded_lg().border_1().border_color(t.colors.border_strong).bg(t.colors.surface).shadow_lg()
            .on_mouse_down_out(cx.listener(move |this, _, window, cx| {
                if !bounds.get().contains(&window.mouse_position()) {
                    this.audio_commit_gate(cx);
                    this.audio_gate_open = None;
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.audio_commit_gate(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| this.audio_commit_gate(cx)))
            .on_key_up(cx.listener(|this, _, _, cx| this.audio_commit_gate(cx)))
            .child(div().flex().items_center().justify_between().child(div().text_sm().child("Noise Gate"))
                .child(toggle(t, ("audio-gate-enabled", id as usize), settings.enabled, true, &focus.gate_enabled, owner.is_focused(id, 10),
                    move |on, _, app| enabled_entity.update(app, |this, cx| {
                        if let Some(source) = this.audio_source(id) {
                            let mut settings = source.noise_gate;
                            settings.enabled = on;
                            this.audio_set_gate(id, settings, cx);
                            this.audio_commit_gate(cx);
                        }
                    }))));
        for (index, label, value, min, max, unit) in [
            (0, "Threshold", settings.threshold_db, -80.0, 0.0, "dBFS"),
            (1, "Attack", settings.attack_ms, 1.0, 100.0, "ms"),
            (2, "Hold", settings.hold_ms, 0.0, 1000.0, "ms"),
            (3, "Release", settings.release_ms, 5.0, 2000.0, "ms"),
        ] {
            let entity = cx.entity();
            panel = panel.child(div().flex().flex_col().gap_1()
                .child(div().flex().justify_between().text_xs().child(label).child(format!("{value:.0} {unit}")))
                .child(slider(t, ("audio-gate-field", id as usize * 4 + index), (value - min) / (max - min), true,
                    &focus.gate_fields[index], owner.is_focused(id, (6 + index) as u8),
                    move |value, _, app| entity.update(app, |this, cx| {
                        if !value.is_finite() { return; }
                        if let Some(source) = this.audio_source(id) {
                            let mut settings = source.noise_gate;
                            let value = (min + value.clamp(0.0, 1.0) * (max - min)).round();
                            match index {
                                0 => settings.threshold_db = value,
                                1 => settings.attack_ms = value,
                                2 => settings.hold_ms = value,
                                _ => settings.release_ms = value,
                            }
                            this.audio_set_gate(id, settings, cx);
                        }
                    }))));
        }
        panel = panel.child(muted(t, "Reduces noise between sounds; does not remove noise during speech. Threshold is measured before source gain."));
        deferred(anchored().snap_to_window_with_margin(px(8.)).child(panel))
    });
    div()
        .relative()
        .flex_none()
        .key_context("Dropdown")
        .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
            this.audio_commit_gate(cx);
            this.audio_gate_open = None;
            window.focus(&this.focus);
            cx.notify();
        }))
        .child(
            div().relative().child(trigger).child(
                canvas(move |bounds, _, _| out.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            ),
        )
        .children(popup.map(|panel| {
            div()
                .absolute()
                .top(px(ROW_BUTTON + 4.))
                .right_0()
                .size_0()
                .child(panel)
        }))
        .into_any_element()
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
    let mut rows = div().flex().flex_col().gap_2();
    for source in sources {
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
            device_known,
        );
        let (tone, mut text, meter) =
            row_status(source.enabled, state.map(|s| &s.status), unavailable);
        if source.enabled
            && state.is_none()
            && unavailable.is_none()
            && source.kind == AudioSourceType::Application
        {
            text = Some(IDLE_APP_HINT.into());
        }
        let overlap = (source.kind == AudioSourceType::Desktop && source.enabled)
            .then(|| overlap_note(&overlap_names))
            .flatten();

        let highlighted = window
            .audio_menu
            .filter(|(open, _)| *open == id)
            .map(|(_, h)| h);
        let menu = row_menu(&t, window, id, highlighted, &focus.menu, cx);

        // The meter shows what is recorded (post-gain, after mute/solo).
        let level_cell = if meter {
            level_meter(&t, window.meters.get(&id).copied().unwrap_or_default()).into_any_element()
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

        let gain = source.gain_percent;
        let audible = window.config.audio.effective_gain_percent(id) > 0;
        let e1 = entity.clone();
        let slider_cell = div()
            .w(px(SLIDER_WIDTH))
            .flex_none()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.audio_commit_gain(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.audio_commit_gain(cx)),
            )
            .on_key_up(cx.listener(|this, _, _, cx| this.audio_commit_gain(cx)))
            .child(slider(
                &t,
                ("audio-gain", id as usize),
                gain_to_value(gain),
                true,
                &focus.slider,
                window.is_focused(id, 1),
                move |v, _, app| e1.update(app, |this, cx| this.audio_set_gain(id, v, cx)),
            ));
        // The number is the gain setting in dB (not a signal level).
        let gain_cell = div()
            .id(("audio-gain-db", id as usize))
            .w(px(GAIN_LABEL_WIDTH))
            .flex_none()
            .flex()
            .justify_end()
            .text_sm()
            .text_color(if audible {
                t.colors.text_secondary
            } else {
                t.colors.text_muted
            })
            .h(px(24.))
            .items_center()
            .tooltip(tooltip(format!(
                "Gain {gain} %. Double-click to edit dB; -40 to +6.02 dB, rounded to 1 % steps."
            )))
            .when_some(
                window
                    .audio_gain_edit
                    .as_ref()
                    .filter(|edit| edit.source == id),
                |this, edit| this.child(div().w_full().child(edit.input.clone())),
            )
            .when(
                window
                    .audio_gain_edit
                    .as_ref()
                    .is_none_or(|edit| edit.source != id),
                |this| {
                    this.cursor_text()
                        .child(crate::meter::gain_label(gain))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                if event.click_count == 2 {
                                    this.audio_edit_gain(id, window, cx);
                                }
                            }),
                        )
                },
            );

        let gate = gate_control(&t, window, source, &focus, cx);

        let (e_on, e_solo, e_mute) = (entity.clone(), entity.clone(), entity.clone());
        let enabled = source.enabled;
        let on_button = row_button(
            &t,
            ("audio-on", id as usize),
            icon(
                if enabled {
                    Icon::Volume
                } else {
                    Icon::VolumeOff
                },
                px(16.),
            )
            .into_any_element(),
            enabled,
            t.colors.accent,
            enabled || (!at_enabled_max && unavailable.is_none()),
            &focus.toggle,
            window.is_focused(id, 2),
            if enabled {
                "Recorded. Click to stop recording this source."
            } else {
                "Not recorded. Click to record this source."
            },
            move |_, app| e_on.update(app, |this, cx| this.audio_toggle(id, !enabled, cx)),
        );
        let solo_button = row_button(
            &t,
            ("audio-solo", id as usize),
            "S".into_any_element(),
            source.solo,
            t.colors.warning,
            true,
            &focus.solo,
            window.is_focused(id, 3),
            "Solo: while any source is soloed, only soloed sources are heard",
            move |_, app| {
                e_solo.update(app, |this, cx| {
                    this.audio_set_flag(id, |s| s.solo = !s.solo, cx)
                })
            },
        );
        let mute_button = row_button(
            &t,
            ("audio-mute", id as usize),
            "M".into_any_element(),
            source.muted,
            t.colors.danger,
            true,
            &focus.mute,
            window.is_focused(id, 4),
            "Mute: record silence for this source, keeping its gain",
            move |_, app| {
                e_mute.update(app, |this, cx| {
                    this.audio_set_flag(id, |s| s.muted = !s.muted, cx)
                })
            },
        );

        let full_label = format!("{name} — {subtitle}");
        // Reference rows: kind tile, name, meter, gain, slider, buttons. Every
        // text column truncates (full text in a tooltip), so long device names
        // never push into the meter, slider or buttons.
        rows = rows.child(
            div()
                .id(("audio-row", id as usize))
                .flex()
                .items_center()
                .gap_2()
                .min_h(px(ROW_HEIGHT))
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(t.colors.surface_raised)
                .child(icon_tile(&t, kind_icon(source.kind), px(40.)))
                .child(
                    div()
                        .id(("audio-name", id as usize))
                        .flex_1()
                        .min_w(px(NAME_MIN_WIDTH))
                        .max_w(px(NAME_MAX_WIDTH))
                        .flex()
                        .flex_col()
                        .tooltip(tooltip(full_label))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .child(status_dot(&t, tone))
                                .child(div().min_w_0().text_base().truncate().child(name)),
                        )
                        .child(muted(&t, subtitle).truncate())
                        .children(overlap.map(|note| {
                            div()
                                .text_xs()
                                .text_color(t.colors.warning)
                                .truncate()
                                .child(note)
                        })),
                )
                .child(div().flex_1().min_w(px(METER_MIN_WIDTH)).child(level_cell))
                .child(gain_cell)
                .child(slider_cell)
                .child(gate)
                .child(on_button)
                .child(solo_button)
                .child(mute_button)
                .child(menu),
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
                .child(muted(&t, "Combined/Master + one track per source")),
        )
        .child(test);

    icon_card(
        &t,
        Icon::Volume,
        "Audio Mixer",
        "Manage and control your audio sources.",
        Some(add.into_any_element()),
    )
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
        assert_eq!(row_unavailable(Game, None, false, None), Some(NO_LOOPBACK));
        assert_eq!(row_unavailable(Game, None, true, None), None);
        assert_eq!(
            row_unavailable(Application, Some("a.exe"), false, None),
            Some(NO_LOOPBACK)
        );
        assert_eq!(
            row_unavailable(Microphone, Some("id"), true, Some(false)),
            Some("Device not available")
        );
        assert_eq!(row_unavailable(Microphone, None, true, Some(false)), None);
    }

    #[test]
    fn gain_maps_round_trip() {
        assert_eq!(value_to_gain(0.5), 100);
        assert_eq!(value_to_gain(1.0), MAX_AUDIO_GAIN_PERCENT);
        assert_eq!(value_to_gain(gain_to_value(137)), 137);
    }

    #[test]
    fn typed_gain_validates_and_normalizes_decimal_separators() {
        assert_eq!(parse_gain_db("0"), Ok(100));
        assert_eq!(parse_gain_db("+6.0"), Ok(200));
        assert_eq!(parse_gain_db("-6.5"), parse_gain_db("-6,5"));
        assert_eq!(parse_gain_db("-12"), Ok(25));
        assert_eq!(parse_gain_db("-6"), Ok(50));
        assert_eq!(parse_gain_db("+3"), Ok(141));
        for invalid in [
            "", "-", "NaN", "inf", "1e99", "1.2.3", "1,2.3", "7", "-41", "1 dB",
        ] {
            assert!(parse_gain_db(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn typed_gain_and_slider_share_supported_percent_steps() {
        for percent in 1..=MAX_AUDIO_GAIN_PERCENT {
            let db = 20.0 * (f64::from(percent) / 100.0).log10();
            let parsed = parse_gain_db(&format!("{db:.6}")).unwrap();
            assert_eq!(parsed, percent);
            assert_eq!(value_to_gain(gain_to_value(parsed)), percent);
            assert_eq!(parse_gain_db(&format!("{db:.2}")), Ok(percent));
        }
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
