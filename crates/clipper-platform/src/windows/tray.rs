//! Notification-area (tray) icon on its own message-pump thread (zero CPU when idle).
//!
//! The UI thread owns the authoritative app state and pushes a [`TrayMenuState`]
//! snapshot whenever it changes; the menu is built from the latest snapshot each
//! time it opens, so labels never drift from the real state. Menu picks and icon
//! clicks come back as [`TrayCommand`]s through the `on_command` callback, which
//! runs on the tray thread and must not block.
//!
//! Desktop notifications use the icon's balloon (`NIF_INFO`). Windows 10/11 show
//! these as native toasts without an AppUserModelID or Start Menu shortcut, and
//! `NIIF_RESPECT_QUIET_TIME` keeps them out of Focus Assist / fullscreen games.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIIF_NOSOUND,
    NIIF_RESPECT_QUIET_TIME, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_BALLOONUSERCLICK, NIN_SELECT, NOTIFY_ICON_DATA_FLAGS, NOTIFY_ICON_INFOTIP_FLAGS,
    NOTIFY_ICON_MESSAGE, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW,
    GetSystemMetrics, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON, IsIconic, IsWindowVisible,
    LR_DEFAULTCOLOR, LoadIconW, LoadImageW, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON,
    SM_CYSMICON, SW_HIDE, SW_RESTORE, SW_SHOW, SetForegroundWindow, SetMenuDefaultItem, ShowWindow,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP,
    WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

/// Callback message the shell sends for icon events.
const WM_TRAY_ICON: u32 = WM_APP + 1;
/// The UI pushed a new menu state or a balloon.
const WM_TRAY_SYNC: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;
/// `NIN_SELECT | NINF_KEY`: the icon was activated with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
/// Pending balloons beyond this are dropped (oldest first): the shell shows one
/// at a time anyway, so a burst must not queue up stale alerts.
const MAX_PENDING_BALLOONS: usize = 3;

/// What a tray click or menu pick asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    OpenMain,
    ToggleReplay,
    SaveClip,
    OpenLibrary,
    OpenRecentClip,
    OpenClipFolder,
    OpenSettings,
    Exit,
}

/// Instant replay as the tray shows it; mirrors the app's replay phase.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TrayReplay {
    #[default]
    Off,
    Starting,
    On,
    Stopping,
    /// Replay cannot be toggled right now (e.g. a manual recording runs).
    Unavailable,
}

/// Snapshot of the app state the tray menu needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayMenuState {
    pub replay: TrayReplay,
    /// A clip save can be requested right now.
    pub can_save: bool,
    /// The library knows at least one clip.
    pub has_recent_clip: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalloonKind {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Balloon {
    pub kind: BalloonKind,
    pub title: String,
    pub body: String,
    pub sound: bool,
}

/// One row of the tray menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        command: TrayCommand,
        label: &'static str,
        enabled: bool,
        checked: bool,
        default: bool,
    },
    Separator,
}

fn item(command: TrayCommand, label: &'static str, enabled: bool) -> MenuEntry {
    MenuEntry::Item {
        command,
        label,
        enabled,
        checked: false,
        default: false,
    }
}

/// The menu for `state`, top to bottom.
pub fn menu_entries(state: &TrayMenuState) -> Vec<MenuEntry> {
    let (replay_label, replay_enabled, checked) = match state.replay {
        TrayReplay::Off => ("Instant Replay: Off", true, false),
        TrayReplay::On => ("Instant Replay: On", true, true),
        TrayReplay::Starting => ("Instant Replay: Starting…", false, false),
        TrayReplay::Stopping => ("Instant Replay: Stopping…", false, true),
        TrayReplay::Unavailable => ("Instant Replay: Unavailable", false, false),
    };
    vec![
        MenuEntry::Item {
            command: TrayCommand::OpenMain,
            label: "Open clipuh",
            enabled: true,
            checked: false,
            default: true,
        },
        MenuEntry::Separator,
        MenuEntry::Item {
            command: TrayCommand::ToggleReplay,
            label: replay_label,
            enabled: replay_enabled,
            checked,
            default: false,
        },
        item(TrayCommand::SaveClip, "Save Last Clip", state.can_save),
        MenuEntry::Separator,
        item(TrayCommand::OpenLibrary, "Open Library", true),
        item(
            TrayCommand::OpenRecentClip,
            "Open Recent Clip",
            state.has_recent_clip,
        ),
        item(TrayCommand::OpenClipFolder, "Open Clip Folder", true),
        item(TrayCommand::OpenSettings, "Settings", true),
        MenuEntry::Separator,
        item(TrayCommand::Exit, "Exit clipuh", true),
    ]
}

