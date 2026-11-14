mod audio;
mod capture_bar;
mod clips;
mod dashboard;
mod editor;
mod quick_actions;
mod recent_card;
mod settings;
mod source_card;
mod storage_card;

use clipper_core::hotkey::Hotkey;
use clipper_core::recording::{RecorderCommand, RecorderEvent, RecorderHandle};
use clipper_platform::windows::hotkey::HotkeyHandle;
use clipper_platform::windows::tray::{TrayCommand, TrayHandle};
use clipper_storage::AppConfig;
use gpui::prelude::*;
use gpui::{
    App, AsyncWindowContext, Context, Div, Entity, FocusHandle, FontWeight, KeyBinding,
    KeyDownEvent, NoAction, Render, Stateful, Task, WeakEntity, Window, actions, div, img, px,
};

use crate::components::{Icon, icon, kbd, list_item, shortcut, tooltip};
use crate::notifications::{Notifier, Toast};
use crate::state::{Phase, RecordingState, StorageProjectionCache, StorageProjectionRun};
use crate::storage_status::{self, StorageStatus};
use crate::theme::{ActiveTheme, Theme};

/// Updates the view from a task spawned on its window. gpui refuses the update
/// while that window is itself mid-update, which happens when a nested Win32
/// message loop (move/size loop, COM call, dialog) runs this task's posted
/// wake-up. That is transient: wait a frame and retry. `None` once the view is
/// gone. Giving up on the first refusal used to end the recorder event loop
/// for good: nothing drained recorder events any more, so clip saves never
/// completed and Instant Replay hung in "Stopping…".
async fn update_view<R>(
    this: &WeakEntity<ClipperWindow>,
    cx: &mut AsyncWindowContext,
    mut update: impl FnMut(&mut ClipperWindow, &mut Window, &mut Context<ClipperWindow>) -> R,
) -> Option<R> {
    loop {
        if let Ok(result) = this.update_in(cx, &mut update) {
            return Some(result);
        }
        this.upgrade()?;
        cx.background_executor()
            .timer(std::time::Duration::from_millis(16))
            .await;
    }
}

/// `v` + the package version this binary was built from (Cargo metadata,
/// shared by every crate of the workspace).
fn version_label() -> &'static str {
    concat!("v", env!("CARGO_PKG_VERSION"))
}

/// clipuh logo shown in the sidebar header; brand purple, independent of the user accent.
fn brand_logo() -> std::sync::Arc<gpui::Image> {
    static LOGO: std::sync::OnceLock<std::sync::Arc<gpui::Image>> = std::sync::OnceLock::new();
    LOGO.get_or_init(|| {
        std::sync::Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            include_bytes!("../../../../assets/branding/clipuh-icon-64.png").to_vec(),
        ))
    })
    .clone()
}

actions!(
    clipper,
    [
        OpenDashboard,
        OpenClips,
        OpenEditor,
        OpenSettings,
        NextPage,
        PreviousPage,
        ToggleCommandPalette,
        FocusNext,
        FocusPrevious,
        SelectNext,
        SelectPrevious,
        Confirm,
        Dismiss,
    ]
);

const WINDOW_CONTEXT: &str = "ClipperWindow";
const PALETTE_CONTEXT: &str = "CommandPalette";
const HOTKEY_CONTEXT: &str = "HotkeyRecorder";

/// Registers the window's keyboard shortcuts. Call once at startup.
pub fn bind_keys(cx: &mut App) {
    crate::components::bind_keys(cx);
    clips::bind_keys(cx);
    let window = Some(WINDOW_CONTEXT);
    let palette = Some(PALETTE_CONTEXT);
    let dropdown = Some("Dropdown");
    cx.bind_keys([
        KeyBinding::new("ctrl-1", OpenDashboard, window),
        KeyBinding::new("ctrl-2", OpenClips, window),
        KeyBinding::new("ctrl-3", OpenEditor, window),
        KeyBinding::new("ctrl-4", OpenSettings, window),
        KeyBinding::new("ctrl-tab", NextPage, window),
        KeyBinding::new("ctrl-shift-tab", PreviousPage, window),
        KeyBinding::new("ctrl-k", ToggleCommandPalette, window),
        KeyBinding::new("tab", FocusNext, window),
        KeyBinding::new("shift-tab", FocusPrevious, window),
        KeyBinding::new("enter", Confirm, window),
        KeyBinding::new("down", SelectNext, palette),
        KeyBinding::new("up", SelectPrevious, palette),
        KeyBinding::new("enter", Confirm, palette),
        KeyBinding::new("escape", Dismiss, palette),
        KeyBinding::new("up", SelectPrevious, dropdown),
        KeyBinding::new("down", SelectNext, dropdown),
        KeyBinding::new("enter", Confirm, dropdown),
        KeyBinding::new("space", Confirm, dropdown),
        KeyBinding::new("escape", Dismiss, dropdown),
        KeyBinding::new("escape", Dismiss, Some(audio::ADD_CONTEXT)),
        KeyBinding::new("space", editor::TogglePlayback, Some(editor::CONTEXT)),
        KeyBinding::new("i", editor::SetIn, Some(editor::CONTEXT)),
        KeyBinding::new("o", editor::SetOut, Some(editor::CONTEXT)),
    ]);
    // gpui runs key bindings before `on_key_down`. While the hotkey recorder has
    // focus, its (deeper) NoAction bindings cancel the window bindings so every
    // combo reaches the recorder instead of navigating or moving focus.
    let recorder = Some(HOTKEY_CONTEXT);
    cx.bind_keys(
        [
            "ctrl-1",
            "ctrl-2",
            "ctrl-3",
            "ctrl-4",
            "ctrl-tab",
            "ctrl-shift-tab",
            "ctrl-k",
            "tab",
            "shift-tab",
            "enter",
        ]
        .map(|keys| KeyBinding::new(keys, NoAction, recorder)),
    );
}

