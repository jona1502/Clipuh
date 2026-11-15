//! Publishing a finished clip: structural check of the written MP4 and a
//! commit under the final name that never replaces an existing file.
//!
//! Shared by replay exports and manual recordings. Guarantees, as far as the
//! platform provides them (see `docs/quality/clip-save-reliability.md`):
//!
//! - **No overwrite.** Windows uses `MoveFileW`, which fails when the target
//!   exists, on every local file system including FAT/exFAT. Elsewhere a hard
//!   link creates the final name atomically and fails if it exists; only
//!   volumes without hard links fall back to a checked rename.
//! - **Atomic visibility** on one volume: the final name appears complete or
//!   not at all. The partial file always lives next to the final one.
//! - **Not power-loss proof.** File contents are flushed before the commit
//!   (`sync_all`, i.e. `FlushFileBuffers`), but the directory entry of the
//!   rename itself is not forced to disk.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

/// Waits before retrying a commit that failed with a transient error (a scanner
/// or indexer holding the file open). Bounded: at most 375 ms in total.
const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
];

/// Errors worth a short retry: another process briefly holds the file
/// (antivirus, search indexer, cloud sync). Windows reports these as
/// ERROR_ACCESS_DENIED (5), ERROR_SHARING_VIOLATION (32) or
/// ERROR_LOCK_VIOLATION (33).
pub fn is_transient(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
        || (cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33)))
}

/// Moves `partial` to `final_path` without ever replacing an existing file.
/// `AlreadyExists` means the final name is taken (nothing was changed); other
/// errors leave `partial` in place.
pub fn commit_no_replace(partial: &Path, final_path: &Path) -> io::Result<()> {
    retry_transient(|| commit_once(partial, final_path), std::thread::sleep)
}

/// Runs `op`, retrying [`is_transient`] failures after the bounded delays.
pub(crate) fn retry_transient(
    mut op: impl FnMut() -> io::Result<()>,
    mut sleep: impl FnMut(Duration),
) -> io::Result<()> {
    let mut delays = RETRY_DELAYS.iter();
    loop {
        match op() {
            Err(error) if is_transient(&error) => match delays.next() {
                Some(delay) => sleep(*delay),
                None => return Err(error),
            },
            result => return result,
        }
    }
}

#[cfg(windows)]
fn commit_once(partial: &Path, final_path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let from: Vec<u16> = partial.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = final_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both are NUL terminated UTF-16 strings that outlive the call.
    // MoveFileW (no MOVEFILE_REPLACE_EXISTING) fails if `to` exists.
    #[allow(unsafe_code)]
    let result = unsafe {
        windows::Win32::Storage::FileSystem::MoveFileW(
            windows::core::PCWSTR(from.as_ptr()),
            windows::core::PCWSTR(to.as_ptr()),
        )
    };
    result.map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xFFFF))
}

#[cfg(not(windows))]
fn commit_once(partial: &Path, final_path: &Path) -> io::Result<()> {
    match std::fs::hard_link(partial, final_path) {
        Ok(()) => {
            // The clip is published; a leftover partial is only clutter.
            let _ = std::fs::remove_file(partial);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(link_error) => {
            // No hard links on this volume: checked rename (tiny race window;
            // this path is not used by the Windows build).
            if std::fs::symlink_metadata(final_path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "the clip name is already taken",
                ));
            }
            std::fs::rename(partial, final_path).map_err(|_| link_error)
        }
    }
}

/// Why a written file is not a complete MP4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incomplete(pub String);

/// Cheap structural check of a finished MP4 (top-level box headers only,
/// a few small reads): `ftyp` first, an `mdat` and a `moov`, every box inside
/// the file and the boxes covering it exactly. An encoder or muxer that did
/// not finalize (no `moov`) or a truncated write fails it. Sample data is not
/// decoded.
pub fn check_mp4(path: &Path) -> io::Result<Result<(), Incomplete>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    Ok(check_boxes(&mut file, len))
}