/// Hover text for the icon.
pub fn tooltip(state: &TrayMenuState) -> &'static str {
    match state.replay {
        TrayReplay::On => "clipuh · Instant Replay on",
        TrayReplay::Starting => "clipuh · Instant Replay starting",
        TrayReplay::Stopping => "clipuh · Instant Replay stopping",
        TrayReplay::Off | TrayReplay::Unavailable => "clipuh · Instant Replay off",
    }
}

fn command_id(command: TrayCommand) -> usize {
    command as usize + 1
}

fn command_from_id(id: usize) -> Option<TrayCommand> {
    use TrayCommand::*;
    [
        OpenMain,
        ToggleReplay,
        SaveClip,
        OpenLibrary,
        OpenRecentClip,
        OpenClipFolder,
        OpenSettings,
        Exit,
    ]
    .into_iter()
    .find(|c| command_id(*c) == id)
}

#[derive(Default)]
struct Shared {
    state: TrayMenuState,
    state_dirty: bool,
    balloons: VecDeque<Balloon>,
}

fn push_balloon(queue: &mut VecDeque<Balloon>, balloon: Balloon) {
    if queue.len() >= MAX_PENDING_BALLOONS {
        queue.pop_front();
    }
    queue.push_back(balloon);
}

/// Owner of the tray thread; dropping it removes the icon and joins the thread.
pub struct Tray {
    handle: TrayHandle,
    thread: Option<JoinHandle<()>>,
}

/// Cheap, `Send` handle for pushing state and balloons to the tray thread.
#[derive(Clone)]
pub struct TrayHandle {
    hwnd: isize,
    shared: Arc<Mutex<Shared>>,
}

impl std::fmt::Debug for TrayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrayHandle").finish_non_exhaustive()
    }
}

impl Tray {
    /// Creates the hidden owner window and adds the icon. `on_command` runs on
    /// the tray thread and must not block.
    pub fn spawn(on_command: impl Fn(TrayCommand) + Send + 'static) -> std::io::Result<Self> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let thread_shared = Arc::clone(&shared);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<isize, String>>();
        let thread = std::thread::Builder::new()
            .name("clipper-tray".into())
            .spawn(move || run(thread_shared, Box::new(on_command), ready_tx))?;
        match ready_rx.recv() {
            Ok(Ok(hwnd)) => Ok(Self {
                handle: TrayHandle { hwnd, shared },
                thread: Some(thread),
            }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(std::io::Error::other(message))
            }
            Err(_) => {
                let _ = thread.join();
                Err(std::io::Error::other("tray thread failed to start"))
            }
        }
    }

    pub fn handle(&self) -> TrayHandle {
        self.handle.clone()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // WM_CLOSE -> DestroyWindow -> WM_DESTROY removes the icon and quits the loop.
        if self.handle.post(WM_CLOSE)
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
    }
}

impl TrayHandle {
    fn post(&self, msg: u32) -> bool {
        let hwnd = HWND(self.hwnd as *mut core::ffi::c_void);
        // SAFETY: plain value arguments; fails harmlessly if the window is gone.
        unsafe { PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0)) }.is_ok()
    }

    fn with_shared(&self, f: impl FnOnce(&mut Shared)) -> bool {
        match self.shared.lock() {
            Ok(mut shared) => {
                f(&mut shared);
                true
            }
            Err(_) => false,
        }
    }

    /// Replaces the menu/tooltip state. Cheap; the icon updates asynchronously.
    pub fn set_state(&self, state: TrayMenuState) {
        let changed = self.with_shared(|shared| {
            shared.state_dirty |= shared.state != state;
            shared.state = state;
        });
        if changed {
            self.post(WM_TRAY_SYNC);
        }
    }

    /// Shows a native desktop notification from the tray icon. Returns false
    /// when the tray thread is gone; display itself is up to Windows.
    pub fn notify(&self, balloon: Balloon) -> bool {
        self.with_shared(|shared| push_balloon(&mut shared.balloons, balloon))
            && self.post(WM_TRAY_SYNC)
    }
}