/// The capture dropdowns in the capture bar and on the Capture page; at most
/// one is open at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureMenu {
    Resolution,
    FrameRate,
    Quality,
    Bitrate,
    ReplayDuration,
}

impl CaptureMenu {
    fn label(self) -> &'static str {
        match self {
            Self::Resolution => "Resolution",
            Self::FrameRate => "Frame rate",
            Self::Quality => "Quality",
            Self::Bitrate => "Base bitrate",
            Self::ReplayDuration => "Replay length",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Dashboard,
    Clips,
    Editor,
    Settings,
}

impl Page {
    const ALL: [Self; 4] = [Self::Dashboard, Self::Clips, Self::Editor, Self::Settings];

    fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Capture",
            Self::Clips => "Library",
            Self::Editor => "Editor",
            Self::Settings => "Settings",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Dashboard => Icon::Camera,
            Self::Clips => Icon::Folder,
            Self::Editor => Icon::Cut,
            Self::Settings => Icon::Settings,
        }
    }

    fn shortcut(self) -> &'static [&'static str] {
        match self {
            Self::Dashboard => &["Ctrl", "1"],
            Self::Clips => &["Ctrl", "2"],
            Self::Editor => &["Ctrl", "3"],
            Self::Settings => &["Ctrl", "4"],
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|page| *page == self).unwrap_or(0)
    }

    fn offset(self, delta: isize) -> Self {
        let len = Self::ALL.len() as isize;
        Self::ALL[(self.index() as isize + delta).rem_euclid(len) as usize]
    }
}

/// Command palette state. Commands are page navigation for now; capture actions
/// join the same list once a backend exists.
struct CommandPalette {
    focus: FocusHandle,
    query: String,
    selected: usize,
}

impl CommandPalette {
    fn matches(&self) -> Vec<Page> {
        let query = self.query.to_lowercase();
        Page::ALL
            .into_iter()
            .filter(|page| page.label().to_lowercase().contains(&query))
            .collect()
    }
}

