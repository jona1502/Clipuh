//! In-app notification queue: bounded, de-duplicated, expiring by deadline.
//!
//! Pure state with no timers of its own. The owner schedules a single wake-up
//! for [`NotificationCenter::next_deadline`] and calls `expire`, so nothing
//! repaints or polls while toasts sit idle.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clipper_storage::NotificationSettings;

/// Toasts visible at once; older ones are dropped first (errors last).
pub(crate) const MAX_TOASTS: usize = 3;
/// Errors stay at least this long so they can be read.
const MIN_ERROR_DURATION: Duration = Duration::from_secs(10);
/// "Saving…" never outlives a lost result by more than this.
const PROGRESS_DURATION: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

/// What a notice is about; controls preferences and where it may appear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Category {
    /// Replay clip save progress and results (also shown by the overlay toast).
    ClipSave,
    Replay,
    Recording,
    Capture,
    Audio,
    Storage,
    /// Feedback for a quick action the user just triggered.
    Action,
}

/// Optional button on a toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToastAction {
    OpenClip(PathBuf),
    OpenSettings,
}

impl ToastAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::OpenClip(_) => "Open Clip",
            Self::OpenSettings => "Settings",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    /// Notices with the same key replace each other (one per topic).
    pub key: String,
    pub category: Category,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub action: Option<ToastAction>,
    /// Shows a progress state that a later notice with the same key resolves.
    pub progress: bool,
    /// Important enough for a native desktop notification.
    pub desktop: bool,
    /// Never plays the notification sound (informational hints).
    pub silent: bool,
}

