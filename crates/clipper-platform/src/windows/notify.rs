//! Overlay helpers: fullscreen detection, non-activating show, notification sound.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Media::Audio::{PlaySoundW, SND_FILENAME, SND_NODEFAULT, SND_SYNC};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, MB_OK,
    SPI_GETWORKAREA, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowLongPtrW, SetWindowPos, SystemParametersInfoW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};
use windows::core::PCWSTR;

/// True when a Direct3D exclusive-fullscreen app is running; overlays cannot
/// appear above it. Returns false on API error.
pub fn exclusive_fullscreen_active() -> bool {
    // SAFETY: no pointer arguments; plain query of the shell state.
    unsafe { SHQueryUserNotificationState() }
        .is_ok_and(|state| state == QUNS_RUNNING_D3D_FULL_SCREEN)
}

/// Preserve native balloon quiet-time behavior when application playback
/// replaces Windows balloon audio. An explicit Settings preview may ignore it.
pub fn notification_quiet_time_active() -> bool {
    // SAFETY: no pointer arguments; plain query of the shell state.
    unsafe { SHQueryUserNotificationState() }.is_ok_and(|state| state == QUNS_QUIET_TIME)
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

/// True when the mouse pointer is inside the window `hwnd` (raw HWND value).
pub fn cursor_over_window(hwnd: isize) -> bool {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let (mut cursor, mut rect) = (POINT::default(), RECT::default());
    // SAFETY: out-pointers reference live locals; a stale handle just fails.
    let known =
        unsafe { GetCursorPos(&mut cursor).is_ok() && GetWindowRect(hwnd, &mut rect).is_ok() };
    known
        && (rect.left..rect.right).contains(&cursor.x)
        && (rect.top..rect.bottom).contains(&cursor.y)
}

/// Short system notification sound; MessageBeep returns immediately.
pub fn play_notification_sound() {
    // SAFETY: no pointer arguments.
    let _ = unsafe { MessageBeep(MB_OK) };
}

/// A single idle worker owns custom sound file loading and synchronous native
/// playback. Sending never waits for a file, audio device or earlier sound.
/// The one-slot mailbox and 500 ms suppression prevent notification bursts
/// from building a playback backlog or starting overlapping application sounds.
#[derive(Clone)]
pub struct NotificationSoundPlayer {
    sender: Option<SyncSender<Option<PathBuf>>>,
}

impl Default for NotificationSoundPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationSoundPlayer {
    pub fn new() -> Self {
        let (sender, receiver) = sync_channel::<Option<PathBuf>>(1);
        let worker = std::thread::Builder::new()
            .name("notification-sound".into())
            .spawn(move || {
                let mut previous = None::<Instant>;
                while let Ok(custom) = receiver.recv() {
                    if previous.is_some_and(|at| at.elapsed() < Duration::from_millis(500)) {
                        continue;
                    }
                    previous = Some(Instant::now());
                    play_custom_or_default(custom.as_deref());
                    // At most one pending request; discard sounds made obsolete
                    // while the current sound played instead of replaying a burst.
                    let _ = receiver.try_recv();
                }
            });
        Self {
            sender: worker.ok().map(|_| sender),
        }
    }

    pub fn play(&self, custom: Option<PathBuf>) {
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(custom);
        } else {
            // Thread creation failed: preserve the existing asynchronous
            // system notification sound without touching the custom file.
            play_notification_sound();
        }
    }
}

fn play_custom_or_default(custom: Option<&Path>) -> bool {
    if let Some(path) = custom.filter(|path| {
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= 5 * 1024 * 1024)
    }) {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if !wide[..wide.len() - 1].contains(&0) {
            // SAFETY: filename is NUL-terminated and remains alive throughout
            // synchronous playback. SND_FILENAME disables registry aliases;
            // SND_NODEFAULT prevents a second Windows fallback sound.
            if unsafe {
                PlaySoundW(
                    PCWSTR(wide.as_ptr()),
                    None,
                    SND_FILENAME | SND_NODEFAULT | SND_SYNC,
                )
            }
            .as_bool()
            {
                return true;
            }
        }
    }
    play_notification_sound();
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_never_blocks_on_a_full_mailbox() {
        let (sender, receiver) = sync_channel(1);
        let player = NotificationSoundPlayer {
            sender: Some(sender),
        };
        player.play(Some(PathBuf::from("first.wav")));
        player.play(Some(PathBuf::from("second.wav")));
        assert_eq!(
            receiver.try_recv().unwrap(),
            Some(PathBuf::from("first.wav"))
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    #[ignore = "plays a short generated WAV and the system notification sound on real Windows audio hardware"]
    fn hardware_notification_sound_custom_default_and_missing_fallback() {
        let path = std::env::temp_dir().join(format!(
            "clipper-notification-playback-{}.wav",
            std::process::id()
        ));
        let mut wave = b"RIFF".to_vec();
        wave.extend_from_slice(&1636u32.to_le_bytes());
        wave.extend_from_slice(b"WAVEfmt ");
        wave.extend_from_slice(&16u32.to_le_bytes());
        wave.extend_from_slice(&1u16.to_le_bytes());
        wave.extend_from_slice(&1u16.to_le_bytes());
        wave.extend_from_slice(&8000u32.to_le_bytes());
        wave.extend_from_slice(&16000u32.to_le_bytes());
        wave.extend_from_slice(&2u16.to_le_bytes());
        wave.extend_from_slice(&16u16.to_le_bytes());
        wave.extend_from_slice(b"data");
        wave.extend_from_slice(&1600u32.to_le_bytes());
        for frame in 0..800 {
            let sample =
                (1000.0 * (std::f32::consts::TAU * 440.0 * frame as f32 / 8000.0).sin()) as i16;
            wave.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(&path, wave).unwrap();
        // Direct worker-side helper calls verify native API calls return;
        // hearing the sound or judging its quality still requires a listener.
        assert!(play_custom_or_default(Some(&path)));
        assert!(!play_custom_or_default(None));
        std::fs::remove_file(&path).unwrap();
        assert!(!play_custom_or_default(Some(&path)));
        let player = NotificationSoundPlayer::new();
        for _ in 0..100 {
            player.play(None);
        }
    }
}
