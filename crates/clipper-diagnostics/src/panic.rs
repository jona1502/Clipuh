//! Panic reporting: a GUI app has no console, so without this hook a crash
//! leaves no trace. The hook writes the panic to the log file, flushes it,
//! tells the app (which may show a dialog) and then runs the previous hook.

use std::backtrace::Backtrace;
use std::panic::PanicHookInfo;

/// What the hook knows about one panic.
#[derive(Clone, Debug)]
pub struct PanicReport {
    /// Thread name, or `<unnamed>`.
    pub thread: String,
    /// The panic payload as text.
    pub message: String,
    /// `file:line:column` of the panic, when known.
    pub location: Option<String>,
}

impl PanicReport {
    fn from_info(info: &PanicHookInfo<'_>) -> Self {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-text panic payload".to_owned());
        Self {
            thread: std::thread::current()
                .name()
                .unwrap_or("<unnamed>")
                .to_owned(),
            message,
            location: info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())),
        }
    }

    /// One-line summary for the log and dialogs.
    pub fn summary(&self) -> String {
        match &self.location {
            Some(location) => format!(
                "thread '{}' panicked at {location}: {}",
                self.thread, self.message
            ),
            None => format!("thread '{}' panicked: {}", self.thread, self.message),
        }
    }
}

/// Installs the panic hook (once per process; a later call replaces the
/// callback). For every panic it logs the summary and a backtrace at error
/// level, flushes the log, calls `on_panic`, then the previously installed
/// hook (the default one prints to stderr).
pub fn install_panic_hook(on_panic: impl Fn(&PanicReport) + Send + Sync + 'static) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let report = PanicReport::from_info(info);
        log::error!(
            "{}\nbacktrace:\n{}",
            report.summary(),
            Backtrace::force_capture()
        );
        crate::logger::try_flush();
        on_panic(&report);
        previous(info);
    }));
}