/// Shows and focuses a (possibly hidden or minimized) top-level window.
pub fn show_window(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: plain value arguments; a stale handle just fails.
    unsafe {
        let restore = if IsIconic(hwnd).as_bool() {
            SW_RESTORE
        } else {
            SW_SHOW
        };
        let _ = ShowWindow(hwnd, restore);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// True when `hwnd` is visible and the foreground window (the user is in it).
pub fn is_foreground_window(hwnd: isize) -> bool {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: plain queries; a stale handle just compares unequal / not visible.
    unsafe { GetForegroundWindow() == hwnd && IsWindowVisible(hwnd).as_bool() }
}

/// True when `hwnd` is on screen: visible (not hidden to the tray) and not
/// minimized. It may still be covered by other windows.
pub fn is_shown(hwnd: isize) -> bool {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: plain queries; a stale handle just reports not visible.
    unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() }
}

/// Hides a top-level window (it keeps running; [`show_window`] brings it back).
pub fn hide_window(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: plain value arguments; a stale handle just fails.
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
}

struct Context {
    shared: Arc<Mutex<Shared>>,
    on_command: Box<dyn Fn(TrayCommand) + Send>,
    icon: HICON,
    owns_icon: bool,
    taskbar_created: u32,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

fn copy_wide(dst: &mut [u16], text: &str) {
    let max = dst.len().saturating_sub(1);
    let mut n = 0;
    for unit in text.encode_utf16().take(max) {
        dst[n] = unit;
        n += 1;
    }
    dst[n] = 0;
}

fn icon_data(hwnd: HWND, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: flags,
        ..Default::default()
    }
}

fn notify_icon(message: NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) -> bool {
    // SAFETY: `data` is a fully initialized NOTIFYICONDATAW with a correct cbSize.
    unsafe { Shell_NotifyIconW(message, data) }.as_bool()
}

fn add_icon(hwnd: HWND, ctx: &Context) -> bool {
    let state = ctx
        .shared
        .lock()
        .map(|s| s.state.clone())
        .unwrap_or_default();
    let mut data = icon_data(hwnd, NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP);
    data.uCallbackMessage = WM_TRAY_ICON;
    data.hIcon = ctx.icon;
    copy_wide(&mut data.szTip, tooltip(&state));
    if !notify_icon(NIM_ADD, &data) {
        return false;
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    notify_icon(NIM_SETVERSION, &data);
    true
}

fn sync(hwnd: HWND, ctx: &Context) {
    let (state, balloons) = match ctx.shared.lock() {
        Ok(mut shared) => {
            let state = std::mem::take(&mut shared.state_dirty).then(|| shared.state.clone());
            (state, std::mem::take(&mut shared.balloons))
        }
        Err(_) => return,
    };
    if let Some(state) = state {
        let mut data = icon_data(hwnd, NIF_TIP | NIF_SHOWTIP);
        copy_wide(&mut data.szTip, tooltip(&state));
        notify_icon(NIM_MODIFY, &data);
    }
    for balloon in balloons {
        let mut data = icon_data(hwnd, NIF_INFO);
        copy_wide(&mut data.szInfoTitle, &balloon.title);
        copy_wide(&mut data.szInfo, &balloon.body);
        let mut flags: NOTIFY_ICON_INFOTIP_FLAGS = match balloon.kind {
            BalloonKind::Info => NIIF_INFO,
            BalloonKind::Warning => NIIF_WARNING,
            BalloonKind::Error => NIIF_ERROR,
        } | NIIF_RESPECT_QUIET_TIME;
        if !balloon.sound {
            flags |= NIIF_NOSOUND;
        }
        data.dwInfoFlags = flags;
        if !notify_icon(NIM_MODIFY, &data) {
            eprintln!("tray: Windows rejected a desktop notification");
        }
    }
}

fn build_menu(state: &TrayMenuState) -> Option<HMENU> {
    // SAFETY: creating an empty menu has no preconditions.
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    for entry in menu_entries(state) {
        // SAFETY: `menu` is live; label buffers outlive each call.
        unsafe {
            match entry {
                MenuEntry::Separator => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
                MenuEntry::Item {
                    command,
                    label,
                    enabled,
                    checked,
                    default,
                } => {
                    let wide: Vec<u16> = label.encode_utf16().chain([0]).collect();
                    let mut flags = MF_STRING;
                    if !enabled {
                        flags |= MF_GRAYED;
                    }
                    if checked {
                        flags |= MF_CHECKED;
                    }
                    let id = command_id(command);
                    let _ = AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr()));
                    if default {
                        let _ = SetMenuDefaultItem(menu, id as u32, 0);
                    }
                }
            }
        }
    }
    Some(menu)
}

