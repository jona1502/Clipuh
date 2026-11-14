//! Real disk usage for the sidebar's storage indicator.
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StorageStatus {
    Unknown,
    Unavailable(String),
    Ready(VolumeUsage),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VolumeUsage {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// Binary units labelled GB/TB, like Windows Explorer; one decimal below 10.
fn fmt_bytes(b: u64) -> String {
    const KB: f64 = 1024.0;
    let b = b as f64;
    let (v, unit) = if b >= KB.powi(4) {
        (b / KB.powi(4), "TB")
    } else if b >= KB.powi(3) {
        (b / KB.powi(3), "GB")
    } else if b >= KB.powi(2) {
        (b / KB.powi(2), "MB")
    } else if b >= KB {
        (b / KB, "KB")
    } else {
        return format!("{} B", b as u64);
    };
    // Rounding can carry 9.96 -> "10.0"; fall through to integer then.
    let one = format!("{v:.1}");
    if v < 10.0 && one != "10.0" {
        format!("{one} {unit}")
    } else {
        format!("{v:.0} {unit}")
    }
}

#[allow(dead_code)] // used by the sidebar once wired
impl VolumeUsage {
    pub fn used_fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        let used = self.total_bytes.saturating_sub(self.free_bytes);
        (used as f64 / self.total_bytes as f64) as f32
    }

    pub fn free_label(&self) -> String {
        format!("{} free", fmt_bytes(self.free_bytes.min(self.total_bytes)))
    }

    pub fn used_label(&self) -> String {
        let used = self.total_bytes.saturating_sub(self.free_bytes);
        format!(
            "{} of {} used",
            fmt_bytes(used),
            fmt_bytes(self.total_bytes)
        )
    }
}

/// Blocking: queries the volume of the recordings folder.
#[allow(dead_code)] // used by the sidebar once wired
pub(crate) fn load(recordings_dir: &Path) -> StorageStatus {
    match clipper_platform::windows::disk::volume_space(recordings_dir) {
        Ok(s) if s.total_bytes > 0 => StorageStatus::Ready(VolumeUsage {
            total_bytes: s.total_bytes,
            free_bytes: s.free_bytes,
        }),
        Ok(_) => StorageStatus::Unavailable("Disk size unknown".into()),
        Err(_) => StorageStatus::Unavailable("Disk unavailable".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    const TB: u64 = 1 << 40;

    #[test]
    fn formatting() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(512 * MB), "512 MB");
        assert_eq!(fmt_bytes(GB), "1.0 GB");
        assert_eq!(fmt_bytes(GB * 84 / 10), "8.4 GB");
        assert_eq!(fmt_bytes(10 * GB), "10 GB");
        assert_eq!(fmt_bytes(128 * GB), "128 GB");
        assert_eq!(fmt_bytes(TB * 12 / 10), "1.2 TB");
        assert_eq!(fmt_bytes(10 * GB - 1), "10 GB");
        assert_eq!(fmt_bytes(1023 * MB), "1023 MB");
    }

    #[test]
    fn labels() {
        let u = VolumeUsage {
            total_bytes: TB,
            free_bytes: TB - 412 * GB,
        };
        assert_eq!(u.used_label(), "412 GB of 1.0 TB used");
        assert_eq!(u.free_label(), "612 GB free");
    }

    #[test]
    fn fraction_edges() {
        let f = |t, fr| {
            VolumeUsage {
                total_bytes: t,
                free_bytes: fr,
            }
            .used_fraction()
        };
        assert_eq!(f(0, 0), 0.0);
        assert_eq!(f(100, 200), 0.0);
        assert_eq!(f(100, 25), 0.75);
        assert_eq!(f(100, 0), 1.0);
    }
}
