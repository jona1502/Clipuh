//! Launch on Windows sign-in through the per-user `Run` registry key.
//!
//! HKCU needs no elevation. The registry is the only source of truth: callers
//! read it back instead of trusting a saved flag.

use std::ffi::c_void;
use std::io::{Error, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE_NAME: PCWSTR = w!("clipuh");

fn check(status: WIN32_ERROR) -> Result<()> {
    status
        .ok()
        .map_err(|error| Error::from_raw_os_error(error.code().0 & 0xFFFF))
}

/// The command Windows runs at sign-in: the quoted executable path.
pub fn launch_command(executable: &Path) -> String {
    format!("\"{}\"", executable.display())
}

/// Whether the `Run` entry exists and starts `executable` (a stale entry for a
/// moved executable counts as off).
pub fn is_enabled(executable: &Path) -> Result<bool> {
    let mut buffer = [0u16; 2048];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: `buffer` is writable for `size` bytes and both strings are static
    // NUL-terminated literals; RRF_RT_REG_SZ guarantees a terminated result.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    check(status)?;
    let chars = (size as usize / 2).saturating_sub(1);
    let stored = String::from_utf16_lossy(&buffer[..chars]);
    Ok(stored.eq_ignore_ascii_case(&launch_command(executable)))
}

/// Adds or removes the `Run` entry for `executable`.
pub fn set_enabled(executable: &Path, enabled: bool) -> Result<()> {
    if !enabled {
        // SAFETY: static NUL-terminated key and value names.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME) };
        return if status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            check(status)
        };
    }
    let command: Vec<u16> = std::ffi::OsStr::new(&launch_command(executable))
        .encode_wide()
        .chain([0])
        .collect();
    // SAFETY: `command` is a NUL-terminated UTF-16 string that outlives the call;
    // its byte length (terminator included) is passed as the data size.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE_NAME,
            REG_SZ.0,
            Some(command.as_ptr().cast::<c_void>()),
            (command.len() * 2) as u32,
        )
    };
    check(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_command_quotes_paths_with_spaces() {
        assert_eq!(
            launch_command(Path::new(r"C:\Program Files\clipuh\clipuh.exe")),
            r#""C:\Program Files\clipuh\clipuh.exe""#
        );
    }
}