/// Main desktop application shell.
pub struct ClipperWindow {
    handle: RecorderHandle,
    hotkeys: HotkeyHandle,
    notifier: Notifier,
    /// The shortcut Clipper wants registered (the configured one).
    hotkey: Hotkey,
    /// Why `hotkey` could not be registered, if it could not.
    hotkey_error: Option<String>,
    hotkey_recorder: Option<settings::HotkeyRecorder>,
    config: AppConfig,
    recording: RecordingState,
    storage_projection_cache: StorageProjectionCache,
    storage_projection_run: StorageProjectionRun,
    /// Awaits recorder events; the UI re-renders only when one arrives.
    _events: Task<()>,
    _hotkey_failures: Task<()>,
    /// Phase 9: toasts, tray state and quick-action bookkeeping.
    feedback: quick_actions::Feedback,
    _tray_commands: Option<Task<()>>,
    active_page: Page,
    focus: FocusHandle,
    nav_focus: Vec<FocusHandle>,
    palette: Option<CommandPalette>,
    config_persistence: std::sync::Arc<crate::state::SettingsSaveOrder>,
    audio_devices: Vec<clipper_audio::AudioDevice>,
    audio_apps: Vec<clipper_audio::windows::AudioApplication>,
    audio_capabilities: Option<clipper_audio::AudioCapabilities>,
    /// Open target dropdown: (mixer source id, highlighted row).
    audio_menu: Option<(u32, usize)>,
    audio_loading: bool,
    /// Latest per-source peaks and engine state; empty when no session runs.
    audio_levels: Vec<clipper_core::recording::SourceLevel>,
    /// Smoothed mixer meters by source id (Capture page only).
    meters: std::collections::HashMap<u32, crate::meter::Ballistics>,
    /// Animates `meters` while the mixer is visible and a meter moves.
    meter_ticker: Option<Task<()>>,
    audio_sources: Vec<clipper_core::recording::AudioSourceState>,
    /// Add Source popup step; `None` is closed.
    audio_add: Option<audio::AddStep>,
    audio_add_focus: FocusHandle,
    audio_add_note: Option<String>,
    audio_notice: Option<String>,
    /// A gain changed and is not saved yet (saved when the drag or key press ends).
    audio_gain_dirty: bool,
    audio_focus: std::cell::RefCell<std::collections::HashMap<u32, audio::RowFocus>>,
    /// Audio handles that held focus at the last render (components get no `Window`),
    /// as (row id, slot); see `ClipperWindow::is_focused` in `audio.rs`.
    audio_focused: std::cell::RefCell<std::collections::HashSet<(u32, u8)>>,
    audio_testing: bool,
    audio_test_path: Option<String>,
    /// Replay was enabled in the config and has not been auto-started yet.
    replay_autostart_pending: bool,
    /// The open capture dropdown and its highlighted row.
    capture_menu: Option<(CaptureMenu, usize)>,
    capture_focus: [FocusHandle; 5],
    /// Video settings (width, height, fps) the running replay or recording was
    /// started with; `None` while neither runs. Lets the capture bar mark a
    /// changed value as pending instead of pretending it is already active.
    active_video: Option<(u32, u32, u32)>,
    /// Keyboard focus for the capture bar's replay switch.
    replay_focus: FocusHandle,
    /// Free space on the recordings volume, shown in the sidebar.
    storage: StorageStatus,
    /// Highlighted row while the Capture page's source menu is open.
    source_menu: Option<usize>,
    source_focus: FocusHandle,
    /// Latest clips in the recordings folder for the Capture page.
    library: Entity<clips::ClipLibrary>,
    /// The clip open in the Editor page; its player only runs while that page is shown.
    editor: Option<Entity<editor::ClipEditor>>,
    /// Custom accent color field on the Settings page.
    accent_input: Entity<crate::components::TextInput>,
    /// The open Settings dropdown and its highlighted row.
    settings_menu: Option<(settings::SettingsMenu, usize)>,
    /// Scroll state of the Settings content; `settings_section` is the
    /// selected category, the only one rendered. Kept while navigating away.
    settings_scroll: gpui::ScrollHandle,
    settings_section: usize,
    /// Whether Windows launches clipuh at sign-in, read from the registry;
    /// `None` while a read or change is in flight.
    startup: Option<bool>,
    startup_error: Option<String>,
    /// Last clip-folder or open-folder failure on the Settings page.
    settings_notice: Option<String>,
    /// Result of the last debug-log action: `(is_error, message)`.
    diagnostics_notice: Option<(bool, String)>,
    /// A debug-log export is running (the button is disabled meanwhile).
    diagnostics_exporting: bool,
}

