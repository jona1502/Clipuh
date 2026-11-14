//! Recent Clips presents the same indexed metadata and previews as the Library.
//! The previous scan + probe + thumbnail path is now owned by LibraryWorker.
use gpui::RenderImage;
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub(crate) enum RecentClips {
    #[default]
    Loading,
    Ready(Vec<RecentClip>),
    Unavailable(String),
}
pub(crate) struct RecentClip {
    pub clip: clipper_storage::library::Clip,
    pub thumbnail: Option<Arc<RenderImage>>,
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
}
