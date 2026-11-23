//! Audio session discovery for the composed desktop remainder
//! ([`crate::desktop`]): which processes currently own an audio session on
//! any active render endpoint.

use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, eRender,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows_core::Interface;

use super::devices::create_enumerator;
use super::stream::map_os_error;
use crate::types::AudioError;

/// Process ids owning an audio session on an active render endpoint, in
/// enumeration order (may repeat). The system sounds session is reported as
/// pid `0`. Needs COM on the calling thread.
///
/// Sessions are listed whether or not they are playing right now: an app
/// that opened its audio stream is captured before its first sound. Sessions
/// of processes that already exited may still be listed; callers drop pids
/// missing from the process table.
pub(crate) fn session_pids() -> Result<Vec<u32>, AudioError> {
    let enumerator = create_enumerator()?;
    // SAFETY: valid enumerator; COM is initialized on this thread.
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
        .map_err(|e| map_os_error("EnumAudioEndpoints", &e))?;
    // SAFETY: valid collection.
    let count = unsafe { devices.GetCount() }.map_err(|e| map_os_error("GetCount", &e))?;
    let mut pids = Vec::new();
    for index in 0..count {
        // SAFETY: index < count.
        let Ok(device) = (unsafe { devices.Item(index) }) else {
            continue;
        };
        // A device that disappears meanwhile is skipped, not an error.
        // SAFETY: valid device; Activate returns an owned interface.
        let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) })
        else {
            continue;
        };
        // SAFETY: valid session manager.
        let Ok(sessions) = (unsafe { manager.GetSessionEnumerator() }) else {
            continue;
        };
        // SAFETY: valid session enumerator.
        let Ok(total) = (unsafe { sessions.GetCount() }) else {
            continue;
        };
        for session in 0..total {
            // SAFETY: session < total.
            let Ok(control) = (unsafe { sessions.GetSession(session) }) else {
                continue;
            };
            let Ok(control) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            // SAFETY: valid session control. S_OK means the system sounds
            // session, S_FALSE any other.
            if unsafe { control.IsSystemSoundsSession() }.0 == 0 {
                pids.push(0);
                continue;
            }
            // A multi-process session reports AUDCLNT_S_NO_SINGLE_PROCESS (a
            // success code) with one of its pids: that process is used.
            // SAFETY: valid session control.
            if let Ok(pid) = unsafe { control.GetProcessId() } {
                pids.push(pid);
            }
        }
    }
    Ok(pids)
}