impl ClipperWindow {
    pub fn new(
        handle: RecorderHandle,
        hotkeys: HotkeyHandle,
        hotkey_failures: async_channel::Receiver<String>,
        tray: Option<(TrayHandle, async_channel::Receiver<TrayCommand>)>,
        config: AppConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = handle.events();
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                crate::diagnostics::log_event(&event);
                let toast = match &event {
                    RecorderEvent::ReplaySaving => Some(Toast::Saving),
                    RecorderEvent::ReplaySaved { path, duration } => Some(Toast::Saved {
                        path: path.clone(),
                        duration: *duration,
                    }),
                    RecorderEvent::ReplaySaveFailed { message } => Some(Toast::Failed {
                        message: message.clone(),
                    }),
                    _ => None,
                };
                let sources_listed = matches!(event, RecorderEvent::SourcesListed(_));
                // Free space only changes when a clip or recording lands on disk.
                let wrote_file = matches!(
                    event,
                    RecorderEvent::ReplaySaved { .. } | RecorderEvent::RecordingFinished { .. }
                );
                let mut pending = Some((event, toast));
                let updated = update_view(&this, cx, |this, window, cx| {
                    let Some((event, toast)) = pending.take() else {
                        return;
                    };
                    let mut quiet = false;
                    let notices = crate::feedback::notices(
                        &event,
                        &crate::feedback::Before {
                            replay: &this.recording.replay.phase,
                            selected_source: this.recording.selected_source(),
                            audio: &this.audio_sources,
                        },
                    );
                    this.storage_projection_run.apply_event(&event);
                    match &event {
                        RecorderEvent::AudioLevels(levels) => {
                            // Readings only set meter targets; the meter ticker
                            // redraws while the mixer is visible and moving.
                            this.audio_levels = levels.clone();
                            this.apply_meter_levels(levels, window, cx);
                            quiet = !levels.is_empty();
                        }
                        RecorderEvent::AudioSources(states) => {
                            this.audio_sources = states.clone();
                            if states.is_empty() {
                                this.audio_levels.clear();
                                this.meters.clear();
                            }
                        }
                        RecorderEvent::AudioTestStarted => this.audio_testing = true,
                        RecorderEvent::AudioTestFinished { path, error } => {
                            this.audio_testing = false;
                            this.audio_levels.clear();
                            this.meters.clear();
                            this.audio_sources.clear();
                            this.audio_test_path =
                                path.as_ref().map(|path| path.display().to_string());
                            if let Some(error) = error {
                                this.recording.fail(error.clone());
                            }
                        }
                        RecorderEvent::RecordingFinished { .. }
                        | RecorderEvent::RecordingFailed { .. } => {
                            this.audio_levels.clear();
                            this.meters.clear();
                        }
                        RecorderEvent::Stopped => {
                            this.audio_levels.clear();
                            this.meters.clear();
                            this.audio_sources.clear();
                            this.audio_testing = false;
                        }
                        _ => {}
                    }
                    this.recording.apply(event);
                    this.refresh_active_video();
                    // Rows of an open source menu are list indexes; a new list or
                    // a started session makes them meaningless.
                    if sources_listed || !this.recording.settings_enabled() {
                        this.source_menu = None;
                    }
                    if wrote_file {
                        this.refresh_storage(cx);
                        this.refresh_recent_clips(window, cx);
                    }
                    if sources_listed {
                        this.maybe_autostart_replay();
                    }
                    // The overlay toast covers clip saves unless in-app toasts
                    // already show them in the focused main window.
                    if let Some(toast) = toast
                        && !(this.config.notifications.in_app && this.main_in_front())
                    {
                        this.notifier.show(toast, &this.config.notifications, cx);
                    }
                    for notice in notices {
                        this.notify_user(notice, cx);
                    }
                    this.sync_tray();
                    if !quiet {
                        cx.notify();
                    }
                })
                .await;
                if updated.is_none() {
                    return;
                }
            }
            // Channel closed without a final event: the worker is gone.
            let _ = this.update(cx, |this, cx| {
                this.audio_testing = false;
                this.audio_levels.clear();
                this.meters.clear();
                this.audio_sources.clear();
                this.storage_projection_run.clear();
                if this.recording.phase != Phase::Idle || !this.recording.replay.is_off() {
                    this.recording.apply(RecorderEvent::Stopped);
                }
                cx.notify();
            });
        });
        let failures = cx.spawn(async move |this, cx| {
            while let Ok(message) = hotkey_failures.recv().await {
                let shown = this.update(cx, |this, cx| {
                    if this.config.notifications.in_app && this.main_in_front() {
                        let notice = crate::notification_center::Notice::new(
                            crate::feedback::CLIP_SAVE_KEY,
                            crate::notification_center::Category::ClipSave,
                            crate::notification_center::Severity::Error,
                            "Clip not saved",
                            message,
                        );
                        this.notify_user(notice, cx);
                    } else {
                        let toast = Toast::Failed { message };
                        this.notifier.show(toast, &this.config.notifications, cx);
                    }
                });
                if shown.is_err() {
                    return;
                }
            }
        });
        let hotkey = config
            .replay
            .hotkey
            .parse()
            .unwrap_or_else(|_| Hotkey::default_save_replay());
        let replay_autostart_pending = config.replay.enabled;
        let focus = cx.focus_handle();
        window.focus(&focus);
        let nav_focus = (0..Page::ALL.len())
            .map(|index| cx.focus_handle().tab_index(index as isize).tab_stop(true))
            .collect();

        let accent_input = settings::accent_input(&config, window, cx);
        let settings_scroll = gpui::ScrollHandle::new();
        // System mode follows the OS light/dark setting the moment it changes.
        cx.observe_window_appearance(window, |this, _, cx| {
            crate::theme::apply(&this.config.appearance, cx)
        })
        .detach();

        let directory = config
            .recording_options()
            .map(|o| o.output_directory)
            .unwrap_or_default();
        let library =
            cx.new(|cx| clips::ClipLibrary::new(config.library.clone(), directory, window, cx));
        cx.subscribe_in(&library, window, |this, _, event, window, cx| match event {
            clips::LibraryEvent::Changed => {
                this.on_library_changed(cx);
                this.sync_tray();
                cx.notify()
            }
            clips::LibraryEvent::Settings(settings) => {
                this.config.library = settings.clone();
                this.persist_config(cx);
            }
            clips::LibraryEvent::Editor(path) => this.open_in_editor(path.clone(), window, cx),
        })
        .detach();
        let main_hwnd = crate::notifications::hwnd_of(window);
        let (tray, tray_commands) = match tray {
            Some((tray, commands)) => {
                let task = cx.spawn_in(window, async move |this, cx| {
                    while let Ok(command) = commands.recv().await {
                        let mut command = Some(command);
                        let alive = update_view(&this, cx, |this, window, cx| {
                            if let Some(command) = command.take() {
                                this.run_tray_command(command, window, cx);
                            }
                        })
                        .await;
                        if alive.is_none() {
                            return;
                        }
                    }
                });
                (Some(tray), Some(task))
            }
            None => (None, None),
        };
        if tray.is_some()
            && let Some(hwnd) = main_hwnd
        {
            Self::close_to_tray(hwnd, window, cx);
        }
        let mut this = Self {
            handle,
            hotkeys,
            notifier: Notifier::new(),
            hotkey,
            hotkey_error: None,
            hotkey_recorder: None,
            config,
            recording: RecordingState::default(),
            storage_projection_cache: StorageProjectionCache::default(),
            storage_projection_run: StorageProjectionRun::default(),
            _events: task,
            _hotkey_failures: failures,
            feedback: quick_actions::Feedback {
                tray,
                main_hwnd,
                ..Default::default()
            },
            _tray_commands: tray_commands,
            active_page: Page::Dashboard,
            focus,
            nav_focus,
            palette: None,
            config_persistence: Default::default(),
            audio_devices: Vec::new(),
            audio_apps: Vec::new(),
            audio_capabilities: None,
            audio_menu: None,
            audio_loading: false,
            audio_levels: Vec::new(),
            meters: Default::default(),
            meter_ticker: None,
            audio_sources: Vec::new(),
            audio_add: None,
            audio_add_focus: cx.focus_handle(),
            audio_add_note: None,
            audio_notice: None,
            audio_gain_dirty: false,
            audio_focus: Default::default(),
            audio_focused: Default::default(),
            audio_testing: false,
            audio_test_path: None,
            replay_autostart_pending,
            capture_menu: None,
            // Indexed by `CaptureMenu` order. Tab follows the screen: the capture
            // bar (resolution 4, fps 5, replay length 6, hotkey 7, replay switch 8)
            // after the sidebar (0-3), then the Capture page: source 9, quality 10,
            // bitrate 11. Settings controls follow from 12.
            capture_focus: [4, 5, 10, 11, 6]
                .map(|index| cx.focus_handle().tab_index(index).tab_stop(true)),
            replay_focus: cx.focus_handle().tab_index(8).tab_stop(true),
            active_video: None,
            storage: StorageStatus::Unknown,
            source_menu: None,
            // First control of the Capture page, after the capture bar (4-8).
            source_focus: cx.focus_handle().tab_index(9).tab_stop(true),
            library,
            editor: None,
            accent_input,
            settings_menu: None,
            settings_scroll,
            settings_section: 0,
            startup: None,
            startup_error: None,
            settings_notice: None,
            diagnostics_notice: None,
            diagnostics_exporting: false,
        };
        this.storage_projection_cache.refresh(&this.config);
        this.refresh_storage(cx);
        this.refresh_recent_clips(window, cx);
        this.send(RecorderCommand::RefreshSources);
        this.refresh_audio_lists(cx);
        this.register_hotkey(cx);
        this
    }

    /// Saves the config off the UI thread; failures show up in the error banner.
    pub(super) fn persist_config(&mut self, cx: &mut Context<Self>) {
        self.storage_projection_cache.refresh(&self.config);
        cx.notify();
        let config = self.config.clone();
        let persistence = self.config_persistence.clone();
        let revision = persistence.request();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    persistence.save(revision, || {
                        config.save().map_err(|error| error.to_string())
                    })
                })
                .await;
            if let Err(message) = result {
                clipper_diagnostics::log::error!("settings could not be saved: {message}");
                this.update(cx, |this, cx| {
                    this.library.update(cx, |library, cx| {
                        library.show_error(
                            format!("Could not save library preferences: {message}"),
                            cx,
                        )
                    });
                    this.recording
                        .fail(format!("Could not save settings: {message}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Queues a command; a full or closed queue surfaces as an error banner.
    fn send(&mut self, command: RecorderCommand) -> bool {
        match self.handle.send(command) {
            Ok(()) => true,
            Err(error) => {
                self.recording.fail(error.to_string());
                false
            }
        }
    }

    /// Opens `path` in the Editor page, replacing (and releasing) any open clip.
    pub(super) fn open_in_editor(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same = self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).path() == path);
        if !same {
            if let Some(old) = self.editor.take() {
                old.update(cx, |old, _| old.release_images(window));
            }
            let editor = cx.new(|cx| editor::ClipEditor::new(path, window, cx));
            cx.subscribe_in(
                &editor,
                window,
                |this, _, event: &editor::EditorEvent, window, cx| {
                    if matches!(event, editor::EditorEvent::PreviewChanged) {
                        cx.notify();
                        return;
                    }
                    this.library.update(cx, |library, cx| library.refresh(cx));
                    this.refresh_recent_clips(window, cx);
                    this.refresh_storage(cx);
                },
            )
            .detach();
            self.editor = Some(editor);
        }
        self.navigate(Page::Editor, window, cx);
    }

    /// Reads free space on the recordings volume off the UI thread. Runs at
    /// startup and after each saved clip or recording; nothing polls.
    fn refresh_storage(&mut self, cx: &mut Context<Self>) {
        let config = self.config.clone();
        cx.spawn(async move |this, cx| {
            let status = cx
                .background_spawn(async move {
                    match config.recording_options() {
                        Ok(options) => storage_status::load(&options.output_directory),
                        Err(error) => StorageStatus::Unavailable(error.to_string()),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.on_storage_status(&status, cx);
                if this.storage != status {
                    this.storage = status;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.library.update(cx, |library, cx| {
            library.set_visible(page == Page::Clips, window, cx)
        });
        if page == Page::Clips {
            self.refresh_recent_clips(window, cx);
        }
        // An open capture menu belongs to the page being left.
        self.capture_menu = None;
        self.settings_menu = None;
        if page == Page::Settings && self.active_page != page {
            // Picks up changes made outside clipuh (Task Manager, Settings app).
            self.sync_startup(None, cx);
        }
        let palette_was_open = self.palette.take().is_some();
        if palette_was_open {
            window.focus(&self.focus);
        }
        if self.active_page != page
            && let Some(editor) = &self.editor
        {
            if page == Page::Editor {
                editor.update(cx, |editor, cx| editor.resume(window, cx));
            } else if self.active_page == Page::Editor {
                editor.update(cx, |editor, cx| editor.suspend(window, cx));
            }
        }
        if self.active_page != page || palette_was_open {
            self.active_page = page;
            cx.notify();
        }
    }

    fn toggle_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.take().is_some() {
            window.focus(&self.focus);
        } else {
            let focus = cx.focus_handle();
            window.focus(&focus);
            self.palette = Some(CommandPalette {
                focus,
                query: String::new(),
                selected: 0,
            });
        }
        cx.notify();
    }

    fn move_palette_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(palette) = &mut self.palette {
            let len = palette.matches().len() as isize;
            if len > 0 {
                palette.selected = (palette.selected as isize + delta).rem_euclid(len) as usize;
                cx.notify();
            }
        }
    }

    fn confirm_palette(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let page = self
            .palette
            .as_ref()
            .and_then(|palette| palette.matches().get(palette.selected).copied());
        if let Some(page) = page {
            self.navigate(page, window, cx);
        }
    }

    fn dismiss_palette(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            window.focus(&self.focus);
            cx.notify();
        }
    }

    /// Typing filters the palette. Navigation keys arrive as actions, not here.
    fn palette_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = &mut self.palette else {
            return;
        };
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }

        let changed = if keystroke.key == "backspace" {
            palette.query.pop().is_some()
        } else if let Some(text) = keystroke
            .key_char
            .as_deref()
            .filter(|text| !text.chars().any(char::is_control))
        {
            palette.query.push_str(text);
            true
        } else {
            false
        };

        if changed {
            palette.selected = 0;
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// The one sidebar of every page (sidebar reference: the left column of
    /// the approved screenshots): logo, four tall navigation rows with a
    /// filled accent active row, storage at the bottom.
    fn sidebar(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = *cx.theme();
        let navigation = Page::ALL
            .into_iter()
            .zip(&self.nav_focus)
            .map(|(page, focus)| {
                let active = self.active_page == page;
                let focused = focus.is_focused(window);
                div()
                    .id(("nav", page.index()))
                    .track_focus(focus)
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .h(px(NAV_ITEM_HEIGHT))
                    .px_3()
                    .rounded_lg()
                    .border_1()
                    .cursor_pointer()
                    .text_sm()
                    .map(|this| {
                        if active {
                            this.bg(t.colors.accent.opacity(ACTIVE_NAV_ALPHA))
                                .border_color(t.colors.accent.opacity(ACTIVE_NAV_ALPHA))
                                .text_color(t.colors.accent_foreground)
                                .font_weight(FontWeight::MEDIUM)
                        } else {
                            this.border_color(gpui::transparent_black())
                                .text_color(t.colors.text)
                                .hover(|style| style.bg(t.colors.surface_hover))
                        }
                    })
                    .when(focused, |this| this.border_color(t.colors.focus_ring))
                    .tooltip(tooltip(format!(
                        "{} ({})",
                        page.label(),
                        page.shortcut().join("+")
                    )))
                    .child(
                        icon(page.icon(), px(16.))
                            .when(!active, |icon| icon.text_color(t.colors.text_secondary)),
                    )
                    .child(div().flex_1().truncate().child(page.label()))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.navigate(page, window, cx)),
                    )
                    .on_action(cx.listener(move |this, _: &Confirm, window, cx| {
                        this.navigate(page, window, cx)
                    }))
            });

        div()
            .id("sidebar")
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(16.))
            .px_3()
            .pt_5()
            .pb_5()
            // Short windows scroll the sidebar instead of clipping storage.
            .overflow_y_scroll()
            .bg(t.colors.sidebar)
            .border_r_1()
            .border_color(t.colors.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .px_2()
                    .h(px(32.))
                    .child(img(brand_logo()).size(px(24.)).flex_none())
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("clipuh"),
                    )
                    // Package version of this build (workspace version).
                    .child(
                        div()
                            .flex_none()
                            .mt(px(4.))
                            .text_xs()
                            .text_color(t.colors.text_muted)
                            .child(version_label()),
                    ),
            )
            .child(div().flex().flex_col().gap_1().children(navigation))
            .child(
                div()
                    .mt_auto()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        // The command palette's only visible entry point.
                        div()
                            .id("open-command-palette")
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(t.metrics.row_height)
                            .px_2()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(t.colors.surface_hover))
                            .text_xs()
                            .text_color(t.colors.text_muted)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_palette(&ToggleCommandPalette, window, cx)
                            }))
                            .child("Commands")
                            .child(shortcut(&t, &["Ctrl", "K"])),
                    )
                    .child(storage_indicator(&t, &self.storage)),
            )
    }

    fn page_content(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        match self.active_page {
            Page::Dashboard => dashboard::content(self, window, cx),
            Page::Clips => div().h_full().child(self.library.clone()),
            Page::Editor => match &self.editor {
                Some(editor) => div().child(editor.clone()),
                None => editor::empty(cx.theme()).child(
                    crate::components::button(
                        cx.theme(),
                        "editor-open-library",
                        "Open Library",
                        crate::components::ButtonVariant::Primary,
                        None,
                        true,
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.navigate(Page::Clips, window, cx)),
                    ),
                ),
            },
            Page::Settings => settings::content(self, window, cx),
        }
    }

    fn command_palette(&self, palette: &CommandPalette, cx: &mut Context<Self>) -> Div {
        let t = *cx.theme();
        let matches = palette.matches();
        let rows = matches.iter().enumerate().map(|(index, page)| {
            let page = *page;
            list_item(
                &t,
                ("palette", index),
                page.icon(),
                format!("Open {}", page.label()),
                Some(page.shortcut()),
                index == palette.selected,
            )
            .on_click(cx.listener(move |this, _, window, cx| this.navigate(page, window, cx)))
        });

        let query = if palette.query.is_empty() {
            div()
                .text_color(t.colors.text_muted)
                .child("Search commands…")
        } else {
            div().child(palette.query.clone())
        };

        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(96.))
            .bg(t.colors.scrim)
            .child(
                // Backdrop: blocks clicks to the shell below and closes the palette.
                div()
                    .id("command-palette-backdrop")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dismiss_palette(&Dismiss, window, cx)
                    })),
            )
            .child(
                div()
                    .id("command-palette")
                    .key_context(PALETTE_CONTEXT)
                    .occlude()
                    .track_focus(&palette.focus)
                    .on_action(
                        cx.listener(|this, _: &SelectNext, _, cx| {
                            this.move_palette_selection(1, cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                        this.move_palette_selection(-1, cx)
                    }))
                    .on_action(cx.listener(Self::confirm_palette))
                    .on_action(cx.listener(Self::dismiss_palette))
                    .on_key_down(cx.listener(Self::palette_key_down))
                    .w(px(520.))
                    .h_auto()
                    .flex()
                    .flex_col()
                    .rounded_xl()
                    .border_1()
                    .border_color(t.colors.border_strong)
                    .bg(t.colors.surface)
                    .shadow_lg()
                    .text_color(t.colors.text)
                    .child(
                        div()
                            .h(px(48.))
                            .px_4()
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(t.colors.border)
                            .text_sm()
                            .child(query),
                    )
                    .child(div().p_2().flex().flex_col().gap_0p5().children(rows).when(
                        matches.is_empty(),
                        |this| {
                            this.child(
                                div()
                                    .px_2()
                                    .py_3()
                                    .text_sm()
                                    .text_color(t.colors.text_muted)
                                    .child("No matching commands"),
                            )
                        },
                    ))
                    .child(
                        div()
                            .h(px(36.))
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_4()
                            .border_t_1()
                            .border_color(t.colors.border)
                            .text_xs()
                            .text_color(t.colors.text_muted)
                            .child(hint(&t, &["↑", "↓"], "Navigate"))
                            .child(hint(&t, &["Enter"], "Open"))
                            .child(hint(&t, &["Esc"], "Close")),
                    ),
            )
    }
}

