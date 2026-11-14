//! Application discovery: audio sessions per render endpoint and window owners.

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2,
    eRender,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows::core::{Interface, PWSTR};

use super::devices::{create_enumerator, take_pwstr};
use super::stream::{ComGuard, map_os_error};
use crate::types::AudioError;

/// An application with an audio session on an active render endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioApplication {
    pub executable: String,
    /// Session display name, or the executable without `.exe`.
    pub name: String,
    pub playing: bool,
}

fn executable_of_pid(pid: u32) -> Option<String> {
    // SAFETY: plain OpenProcess; the handle is closed below on every path.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` is valid for `len` u16s; `len` is updated to the written length.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_ok();
    // SAFETY: `handle` was opened above and is closed exactly once.
    let _ = unsafe { CloseHandle(handle) };
    if !ok {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit(['\\', '/']).next().map(str::to_owned)
}

/// Executable file name of the process owning the top-level window `hwnd`
/// (HWND value as u64). `None` if the window or process is gone.
pub fn executable_of_window(hwnd: u64) -> Option<String> {
    if hwnd == 0 {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: GetWindowThreadProcessId tolerates invalid handles (returns 0);
    // `pid` is a valid out pointer.
    let thread = unsafe { GetWindowThreadProcessId(HWND(hwnd as usize as *mut _), Some(&mut pid)) };
    if thread == 0 || pid == 0 {
        return None;
    }
    executable_of_pid(pid)
}

/// Applications with an audio session on any active render endpoint, one per
/// executable, sorted by name. Blocking; call off the UI thread.
pub fn list_audio_applications() -> Result<Vec<AudioApplication>, AudioError> {
    let _com = ComGuard::init();
    let enumerator = create_enumerator()?;
    // SAFETY: valid enumerator.
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
        .map_err(|e| map_os_error("EnumAudioEndpoints", &e))?;
    // SAFETY: valid collection.
    let count = unsafe { devices.GetCount() }.map_err(|e| map_os_error("GetCount", &e))?;
    let own = std::process::id();
    let mut apps: Vec<AudioApplication> = Vec::new();
    for index in 0..count {
        // SAFETY: index < count.
        let Ok(device) = (unsafe { devices.Item(index) }) else {
            continue;
        };
        // SAFETY: valid device; activation yields an owned COM interface.
        let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) })
        else {
            continue;
        };
        // SAFETY: valid manager.
        let Ok(sessions) = (unsafe { manager.GetSessionEnumerator() }) else {
            continue;
        };
        // SAFETY: valid enumerator.
        let n = unsafe { sessions.GetCount() }.unwrap_or(0);
        for i in 0..n {
            // SAFETY: i < n.
            let Ok(control) = (unsafe { sessions.GetSession(i) }) else {
                continue;
            };
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            // SAFETY: valid session control interfaces; the display name is a
            // CoTaskMem string that `take_pwstr` frees.
            let (pid, state, display, system) = unsafe {
                (
                    control2.GetProcessId().unwrap_or(0),
                    control.GetState().ok(),
                    control.GetDisplayName().ok().map(take_pwstr),
                    control2.IsSystemSoundsSession().0 == 0,
                )
            };
            if pid == 0 || pid == own || system {
                continue;
            }
            let Some(executable) = executable_of_pid(pid) else {
                continue;
            };
            let playing = state == Some(AudioSessionStateActive);
            if let Some(existing) = apps
                .iter_mut()
                .find(|a| a.executable.eq_ignore_ascii_case(&executable))
            {
                existing.playing |= playing;
                continue;
            }
            let stem = executable
                .rsplit_once('.')
                .map_or(executable.as_str(), |(stem, _)| stem)
                .to_owned();
            let name = display
                .filter(|d| !d.is_empty() && !d.starts_with('@'))
                .unwrap_or(stem);
            apps.push(AudioApplication {
                executable,
                name,
                playing,
            });
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_succeeds() {
        // May be empty on machines without playing apps; must not error.
        assert!(list_audio_applications().is_ok());
    }

    #[test]
    fn zero_window_has_no_executable() {
        assert_eq!(executable_of_window(0), None);
    }
}
