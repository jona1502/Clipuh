//! Newest saved clips for the Capture page: a blocking scan (run it off the
//! UI thread) plus the pure formatting helpers the card uses.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clipper_media::{thumbnail::thumbnail, trim::probe};
use gpui::RenderImage;

const THUMBNAIL_WIDTH: u32 = 160;

#[derive(Default)]
pub(crate) enum RecentClips {
    #[default]
    Loading,
    Ready(Vec<RecentClip>),
    Unavailable(String),
}

pub(crate) struct RecentClip {
    pub path: PathBuf,
    pub name: String,
    pub duration: Option<Duration>,
    pub size_bytes: u64,
    pub modified: SystemTime,
    pub thumbnail: Option<Arc<RenderImage>>,
}

/// Newest `limit` `*.mp4` (never `*.partial.mp4`) in `dir`. Only those are
/// probed and thumbnailed. A missing directory is an empty list.
pub(crate) fn load(dir: &Path, limit: usize) -> RecentClips {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return RecentClips::Ready(vec![]),
        Err(e) => return RecentClips::Unavailable(format!("Clips folder unavailable: {e}")),
    };
    let mut found: Vec<(PathBuf, u64, SystemTime)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.to_ascii_lowercase();
            if !name.ends_with(".mp4") || name.ends_with(".partial.mp4") {
                return None;
            }
            let meta = entry.metadata().ok().filter(|m| m.is_file())?;
            Some((path, meta.len(), meta.modified().ok()?))
        })
        .collect();
    found.sort_by_key(|entry| std::cmp::Reverse(entry.2));
    found.truncate(limit);
    RecentClips::Ready(
        found
            .into_iter()
            .map(|(path, size_bytes, modified)| RecentClip {
                name: path
                    .file_stem()
                    .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
                duration: probe(&path).ok().map(|info| info.duration),
                thumbnail: thumbnail(&path, THUMBNAIL_WIDTH)
                    .ok()
                    .and_then(render_image),
                path,
                size_bytes,
                modified,
            })
            .collect(),
    )
}

fn render_image(t: clipper_media::thumbnail::Thumbnail) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(t.width, t.height, t.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// "just now", "2 min ago", "3 h ago", "yesterday", "4 days ago", else `YYYY-MM-DD` (UTC).
pub(crate) fn relative_time(now: SystemTime, then: SystemTime) -> String {
    let secs = now.duration_since(then).map_or(0, |d| d.as_secs());
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        86_400..172_800 => "yesterday".into(),
        172_800..604_800 => format!("{} days ago", secs / 86_400),
        _ => date(then),
    }
}

fn date(t: SystemTime) -> String {
    let days = t
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Decimal units: "226 MB", "1.2 GB".
pub(crate) fn size_label(bytes: u64) -> String {
    const KB: f64 = 1e3;
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / KB).max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn relative_time_buckets() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let ago = |s| relative_time(now, now - Duration::from_secs(s));
        assert_eq!(ago(10), "just now");
        assert_eq!(ago(120), "2 min ago");
        assert_eq!(ago(3 * 3600), "3 h ago");
        assert_eq!(ago(30 * 3600), "yesterday");
        assert_eq!(ago(4 * 86_400), "4 days ago");
        assert_eq!(ago(30 * 86_400), "2023-10-15");
        assert_eq!(relative_time(now, now + Duration::from_secs(5)), "just now");
    }

    #[test]
    fn size_labels() {
        assert_eq!(size_label(226_000_000), "226 MB");
        assert_eq!(size_label(1_234_000_000), "1.2 GB");
        assert_eq!(size_label(500), "1 KB");
    }

    #[test]
    fn load_orders_limits_and_skips_partials() {
        let dir = std::env::temp_dir().join(format!("clipper-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = SystemTime::now() - Duration::from_secs(10_000);
        for (name, age) in [
            ("a.mp4", 1),
            ("b.mp4", 4),
            ("c.mp4", 3),
            ("d.mp4", 2),
            ("new.partial.mp4", 9),
            ("n.txt", 8),
        ] {
            let f = File::create(dir.join(name)).unwrap();
            f.set_modified(base + Duration::from_secs(age * 100))
                .unwrap();
        }
        let RecentClips::Ready(clips) = load(&dir, 3) else {
            panic!("not ready")
        };
        let names: Vec<_> = clips.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["b", "c", "d"]);
        assert!(
            clips
                .iter()
                .all(|c| c.duration.is_none() && c.thumbnail.is_none())
        );
        assert!(matches!(load(&dir.join("missing"), 3), RecentClips::Ready(v) if v.is_empty()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