fn check_boxes(file: &mut (impl Read + Seek), len: u64) -> Result<(), Incomplete> {
    let fail = |detail: String| Err(Incomplete(detail));
    let (mut at, mut first, mut moov, mut mdat) = (0u64, true, false, false);
    while at < len {
        if len - at < 8 {
            return fail(format!("{} stray bytes at the end", len - at));
        }
        let mut header = [0u8; 16];
        file.seek(SeekFrom::Start(at))
            .and_then(|_| file.read_exact(&mut header[..8]))
            .map_err(|error| Incomplete(format!("box header at {at} unreadable: {error}")))?;
        let kind = [header[4], header[5], header[6], header[7]];
        let size = match u32::from_be_bytes([header[0], header[1], header[2], header[3]]) {
            0 => len - at,
            1 => {
                file.read_exact(&mut header[8..16])
                    .map_err(|error| Incomplete(format!("large box size at {at}: {error}")))?;
                u64::from_be_bytes(header[8..16].try_into().unwrap_or_default())
            }
            size => u64::from(size),
        };
        let name = String::from_utf8_lossy(&kind).into_owned();
        if size < 8 || size > len - at {
            return fail(format!(
                "box '{name}' at {at} claims {size} bytes, {} remain",
                len - at
            ));
        }
        if first && &kind != b"ftyp" {
            return fail(format!("first box is '{name}', not 'ftyp'"));
        }
        first = false;
        moov |= &kind == b"moov";
        mdat |= &kind == b"mdat";
        at += size;
    }
    match (moov, mdat) {
        (true, true) => Ok(()),
        (false, _) => fail("no 'moov' box: the file was not finalized".into()),
        (_, false) => fail("no 'mdat' box".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn mp4_box(kind: &[u8; 4], body: usize) -> Vec<u8> {
        let mut out = ((body + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.resize(body + 8, 0);
        out
    }

    fn check(bytes: &[u8]) -> Result<(), Incomplete> {
        check_boxes(&mut Cursor::new(bytes), bytes.len() as u64)
    }

    fn complete() -> Vec<u8> {
        [
            mp4_box(b"ftyp", 16),
            mp4_box(b"mdat", 4000),
            mp4_box(b"moov", 300),
        ]
        .concat()
    }

    #[test]
    fn finalized_layout_passes() {
        assert_eq!(check(&complete()), Ok(()));
        // moov first (faststart) is fine too.
        let faststart = [
            mp4_box(b"ftyp", 16),
            mp4_box(b"moov", 300),
            mp4_box(b"mdat", 10),
        ]
        .concat();
        assert_eq!(check(&faststart), Ok(()));
    }

    #[test]
    fn unfinalized_or_truncated_files_fail() {
        let no_moov = [mp4_box(b"ftyp", 16), mp4_box(b"mdat", 4000)].concat();
        assert!(check(&no_moov).unwrap_err().0.contains("not finalized"));
        let file = complete();
        for cut in [0, 7, 30, 4000, file.len() - 1] {
            assert!(
                check(&file[..cut]).is_err() || cut == 0,
                "cut at {cut} passed"
            );
        }
        assert!(check(&[]).is_err(), "an empty file is not a clip");
        let mut trailing = complete();
        trailing.extend_from_slice(&[0, 0, 0]);
        assert!(check(&trailing).is_err());
        let wrong_start = [mp4_box(b"mdat", 4), mp4_box(b"moov", 4)].concat();
        assert!(check(&wrong_start).unwrap_err().0.contains("ftyp"));
        let mut zero_size = complete();
        zero_size[0..4].copy_from_slice(&4u32.to_be_bytes());
        assert!(check(&zero_size).is_err());
    }

    #[test]
    fn transient_errors_are_retried_a_bounded_number_of_times() {
        let mut calls = 0;
        let mut slept = Vec::new();
        let result = retry_transient(
            || {
                calls += 1;
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            },
            |delay| slept.push(delay),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 5);
        assert_eq!(slept.iter().sum::<Duration>(), Duration::from_millis(375));

        let mut calls = 0;
        let result = retry_transient(
            || {
                calls += 1;
                if calls < 3 {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                } else {
                    Ok(())
                }
            },
            |_| {},
        );
        assert!(result.is_ok());
        assert_eq!(calls, 3);

        // A taken name or a missing folder is final: no retry.
        for kind in [io::ErrorKind::AlreadyExists, io::ErrorKind::NotFound] {
            let mut calls = 0;
            let _ = retry_transient(
                || {
                    calls += 1;
                    Err(io::Error::from(kind))
                },
                |_| panic!("must not sleep"),
            );
            assert_eq!(calls, 1);
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clipuh publish {name} é {}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn commit_publishes_and_never_replaces() {
        let dir = temp_dir("commit");
        let partial = dir.join("Clipper_Replay ü.mp4.partial");
        let final_path = dir.join("Clipper_Replay ü.mp4");
        std::fs::write(&partial, b"new clip").unwrap();
        commit_no_replace(&partial, &final_path).unwrap();
        assert_eq!(std::fs::read(&final_path).unwrap(), b"new clip");
        assert!(!partial.exists());

        // The final name is taken: nothing changes, the partial stays.
        std::fs::write(&partial, b"second").unwrap();
        let error = commit_no_replace(&partial, &final_path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&final_path).unwrap(), b"new clip");
        assert_eq!(std::fs::read(&partial).unwrap(), b"second");

        // A missing partial is an error, not a silent success.
        let missing = dir.join("missing.mp4.partial");
        assert!(commit_no_replace(&missing, &dir.join("other.mp4")).is_err());
        assert!(!dir.join("other.mp4").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
