pub use crate::config::LibrarySort;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
#[cfg(windows)]
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    GetFileInformationByHandle, MoveFileW,
};
#[cfg(windows)]
use windows::core::PCWSTR;

/// One completed MP4 in the configured library directory.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// Stable file identity. Windows IDs use the volume serial and file index;
    /// `path:` IDs indicate a metadata based fallback.
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub source: Option<String>,
    pub duration: Option<Duration>,
    pub size_bytes: u64,
    pub created: Option<SystemTime>,
    pub modified: SystemTime,
}

impl Clip {
    /// Whether the ID comes from the operating system's stable file identity.
    pub fn has_stable_identity(&self) -> bool {
        self.id.starts_with("win:")
    }
}

/// Finds direct, completed MP4 children and reuses decoded metadata for files
/// whose identity and size/mtime fingerprint have not changed.
pub fn scan(dir: &Path, previous: &[Clip]) -> io::Result<Vec<Clip>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };

    let previous_by_id: HashMap<&str, &Clip> = previous
        .iter()
        .map(|clip| (clip.id.as_str(), clip))
        .collect();
    let mut clips = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();
        if path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".partial.mp4")
        }) {
            continue;
        }
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !extension.eq_ignore_ascii_case("mp4") {
            continue;
        }

        // Read metadata without following links. This also handles a path that
        // changed to a symlink between file_type() and metadata collection.
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let modified = metadata.modified()?;
        let Some(id) = file_id(&path, &metadata) else {
            continue;
        };
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let cached = previous_by_id
            .get(id.as_str())
            .copied()
            .filter(|clip| clip.size_bytes == metadata.len() && clip.modified == modified);

        clips.push(Clip {
            id,
            path,
            name,
            source: cached.and_then(|clip| clip.source.clone()),
            duration: cached.and_then(|clip| clip.duration),
            size_bytes: metadata.len(),
            created: metadata.created().ok(),
            modified,
        });
    }

    clips.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.path.cmp(&b.path)));
    clips.dedup_by(|a, b| a.id == b.id);
    Ok(clips)
}

/// Returns clip indices matching the query, sorted by the selected field.
/// Case-insensitive search covers the clip name and known source label.
pub fn query(
    clips: &[Clip],
    search: &str,
    source: Option<&str>,
    favorites_only: bool,
    favorites: &BTreeSet<String>,
    sort: LibrarySort,
) -> Vec<usize> {
    let needle = search.trim().to_lowercase();
    let mut indices: Vec<_> = clips
        .iter()
        .enumerate()
        .filter(|(_, clip)| {
            if favorites_only && !favorites.contains(&clip.id) {
                return false;
            }
            if let Some(source) = source
                && !clip
                    .source
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(source))
            {
                return false;
            }
            let filename = clip
                .path
                .file_name()
                .map(|value| value.to_string_lossy().to_lowercase());
            if !needle.is_empty()
                && !clip.name.to_lowercase().contains(&needle)
                && !filename
                    .as_deref()
                    .is_some_and(|value| value.contains(&needle))
                && !clip
                    .source
                    .as_deref()
                    .is_some_and(|value| value.to_lowercase().contains(&needle))
            {
                return false;
            }
            true
        })
        .map(|(index, _)| index)
        .collect();

    indices.sort_by(|left, right| {
        let a = &clips[*left];
        let b = &clips[*right];
        let field_order = match sort {
            LibrarySort::Newest => sort_date(b).cmp(&sort_date(a)),
            LibrarySort::Oldest => sort_date(a).cmp(&sort_date(b)),
            LibrarySort::NameAsc => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            LibrarySort::NameDesc => b.name.to_lowercase().cmp(&a.name.to_lowercase()),
            LibrarySort::Duration => match (a.duration, b.duration) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            },
            LibrarySort::Size => a.size_bytes.cmp(&b.size_bytes),
        };
        field_order.then_with(|| a.id.cmp(&b.id))
    });
    indices
}

fn sort_date(clip: &Clip) -> SystemTime {
    clip.created.unwrap_or(clip.modified)
}

/// Stable-ID selection state shared by grid and list views.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub ids: BTreeSet<String>,
    pub anchor: Option<String>,
}

impl Selection {
    /// Applies a single, toggle, or inclusive range selection.
    pub fn select(&mut self, id: &str, ordered: &[String], ctrl: bool, shift: bool) {
        let Some(index) = ordered.iter().position(|candidate| candidate == id) else {
            return;
        };

        if shift
            && let Some(anchor) = self
                .anchor
                .as_deref()
                .and_then(|anchor| ordered.iter().position(|candidate| candidate == anchor))
        {
            let (start, end) = if anchor <= index {
                (anchor, index)
            } else {
                (index, anchor)
            };
            if !ctrl {
                self.ids.clear();
            }
            self.ids.extend(ordered[start..=end].iter().cloned());
            return;
        }

        if ctrl {
            if !self.ids.insert(id.to_owned()) {
                self.ids.remove(id);
            }
        } else {
            self.ids.clear();
            self.ids.insert(id.to_owned());
        }
        self.anchor = Some(id.to_owned());
    }

