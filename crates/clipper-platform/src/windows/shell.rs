//! Shell actions: open a file with its default app, reveal it in Explorer.
//!
//! Callers must be on a thread in a COM apartment (the GPUI UI thread is).

use std::io::{Error, ErrorKind, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn not_found(path: &Path) -> Error {
    Error::new(
        ErrorKind::NotFound,
        format!("{} does not exist", path.display()),
    )
}

/// Opens `path` (file or folder) with its default handler.
pub fn open_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(not_found(path));
    }
    let wide = wide(path);
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call;
    // the other pointer arguments are null/None.
    let code = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    }
    .0 as isize;
    if code <= 32 {
        return Err(Error::other(format!(
            "could not open {} (shell error {code})",
            path.display()
        )));
    }
    Ok(())
}

/// Opens Explorer with `path` selected. If the file is gone but its folder
/// exists, opens the folder instead.
pub fn reveal_in_folder(path: &Path) -> Result<()> {
    if !path.exists() {
        return match path.parent() {
            Some(dir) if dir.is_dir() => open_path(dir),
            _ => Err(not_found(path)),
        };
    }
    let wide = wide(path);
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `pidl` is a valid
    // out-pointer. On success the returned PIDL is ours to free.
    unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None) }
        .map_err(Error::other)?;
    // SAFETY: `pidl` is a valid absolute PIDL from SHParseDisplayName; a null
    // folder PIDL with one item means "select this absolute item".
    let res = unsafe { SHOpenFolderAndSelectItems(pidl, None, 0) };
    // SAFETY: `pidl` was allocated by the shell with CoTaskMemAlloc and is not
    // used afterwards.
    unsafe { CoTaskMemFree(Some(pidl.cast_const().cast())) };
    res.map_err(Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_path_is_not_found() {
        let p = Path::new("Z:/definitely/missing/clip.mp4");
        assert_eq!(open_path(p).unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(reveal_in_folder(p).unwrap_err().kind(), ErrorKind::NotFound);
    }
}
