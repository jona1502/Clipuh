//! Rotating file logging and privacy-sanitized diagnostics export for clipuh.

mod export;
mod logger;
mod panic;
mod sanitize;

pub use export::{ExportSummary, SystemInfo, export_zip};
pub use log;
pub use logger::{
    DiagnosticsError, LogConfig, Verbosity, current_verbosity, default_log_dir, flush, init,
    log_dir, set_verbosity, shutdown,
};
pub use panic::{PanicReport, install_panic_hook};
pub use sanitize::sanitize;

#[doc(hidden)]
pub use export::export_zip_with_cap;
