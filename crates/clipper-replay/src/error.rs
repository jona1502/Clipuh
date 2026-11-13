//! Replay error type shared by every module.

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum ReplayError {
    /// No complete keyframe-aligned footage is buffered yet.
    NotReady,
    /// The target volume does not have enough free space.
    DiskFull { path: PathBuf },
    /// No permission to write `path`.
    AccessDenied { path: PathBuf },
    /// A temporary segment failed validation (checksum, truncation, format).
    Corrupt(String),
    /// Too many exports are already running.
    Busy,
    /// The exporter or buffer is shutting down.
    Shutdown,
    /// Input packets violate the contract (e.g. unknown track, bad layout).
    Invalid(String),
    Io {
        context: String,
        source: std::io::Error,
    },
}

impl ReplayError {
    /// Wraps an I/O error, mapping disk-full and permission errors.
    pub fn io(context: impl Into<String>, path: &Path, source: std::io::Error) -> Self {
        // ERROR_DISK_FULL (112) / ERROR_HANDLE_DISK_FULL (39) on Windows,
        // ENOSPC (28) elsewhere.
        let code = source.raw_os_error();
        let full = source.kind() == std::io::ErrorKind::StorageFull
            || (cfg!(windows) && matches!(code, Some(112 | 39)))
            || (cfg!(not(windows)) && code == Some(28));
        if full {
            return Self::DiskFull {
                path: path.to_path_buf(),
            };
        }
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            return Self::AccessDenied {
                path: path.to_path_buf(),
            };
        }
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReady => f.write_str("the replay buffer has no footage yet"),
            Self::DiskFull { path } => write!(f, "not enough disk space at {}", path.display()),
            Self::AccessDenied { path } => {
                write!(f, "no permission to write to {}", path.display())
            }
            Self::Corrupt(detail) => write!(f, "a temporary replay segment is damaged: {detail}"),
            Self::Busy => f.write_str("clips are already being saved, try again in a moment"),
            Self::Shutdown => f.write_str("the replay buffer is shutting down"),
            Self::Invalid(detail) => write!(f, "invalid replay data: {detail}"),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for ReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
