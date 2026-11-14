//! Volume free/total space.
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::core::PCWSTR;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeSpace {
    pub total_bytes: u64,
    /// Free bytes available to the calling user (respects quotas).
    pub free_bytes: u64,
}

/// Space on the volume holding `path`. Walks up to the nearest existing
/// ancestor (the recordings folder may not exist yet).
pub fn volume_space(path: &Path) -> io::Result<VolumeSpace> {
    let existing = path
        .ancestors()
        .find(|p| !p.as_os_str().is_empty() && p.exists())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no existing ancestor"))?;
    let wide: Vec<u16> = existing.as_os_str().encode_wide().chain([0]).collect();
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated and outlives the call; the out
    // pointers reference live locals; the total-free pointer is optional.
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut free),
            Some(&mut total),
            None,
        )
    }
    .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(VolumeSpace {
        total_bytes: total,
        free_bytes: free,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_and_missing_paths() {
        let dir = std::env::temp_dir();
        for p in [dir.clone(), dir.join("clipper-no-such-dir").join("x")] {
            let s = volume_space(&p).unwrap();
            assert!(s.total_bytes > 0 && s.free_bytes <= s.total_bytes);
        }
    }

    #[test]
    fn missing_drive_errors() {
        let letter = ('A'..='Z')
            .find(|c| !Path::new(&format!("{c}:\\")).exists())
            .expect("an unused drive letter");
        assert!(volume_space(Path::new(&format!("{letter}:/a/b"))).is_err());
    }
}
