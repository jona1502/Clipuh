//! Modal message boxes for errors that happen without a usable main window
//! (startup failures, crashes). A GUI-subsystem process has no console, so
//! these are the only way such errors reach the user.

use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TASKMODAL, MessageBoxW,
};
use windows::core::HSTRING;

/// Shows a blocking error box with `title` and `text` and returns when the
/// user closes it. Without an interactive desktop it returns immediately.
///
/// The box runs on a fresh thread: on a thread whose message loop is ending
/// or unwinding (the UI thread in a panic) `MessageBoxW` returns at once
/// without showing anything.
pub fn show_error(title: &str, text: &str) {
    let title = HSTRING::from(title);
    let text = HSTRING::from(text);
    let shown = std::thread::Builder::new()
        .name("clipuh-error-box".into())
        .spawn(move || {
            // SAFETY: both strings are valid NUL-terminated UTF-16 for the
            // call; no owner window.
            unsafe {
                MessageBoxW(
                    None,
                    &text,
                    &title,
                    MB_OK | MB_ICONERROR | MB_TASKMODAL | MB_SETFOREGROUND,
                );
            }
        });
    if let Ok(shown) = shown {
        let _ = shown.join();
    }
}
