mod audio;
mod capture_bar;
mod clips;
mod dashboard;
mod editor;
mod recent_card;
mod settings;
mod source_card;
mod storage_card;

use clipper_core::hotkey::Hotkey;
use clipper_core::recording::{RecorderCommand, RecorderEvent, RecorderHandle};
use clipper_platform::windows::hotkey::HotkeyHandle;
use clipper_storage::AppConfig;
use gpui::prelude::*;
use gpui::{
    App, Context, Div, Entity, FocusHandle, FontWeight, KeyBinding, KeyDownEvent, NoAction, Render,
    Task, Window, actions, div, img, px,
};

use crate::components::{kbd, list_item, shortcut};
use crate::notifications::{Notifier, Toast};
use crate::state::{Phase, RecordingState, StorageProjectionCache, StorageProjectionRun};
use crate::storage_status::{self, StorageStatus};
use crate::theme::{ActiveTheme, Theme};

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

    fn glyph(self) -> &'static str {
        match self {
            Self::Dashboard => "◧",
            Self::Clips => "▤",
            Self::Editor => "◫",
            Self::Settings => "◎",
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
    audio_levels_drawn_at: Option<std::time::Instant>,
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
    /// Keyboard focus for the notification toggles on the Settings page.
    notify_focus: [FocusHandle; 4],
}

impl ClipperWindow {
    pub fn new(
        handle: RecorderHandle,
        hotkeys: HotkeyHandle,
        hotkey_failures: async_channel::Receiver<String>,
        config: AppConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = handle.events();
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
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
                let updated = this.update_in(cx, |this, window, cx| {
                    let mut quiet = false;
                    this.storage_projection_run.apply_event(&event);
                    match &event {
                        RecorderEvent::AudioLevels(levels) => {
                            // Meters only exist on the Capture page, and only visible
                            // change (>= 10 Hz apart) is worth a redraw.
                            let since = this.audio_levels_drawn_at.map(|t| t.elapsed());
                            let redraw = this.active_page == Page::Dashboard
                                && audio::levels_need_redraw(&this.audio_levels, levels, since);
                            if redraw {
                                this.audio_levels_drawn_at = Some(std::time::Instant::now());
                                this.audio_levels = levels.clone();
                            } else if this.active_page != Page::Dashboard {
                                this.audio_levels = levels.clone();
                            }
                            quiet = !redraw && !levels.is_empty();
                        }
                        RecorderEvent::AudioSources(states) => {
                            this.audio_sources = states.clone();
                            if states.is_empty() {
                                this.audio_levels.clear();
                            }
                        }
                        RecorderEvent::AudioTestStarted => this.audio_testing = true,
                        RecorderEvent::AudioTestFinished { path, error } => {
                            this.audio_testing = false;
                            this.audio_levels.clear();
                            this.audio_sources.clear();
                            this.audio_test_path =
                                path.as_ref().map(|path| path.display().to_string());
                            if let Some(error) = error {
                                this.recording.fail(error.clone());
                            }
                        }
                        RecorderEvent::RecordingFinished { .. }
                        | RecorderEvent::RecordingFailed { .. } => this.audio_levels.clear(),
                        RecorderEvent::Stopped => {
                            this.audio_levels.clear();
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
                    if let Some(toast) = toast {
                        this.notifier.show(toast, &this.config.notifications, cx);
                    }
                    if !quiet {
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    return;
                }
            }
            // Channel closed without a final event: the worker is gone.
            let _ = this.update(cx, |this, cx| {
                this.audio_testing = false;
                this.audio_levels.clear();
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
                    let toast = Toast::Failed { message };
                    this.notifier.show(toast, &this.config.notifications, cx);
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
            clips::LibraryEvent::Changed => cx.notify(),
            clips::LibraryEvent::Settings(settings) => {
                this.config.library = settings.clone();
                this.persist_config(cx);
            }
            clips::LibraryEvent::Editor(path) => this.open_in_editor(path.clone(), window, cx),
        })
        .detach();
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
            audio_levels_drawn_at: None,
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
            // Settings page, after every Capture page control (0-11).
            notify_focus: std::array::from_fn(|index| {
                cx.focus_handle()
                    .tab_index(12 + index as isize)
                    .tab_stop(true)
            }),
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
            self.editor = Some(cx.new(|cx| editor::ClipEditor::new(path, window, cx)));
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

    fn sidebar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let t = *cx.theme();
        let navigation = Page::ALL
            .into_iter()
            .zip(&self.nav_focus)
            .map(|(page, focus)| {
                let focused = focus.is_focused(window);
                list_item(
                    &t,
                    ("nav", page.index()),
                    page.glyph(),
                    page.label(),
                    Some(page.shortcut()),
                    self.active_page == page,
                )
                .track_focus(focus)
                .border_1()
                .border_color(if focused {
                    t.colors.focus_ring
                } else {
                    gpui::transparent_black()
                })
                .on_click(cx.listener(move |this, _, window, cx| this.navigate(page, window, cx)))
                .on_action(
                    cx.listener(move |this, _: &Confirm, window, cx| {
                        this.navigate(page, window, cx)
                    }),
                )
            });

        div()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(t.metrics.section_gap)
            .px_3()
            .py_4()
            .bg(t.colors.sidebar)
            .border_r_1()
            .border_color(t.colors.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(28.))
                    .child(img(brand_logo()).size(px(22.)).flex_none())
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("clipuh"),
                    ),
            )
            .child(div().flex().flex_col().gap_0p5().children(navigation))
            .child(
                div()
                    .mt_auto()
                    .flex()
                    .flex_col()
                    .gap_3()
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

    fn page_content(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        match self.active_page {
            Page::Dashboard => dashboard::content(self, window, cx),
            Page::Clips => div().h_full().child(self.library.clone()),
            Page::Editor => match &self.editor {
                Some(editor) => div().child(editor.clone()),
                None => editor::empty(cx.theme()),
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
                page.glyph(),
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
const SIDEBAR_WIDTH: f32 = 236.0;

/// Free space on the recordings volume with a compact usage bar. Shows a
/// neutral state until the first (background) read finishes or when the
/// volume cannot be read; never an invented number.
fn storage_indicator(t: &Theme, storage: &StorageStatus) -> Div {
    let caption = div()
        .text_xs()
        .text_color(t.colors.text_muted)
        .child("Storage");
    let base = div().px_2().flex().flex_col().gap_1p5().child(caption);
    match storage {
        StorageStatus::Ready(usage) => base
            .child(
                div()
                    .h(px(4.))
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
                div().flex().justify_between().gap_2().text_xs().child(
                    div()
                        .text_color(t.colors.text_secondary)
                        .child(usage.free_label()),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(t.colors.text_muted)
                    .truncate()
                    .child(usage.used_label()),
            ),
        StorageStatus::Unknown => base.child(
            div()
                .text_xs()
                .text_color(t.colors.text_muted)
                .child("Checking…"),
        ),
        StorageStatus::Unavailable(reason) => base.child(
            div()
                .text_xs()
                .text_color(t.colors.text_muted)
                .truncate()
                .child(format!("Unavailable: {reason}")),
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
        let t = *cx.theme();
        self.snapshot_audio_focus(window);
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
                                    .when(self.active_page == Page::Clips, |d| d.h_full())
                                    .when(self.active_page != Page::Clips, |d| {
                                        d.max_w(px(960.)).mx_auto()
                                    })
                                    .px_8()
                                    .py_6()
                                    .child(self.page_content(window, cx)),
                            ),
                    ),
            )
            .children(palette)
    }
}

#[cfg(test)]
mod tests {
    use super::Page;

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
