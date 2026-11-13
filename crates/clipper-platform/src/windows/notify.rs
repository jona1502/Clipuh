//! Overlay helpers: fullscreen detection, non-activating show, notification sound.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, MB_OK, SPI_GETWORKAREA, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowLongPtrW,
    SetWindowPos, SystemParametersInfoW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

/// True when a Direct3D exclusive-fullscreen app is running; overlays cannot
/// appear above it. Returns false on API error.
pub fn exclusive_fullscreen_active() -> bool {
    // SAFETY: no pointer arguments; plain query of the shell state.
    unsafe { SHQueryUserNotificationState() }
        .is_ok_and(|state| state == QUNS_RUNNING_D3D_FULL_SCREEN)
}

/// Turns a hidden top-level window into a topmost, non-activating tool window,
/// sizes it to `width` x `height` logical px, docks it to the bottom-right of
/// the primary work area (above the taskbar, `margin` logical px from the
/// edges) and shows it without taking focus. `hwnd` is the raw HWND value.
///
/// The size is set here because gpui applies a hidden window's own bounds only
/// when it activates the window, which would steal focus.
pub fn show_without_activation(
    hwnd: isize,
    width: i32,
    height: i32,
    margin: i32,
) -> std::io::Result<()> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let extra = (WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW).0 as isize;
    let mut work = RECT::default();
    // SAFETY: the caller guarantees `hwnd` is a live window; RECT out-pointers
    // are valid locals. These calls only read/modify the window's style and
    // position and fail harmlessly on a bad handle.
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&raw mut work).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .map_err(std::io::Error::other)?;
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let [width, height, margin] = [width, height, margin].map(|v| v * dpi / 96);
        let x = work.right - width - margin;
        let y = work.bottom - height - margin;
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | extra);
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
        )
    }
    .map_err(std::io::Error::other)
}

/// Short system notification sound; MessageBeep returns immediately.
pub fn play_notification_sound() {
    // SAFETY: no pointer arguments.
    let _ = unsafe { MessageBeep(MB_OK) };
}