impl Notice {
    pub fn new(
        key: impl Into<String>,
        category: Category,
        severity: Severity,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            category,
            severity,
            title: title.into(),
            body: body.into(),
            action: None,
            progress: false,
            desktop: false,
            silent: false,
        }
    }

    pub fn action(mut self, action: ToastAction) -> Self {
        self.action = Some(action);
        self
    }

    pub fn desktop(mut self) -> Self {
        self.desktop = true;
        self
    }

    pub fn progress(mut self) -> Self {
        self.progress = true;
        self
    }

    pub fn silent(mut self) -> Self {
        self.silent = true;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Toast {
    pub id: u64,
    pub notice: Notice,
    /// How many identical notices were grouped into this toast.
    pub count: u32,
    pub expires_at: Instant,
}

/// How long `notice` stays on screen with these preferences.
pub(crate) fn display_duration(notice: &Notice, settings: &NotificationSettings) -> Duration {
    if notice.progress {
        return PROGRESS_DURATION;
    }
    let base = Duration::from_secs(u64::from(settings.duration_seconds.max(1)));
    match notice.severity {
        Severity::Info | Severity::Success => base,
        Severity::Warning => base * 2,
        Severity::Error => (base * 3).max(MIN_ERROR_DURATION),
    }
}

/// Whether the user's preferences allow `notice` at all.
pub(crate) fn allowed(notice: &Notice, settings: &NotificationSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    !(notice.category == Category::ClipSave
        && notice.severity == Severity::Error
        && !settings.notify_on_failure)
}

#[derive(Debug, Default)]
pub(crate) struct NotificationCenter {
    toasts: VecDeque<Toast>,
    next_id: u64,
}

impl NotificationCenter {
    /// Adds `notice`, grouping it with an identical visible one or replacing
    /// one with the same key. Returns the toast id.
    pub fn push(&mut self, notice: Notice, now: Instant, duration: Duration) -> u64 {
        let expires_at = now + duration;
        if let Some(index) = self.toasts.iter().position(|t| t.notice.key == notice.key) {
            let mut toast = self.toasts.remove(index).expect("index from position");
            let same = toast.notice.title == notice.title && toast.notice.body == notice.body;
            toast.count = if same {
                toast.count.saturating_add(1)
            } else {
                1
            };
            toast.notice = notice;
            toast.expires_at = expires_at;
            let id = toast.id;
            self.toasts.push_front(toast);
            return id;
        }
        if self.toasts.len() >= MAX_TOASTS {
            self.evict_one();
        }
        self.next_id += 1;
        let id = self.next_id;
        self.toasts.push_front(Toast {
            id,
            notice,
            count: 1,
            expires_at,
        });
        id
    }

    /// Drops the oldest non-error toast, else the oldest toast.
    fn evict_one(&mut self) {
        let index = self
            .toasts
            .iter()
            .rposition(|t| t.notice.severity != Severity::Error)
            .unwrap_or(self.toasts.len() - 1);
        self.toasts.remove(index);
    }

    pub fn dismiss(&mut self, id: u64) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.id != id);
        self.toasts.len() != before
    }

    /// Removes expired toasts; true when any were removed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.expires_at > now);
        self.toasts.len() != before
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.toasts.iter().map(|t| t.expires_at).min()
    }

    /// Newest first.
    pub fn toasts(&self) -> impl Iterator<Item = &Toast> {
        self.toasts.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(key: &str, severity: Severity) -> Notice {
        Notice::new(key, Category::Replay, severity, key, "")
    }
    const S: Duration = Duration::from_secs(4);

    #[test]
    fn queue_is_bounded_and_newest_first() {
        let now = Instant::now();
        let mut c = NotificationCenter::default();
        for i in 0..10 {
            c.push(n(&i.to_string(), Severity::Info), now, S);
        }
        assert_eq!(c.toasts().count(), MAX_TOASTS);
        let keys: Vec<_> = c.toasts().map(|t| t.notice.key.clone()).collect();
        assert_eq!(keys, ["9", "8", "7"]);
    }

    #[test]
    fn eviction_keeps_errors() {
        let now = Instant::now();
        let mut c = NotificationCenter::default();
        c.push(n("err", Severity::Error), now, S);
        for i in 0..5 {
            c.push(n(&i.to_string(), Severity::Success), now, S);
        }
        assert!(c.toasts().any(|t| t.notice.key == "err"));
        assert_eq!(c.toasts().count(), MAX_TOASTS);
    }

    #[test]
    fn identical_notices_group() {
        let now = Instant::now();
        let mut c = NotificationCenter::default();
        let a = c.push(n("x", Severity::Error), now, S);
        let b = c.push(n("x", Severity::Error), now + S, S);
        assert_eq!(a, b);
        let t = c.toasts().next().unwrap();
        assert_eq!(t.count, 2);
        assert_eq!(t.expires_at, now + S + S);
        assert_eq!(c.toasts().count(), 1);
    }

    #[test]
    fn same_key_new_content_replaces() {
        let now = Instant::now();
        let mut c = NotificationCenter::default();
        c.push(
            Notice::new("clip", Category::ClipSave, Severity::Info, "Saving", "").progress(),
            now,
            S,
        );
        c.push(
            Notice::new("clip", Category::ClipSave, Severity::Success, "Saved", ""),
            now,
            S,
        );
        let t = c.toasts().next().unwrap();
        assert_eq!((t.notice.title.as_str(), t.count), ("Saved", 1));
        assert_eq!(c.toasts().count(), 1);
    }

    #[test]
    fn dismiss_and_expire_lifecycle() {
        let now = Instant::now();
        let mut c = NotificationCenter::default();
        let a = c.push(n("a", Severity::Info), now, Duration::from_secs(1));
        c.push(n("b", Severity::Info), now, Duration::from_secs(5));
        assert_eq!(c.next_deadline(), Some(now + Duration::from_secs(1)));
        assert!(c.dismiss(a));
        assert!(!c.dismiss(a));
        assert!(!c.expire(now + Duration::from_secs(4)));
        assert!(c.expire(now + Duration::from_secs(5)));
        assert!(c.is_empty());
        assert_eq!(c.next_deadline(), None);
    }

    #[test]
    fn durations_by_severity() {
        let s = NotificationSettings::default();
        let base = Duration::from_secs(u64::from(s.duration_seconds));
        assert_eq!(display_duration(&n("a", Severity::Success), &s), base);
        assert_eq!(display_duration(&n("a", Severity::Warning), &s), base * 2);
        assert!(display_duration(&n("a", Severity::Error), &s) >= MIN_ERROR_DURATION);
        assert_eq!(
            display_duration(&n("a", Severity::Info).progress(), &s),
            PROGRESS_DURATION
        );
        assert!(Severity::Error > Severity::Warning && Severity::Warning > Severity::Success);
    }

    #[test]
    fn preferences_filter() {
        let mut s = NotificationSettings::default();
        let fail = Notice::new("c", Category::ClipSave, Severity::Error, "x", "");
        assert!(allowed(&fail, &s));
        s.notify_on_failure = false;
        assert!(!allowed(&fail, &s));
        assert!(allowed(&n("r", Severity::Error), &s));
        s.enabled = false;
        assert!(!allowed(&n("r", Severity::Info), &s));
    }
}