/// Sidebar width at the default density; the capture bar takes the rest.
const SIDEBAR_WIDTH: f32 = 200.0;
/// Navigation row height and the active row's accent fill (references: tall
/// rows, a solid but dimmed accent so the label stays readable).
const NAV_ITEM_HEIGHT: f32 = 36.0;
const ACTIVE_NAV_ALPHA: f32 = 0.6;

/// Free space on the recordings volume with a usage bar, laid out like the
/// references: "Storage · N free", bar, "used / total". Shows a neutral state
/// until the first (background) read finishes or when the volume cannot be
/// read; never an invented number.
fn storage_indicator(t: &Theme, storage: &StorageStatus) -> Div {
    let free = match storage {
        StorageStatus::Ready(usage) => usage.free_label(),
        StorageStatus::Unknown => "Checking…".to_owned(),
        StorageStatus::Unavailable(_) => "Unavailable".to_owned(),
    };
    let heading = div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(icon(Icon::Drive, px(16.)).text_color(t.colors.text_secondary))
        .child(div().flex_1().child("Storage"))
        .child(
            div()
                .text_color(t.colors.text_secondary)
                .truncate()
                .child(free),
        );
    let base = div().px_2().flex().flex_col().gap_2().child(heading);
    match storage {
        StorageStatus::Ready(usage) => base
            .child(
                div()
                    .h(px(8.))
                    .w_full()
                    .rounded_full()
                    .bg(t.colors.surface_active)
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(usage.used_fraction()))
                            .rounded_full()
                            .bg(t.colors.accent),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(t.colors.text_muted)
                    .truncate()
                    .child(usage.used_label()),
            ),
        StorageStatus::Unknown => base,
        StorageStatus::Unavailable(reason) => base.child(
            div()
                .text_xs()
                .text_color(t.colors.text_muted)
                .truncate()
                .child(reason.clone()),
        ),
    }
}