/// Shows the context menu at the cursor and returns the picked command.
fn show_menu(hwnd: HWND, state: &TrayMenuState) -> Option<TrayCommand> {
    let menu = build_menu(state)?;
    let mut cursor = Default::default();
    // SAFETY: `menu` and `hwnd` are live; the owner must be foreground so the
    // menu closes when the user clicks elsewhere (documented TrackPopupMenu
    // requirement), and WM_NULL afterwards lets it dismiss reliably.
    let picked = unsafe {
        let _ = GetCursorPos(&mut cursor);
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            cursor.x,
            cursor.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        picked.0
    };
    usize::try_from(picked).ok().and_then(command_from_id)
}

fn dispatch(command: TrayCommand) {
    CONTEXT.with_borrow(|ctx| {
        if let Some(ctx) = ctx {
            (ctx.on_command)(command);
        }
    });
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY_ICON => {
            // NOTIFYICON_VERSION_4: the event is in the low word of lParam.
            match (lparam.0 as u32) & 0xFFFF {
                WM_CONTEXTMENU => {
                    // No borrow may be held across TrackPopupMenu: its modal
                    // loop re-enters this procedure.
                    let state = CONTEXT.with_borrow(|ctx| {
                        ctx.as_ref()
                            .and_then(|c| c.shared.lock().ok().map(|s| s.state.clone()))
                    });
                    if let Some(command) = state.and_then(|s| show_menu(hwnd, &s)) {
                        dispatch(command);
                    }
                }
                NIN_SELECT | NIN_KEYSELECT | NIN_BALLOONUSERCLICK => {
                    dispatch(TrayCommand::OpenMain)
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TRAY_SYNC => {
            CONTEXT.with_borrow(|ctx| {
                if let Some(ctx) = ctx {
                    sync(hwnd, ctx);
                }
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: `hwnd` is this thread's live window.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            notify_icon(NIM_DELETE, &icon_data(hwnd, NOTIFY_ICON_DATA_FLAGS(0)));
            // SAFETY: no arguments besides the exit code.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            // Explorer restarted: the old icon is gone, add it again (same id,
            // so it never duplicates).
            let recreated = CONTEXT.with_borrow(|ctx| {
                ctx.as_ref()
                    .filter(|c| c.taskbar_created != 0 && msg == c.taskbar_created)
                    .map(|c| add_icon(hwnd, c))
            });
            if recreated.is_some() {
                return LRESULT(0);
            }
            // SAFETY: default handling for a message of this window.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// The app icon embedded as resource id 1 (see clipper-app's build script),
/// else the stock application icon.
fn load_icon() -> (HICON, bool) {
    // SAFETY: plain queries; the resource id is passed MAKEINTRESOURCE-style.
    unsafe {
        let module = GetModuleHandleW(None).ok();
        let size = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        if let Some(module) = module
            && let Ok(handle) = LoadImageW(
                Some(module.into()),
                PCWSTR(std::ptr::without_provenance(1)),
                IMAGE_ICON,
                size.0,
                size.1,
                LR_DEFAULTCOLOR,
            )
        {
            return (HICON(handle.0), true);
        }
        (LoadIconW(None, IDI_APPLICATION).unwrap_or_default(), false)
    }
}

fn create_window() -> Result<HWND, String> {
    let class = w!("ClipuhTrayOwner");
    // SAFETY: the class name is a static wide string; wndproc has the required
    // signature. Re-registering an existing class fails harmlessly.
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.message())?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // A hidden top-level window (not message-only): it must receive the
        // TaskbarCreated broadcast and be able to own a foreground popup menu.
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
            w!("clipuh tray"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| e.message())
    }
}

fn run(
    shared: Arc<Mutex<Shared>>,
    on_command: Box<dyn Fn(TrayCommand) + Send>,
    ready: mpsc::Sender<Result<isize, String>>,
) {
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(message) => {
            let _ = ready.send(Err(format!("could not create the tray window: {message}")));
            return;
        }
    };
    let (icon, owns_icon) = load_icon();
    // SAFETY: static NUL-terminated string.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    let ctx = Context {
        shared,
        on_command,
        icon,
        owns_icon,
        taskbar_created,
    };
    if !add_icon(hwnd, &ctx) {
        // Explorer may not be up yet; TaskbarCreated adds the icon later.
        eprintln!("tray: could not add the notification-area icon yet");
    }
    CONTEXT.set(Some(ctx));
    let _ = ready.send(Ok(hwnd.0 as isize));

    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out-pointer; None = any window of this thread.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        // SAFETY: `msg` came from GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if let Some(ctx) = CONTEXT.take()
        && ctx.owns_icon
    {
        // SAFETY: the icon was loaded by LoadImageW without LR_SHARED and the
        // shell no longer references it (icon deleted in WM_DESTROY).
        let _ = unsafe { DestroyIcon(ctx.icon) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(state: &TrayMenuState) -> Vec<(TrayCommand, &'static str, bool, bool)> {
        menu_entries(state)
            .into_iter()
            .filter_map(|e| match e {
                MenuEntry::Item {
                    command,
                    label,
                    enabled,
                    checked,
                    ..
                } => Some((command, label, enabled, checked)),
                MenuEntry::Separator => None,
            })
            .collect()
    }

    fn find(state: &TrayMenuState, c: TrayCommand) -> (&'static str, bool, bool) {
        labels(state)
            .into_iter()
            .find(|(cmd, ..)| *cmd == c)
            .map(|(_, l, e, ch)| (l, e, ch))
            .unwrap()
    }

    #[test]
    fn replay_entry_follows_state() {
        let mut s = TrayMenuState::default();
        assert_eq!(
            find(&s, TrayCommand::ToggleReplay),
            ("Instant Replay: Off", true, false)
        );
        s.replay = TrayReplay::On;
        assert_eq!(
            find(&s, TrayCommand::ToggleReplay),
            ("Instant Replay: On", true, true)
        );
        for r in [
            TrayReplay::Starting,
            TrayReplay::Stopping,
            TrayReplay::Unavailable,
        ] {
            s.replay = r;
            assert!(!find(&s, TrayCommand::ToggleReplay).1);
        }
    }

    #[test]
    fn save_and_recent_follow_availability() {
        let mut s = TrayMenuState::default();
        assert!(!find(&s, TrayCommand::SaveClip).1);
        assert!(!find(&s, TrayCommand::OpenRecentClip).1);
        s.can_save = true;
        s.has_recent_clip = true;
        assert!(find(&s, TrayCommand::SaveClip).1);
        assert!(find(&s, TrayCommand::OpenRecentClip).1);
    }

    #[test]
    fn every_command_round_trips_through_its_id() {
        for (c, ..) in labels(&TrayMenuState::default()) {
            assert_eq!(command_from_id(command_id(c)), Some(c));
        }
        assert_eq!(command_from_id(0), None);
        assert_eq!(command_from_id(99), None);
    }

    #[test]
    fn menu_has_exit_last_and_open_default() {
        let entries = menu_entries(&TrayMenuState::default());
        assert!(matches!(
            entries.last(),
            Some(MenuEntry::Item {
                command: TrayCommand::Exit,
                ..
            })
        ));
        assert!(matches!(
            entries.first(),
            Some(MenuEntry::Item {
                command: TrayCommand::OpenMain,
                default: true,
                ..
            })
        ));
    }

    #[test]
    fn wide_copy_truncates_and_terminates() {
        let mut buf = [0xFFFFu16; 4];
        copy_wide(&mut buf, "abcdef");
        assert_eq!(buf, [97, 98, 99, 0]);
        copy_wide(&mut buf, "");
        assert_eq!(buf[0], 0);
    }

    #[test]
    fn balloon_queue_is_bounded() {
        let mut q = VecDeque::new();
        for i in 0..10 {
            push_balloon(
                &mut q,
                Balloon {
                    kind: BalloonKind::Info,
                    title: i.to_string(),
                    body: String::new(),
                    sound: false,
                },
            );
        }
        assert_eq!(q.len(), MAX_PENDING_BALLOONS);
        assert_eq!(q.back().unwrap().title, "9");
    }

    #[test]
    fn tooltip_reflects_replay() {
        let mut s = TrayMenuState::default();
        assert!(tooltip(&s).ends_with("off"));
        s.replay = TrayReplay::On;
        assert!(tooltip(&s).ends_with("on"));
    }

    #[test]
    fn handle_tracks_dirty_state_without_thread() {
        let handle = TrayHandle {
            hwnd: 0,
            shared: Arc::new(Mutex::new(Shared::default())),
        };
        handle.set_state(TrayMenuState::default());
        assert!(!handle.shared.lock().unwrap().state_dirty);
        handle.set_state(TrayMenuState {
            can_save: true,
            ..Default::default()
        });
        assert!(handle.shared.lock().unwrap().state_dirty);
    }

    /// Creates and removes a real tray icon; needs an interactive Windows session.
    #[test]
    #[ignore = "touches the real notification area"]
    fn spawn_and_drop_real_tray() {
        let tray = Tray::spawn(|_| {}).expect("tray");
        tray.handle().set_state(TrayMenuState {
            replay: TrayReplay::On,
            can_save: true,
            has_recent_clip: false,
        });
        drop(tray);
    }
}
