//! Shell actions: open a file with its default app, reveal it in Explorer.
//!
//! Callers must be on a thread in a COM apartment (the GPUI UI thread is).

use std::io::{Error, ErrorKind, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IBindCtx,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    FILEOPERATION_FLAGS, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOFX_EARLYFAILURE,
    FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IFileOperationProgressSink, IShellItem,
    SHCreateItemFromParsingName, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW,
};
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

/// Moves a file to the Windows Recycle Bin.
///
/// This does not fall back to permanent deletion if the shell operation fails.
/// Initializes an STA for the shell call; an existing incompatible apartment is
/// returned as an error. Call from an STA worker if the caller is MTA or must
/// remain responsive.
pub fn recycle_file(path: &Path) -> Result<()> {
    if path.as_os_str().encode_wide().any(|unit| unit == 0) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "path contains an interior NUL",
        ));
    }
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{} is not a file", path.display()),
        ));
    }

    let absolute_path = std::path::absolute(path)?;
    let wide: Vec<u16> = absolute_path.as_os_str().encode_wide().chain([0]).collect();

    let _apartment = StaApartment::init()?;
    // SAFETY: COM is initialized in an STA for this call, and the shell item
    // stays alive until PerformOperations returns.
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>) }
            .map_err(Error::other)?;
    // SAFETY: CoCreateInstance is called from the initialized STA above.
    let operation: IFileOperation = unsafe {
        CoCreateInstance(
            &FileOperation,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
    }
    .map_err(Error::other)?;
    // SAFETY: all IFileOperation calls are made on the same STA that created it.
    unsafe { operation.SetOperationFlags(recycle_operation_flags()) }.map_err(Error::other)?;
    // SAFETY: `item` is a live shell item; this queues a single-item deletion.
    unsafe { operation.DeleteItem(&item, None::<&IFileOperationProgressSink>) }
        .map_err(Error::other)?;
    // SAFETY: the operation remains live and is called on its creating STA.
    unsafe { operation.PerformOperations() }.map_err(Error::other)?;
    // SAFETY: the operation remains live and is called on its creating STA.
    if unsafe { operation.GetAnyOperationsAborted() }
        .map_err(Error::other)?
        .as_bool()
    {
        return Err(Error::new(
            ErrorKind::Interrupted,
            format!("recycling {} was aborted", path.display()),
        ));
    }
    Ok(())
}

fn recycle_operation_flags() -> FILEOPERATION_FLAGS {
    FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT | FOFX_EARLYFAILURE | FOFX_RECYCLEONDELETE
}

struct StaApartment {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl StaApartment {
    fn init() -> Result<Self> {
        // SAFETY: no reserved pointer; Drop balances successful S_OK/S_FALSE
        // initialization on this same thread. A conflicting MTA is an error.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(Error::other)?;
        Ok(Self {
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for StaApartment {
    fn drop(&mut self) {
        // SAFETY: paired with successful STA initialization on this thread.
        unsafe { CoUninitialize() };
    }
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

    #[test]
    fn recycle_missing_path_is_not_found() {
        let p = Path::new("Z:/definitely/missing/clip.mp4");
        assert_eq!(recycle_file(p).unwrap_err().kind(), ErrorKind::NotFound);
    }

    #[test]
    fn recycle_rejects_directories() {
        let p = std::env::temp_dir();
        assert_eq!(
            recycle_file(&p).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn recycle_rejects_paths_with_interior_nuls() {
        let p = Path::new("C:\\temp\\bad\0path.txt");
        assert_eq!(recycle_file(p).unwrap_err().kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn recycle_requires_the_native_recycle_flag() {
        let flags = recycle_operation_flags();
        assert_ne!(
            flags.0 & windows::Win32::UI::Shell::FOFX_RECYCLEONDELETE.0,
            0
        );
    }

    #[test]
    #[ignore = "moves a disposable file into the Windows Recycle Bin"]
    fn recycle_disposable_mp4_out_of_its_source_path() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "clipper-platform-recycle-{}-{suffix}.mp4",
            std::process::id()
        ));
        std::fs::write(&path, b"temporary recycle test file").unwrap();

        let result = recycle_file(&path);
        let source_remains = path.exists();
        if source_remains {
            std::fs::remove_file(&path).unwrap();
        }

        result.unwrap();
        assert!(!source_remains, "recycled file remains at its source path");
    }
}