    /// Drops selected IDs and the anchor for clips that no longer exist.
    pub fn retain(&mut self, clips: &[Clip]) {
        let available: HashSet<&str> = clips.iter().map(|clip| clip.id.as_str()).collect();
        self.ids.retain(|id| available.contains(id.as_str()));
        if self
            .anchor
            .as_deref()
            .is_some_and(|anchor| !available.contains(anchor))
        {
            self.anchor = None;
        }
    }

    pub fn clear(&mut self) {
        self.ids.clear();
        self.anchor = None;
    }

    pub fn all(&mut self, ordered: &[String]) {
        self.ids = ordered.iter().cloned().collect();
        self.anchor = ordered.first().cloned();
    }
}

/// Renames a clip in place after checking that the selected path still refers
/// to the same file. The extension and parent directory are preserved.
pub fn rename(clip: &Clip, new_stem: &str) -> io::Result<PathBuf> {
    validate_stem(new_stem)?;
    let extension = clip
        .path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|extension| extension.eq_ignore_ascii_case("mp4"))
        .ok_or_else(|| invalid_input("the selected path is not an MP4"))?;
    let parent = clip
        .path
        .parent()
        .ok_or_else(|| invalid_input("the selected clip has no parent directory"))?;

    verify(clip)?;

    let old_stem = clip
        .path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if old_stem == new_stem {
        return Ok(clip.path.clone());
    }

    let destination = parent.join(format!("{new_stem}.{extension}"));
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a clip with that name already exists",
            ));
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        Err(_) => {}
    }
    rename_no_replace(&clip.path, &destination)?;
    Ok(destination)
}

/// Checks that the clip path still refers to the indexed file and fingerprint.
/// Use this before acting on a selected clip after a background scan or delay.
pub fn verify(clip: &Clip) -> io::Result<()> {
    let metadata = fs::symlink_metadata(&clip.path)?;
    if !metadata.file_type().is_file() {
        return Err(not_found("the selected clip is no longer a regular file"));
    }
    let Some(current_id) = file_id(&clip.path, &metadata) else {
        return Err(not_found("the selected clip is no longer a regular file"));
    };
    if current_id != clip.id
        || metadata.len() != clip.size_bytes
        || metadata.modified()? != clip.modified
    {
        return Err(not_found("the selected clip was replaced or changed"));
    }
    Ok(())
}

fn validate_stem(stem: &str) -> io::Result<()> {
    if stem.trim().is_empty() {
        return Err(invalid_input("a clip name cannot be empty"));
    }
    if matches!(stem.chars().last(), Some('.' | ' '))
        || stem
            .chars()
            .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
    {
        return Err(invalid_input(
            "the clip name contains invalid Windows characters",
        ));
    }

    let device_name = stem.split('.').next().unwrap_or(stem).to_ascii_uppercase();
    let reserved = matches!(device_name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            device_name.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9')
            })
        });
    if reserved {
        return Err(invalid_input(
            "the clip name is a reserved Windows device name",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let from: Vec<u16> = from.as_os_str().encode_wide().chain([0]).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain([0]).collect();
    // MoveFileW fails atomically if the destination already exists.
    unsafe { MoveFileW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr())) }.map_err(io::Error::other)
}

#[cfg(not(windows))]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    // `rename` replaces an existing file on Unix. Creating the hard link first
    // gives the destination an atomic no-clobber check in the same directory.
    fs::hard_link(from, to)?;
    if let Err(error) = fs::remove_file(from) {
        let _ = fs::remove_file(to);
        return Err(error);
    }
    Ok(())
}

#[cfg(windows)]
fn file_id(path: &Path, metadata: &fs::Metadata) -> Option<String> {
    match native_file_id(path) {
        Ok(Some(id)) => Some(id),
        Ok(None) => None,
        Err(_) => Some(fallback_file_id(path, metadata)),
    }
}

#[cfg(not(windows))]
fn file_id(path: &Path, metadata: &fs::Metadata) -> Option<String> {
    Some(fallback_file_id(path, metadata))
}

#[cfg(windows)]
fn native_file_id(path: &Path) -> io::Result<Option<String>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    let handle = HANDLE(file.as_raw_handle());
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut information) }.map_err(io::Error::other)?;

    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Ok(None);
    }
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok(Some(format!(
        "win:{:08x}:{index:016x}",
        information.dwVolumeSerialNumber
    )))
}

fn fallback_file_id(path: &Path, metadata: &fs::Metadata) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let canonical = canonical
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    let created = metadata
        .created()
        .ok()
        .map(time_token)
        .unwrap_or_else(|| "unknown".to_owned());
    let modified = metadata
        .modified()
        .ok()
        .map(time_token)
        .unwrap_or_else(|| "unknown".to_owned());
    format!("path:{canonical}:{}:{created}:{modified}", metadata.len())
}

fn time_token(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos().to_string(),
        Err(_) => "pre-epoch".to_owned(),
    }
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn not_found(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, message)
}