fn hint(t: &Theme, keys: &[&'static str], label: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .children(keys.iter().map(|key| kbd(t, *key)))
        .child(label)
}

impl Render for ClipperWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.active_page == Page::Editor
            && let Some(editor) = &self.editor
            && editor.read(cx).preview_fullscreen
        {
            return div()
                .id("clipper-fullscreen-preview")
                .size_full()
                .child(editor.clone());
        }
        let full_height = matches!(self.active_page, Page::Clips | Page::Settings);
        let t = *cx.theme();
        self.snapshot_audio_focus(window);
        // Covers state changes made without a recorder event (e.g. replay
        // requested: Starting); set_state ignores unchanged snapshots.
        self.sync_tray();
        let palette = self
            .palette
            .as_ref()
            .map(|palette| self.command_palette(palette, cx));

        div()
            .id("clipper-window")
            .key_context(WINDOW_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &OpenDashboard, window, cx| {
                this.navigate(Page::Dashboard, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenClips, window, cx| {
                    this.navigate(Page::Clips, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &OpenEditor, window, cx| {
                this.navigate(Page::Editor, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.navigate(Page::Settings, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextPage, window, cx| {
                this.navigate(this.active_page.offset(1), window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousPage, window, cx| {
                this.navigate(this.active_page.offset(-1), window, cx)
            }))
            .on_action(cx.listener(Self::toggle_palette))
            // Tab must not move focus out of an open palette, or its keys stop reaching it.
            // An open capture menu only takes keys while its trigger is focused,
            // so moving focus closes it.
            .on_action(cx.listener(|this, _: &FocusNext, window, cx| {
                if this.palette.is_none() {
                    this.capture_close(cx);
                    window.focus_next()
                }
            }))
            .on_action(cx.listener(|this, _: &FocusPrevious, window, cx| {
                if this.palette.is_none() {
                    this.capture_close(cx);
                    window.focus_prev()
                }
            }))
            .relative()
            .size_full()
            .flex()
            .bg(t.colors.background)
            .text_color(t.colors.text)
            .child(self.sidebar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.capture_bar(window, cx))
                    .child(
                        div()
                            .id("main-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(
                                div()
                                    .w_full()
                                    // Library and Settings scroll inside themselves.
                                    .when(full_height, |d| d.h_full())
                                    // Pages use the full width beside the sidebar, inset
                                    // like the capture bar (references).
                                    .px(px(capture_bar::PAGE_GUTTER))
                                    .pt_4()
                                    .pb_6()
                                    .child(self.page_content(window, cx)),
                            ),
                    ),
            )
            .children(self.toast_stack(cx))
            .children(palette)
    }
}

#[cfg(test)]
mod tests {
    use super::Page;

    #[test]
    fn version_label_matches_the_package_version() {
        assert_eq!(
            super::version_label(),
            format!("v{}", env!("CARGO_PKG_VERSION"))
        );
        // The app binary reports the same workspace version.
        let workspace = include_str!("../../../../Cargo.toml");
        let declared = workspace
            .lines()
            .find_map(|line| line.trim().strip_prefix("version = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("workspace version");
        assert_eq!(declared, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn page_offset_wraps_in_both_directions() {
        assert_eq!(Page::Dashboard.offset(1), Page::Clips);
        assert_eq!(Page::Settings.offset(1), Page::Dashboard);
        assert_eq!(Page::Dashboard.offset(-1), Page::Settings);
    }

    #[test]
    fn page_shortcuts_follow_navigation_order() {
        for (index, page) in Page::ALL.into_iter().enumerate() {
            assert_eq!(page.index(), index);
            assert_eq!(page.shortcut()[1], (index + 1).to_string());
        }
    }
}
