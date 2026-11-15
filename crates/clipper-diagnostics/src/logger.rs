use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use flexi_logger::{
    Cleanup, Criterion, DeferredNow, FileSpec, LogSpecBuilder, Logger, LoggerHandle, Naming,
    WriteMode,
};
use log::{LevelFilter, Record};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Verbosity {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl Verbosity {
    pub const ALL: [Verbosity; 5] = [
        Verbosity::Error,
        Verbosity::Warn,
        Verbosity::Info,
        Verbosity::Debug,
        Verbosity::Trace,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Verbosity::Error => "Error",
            Verbosity::Warn => "Warn",
            Verbosity::Info => "Info",
            Verbosity::Debug => "Debug",
            Verbosity::Trace => "Trace",
        }
    }

    fn filter(self) -> LevelFilter {
        match self {
            Verbosity::Error => LevelFilter::Error,
            Verbosity::Warn => LevelFilter::Warn,
            Verbosity::Info => LevelFilter::Info,
            Verbosity::Debug => LevelFilter::Debug,
            Verbosity::Trace => LevelFilter::Trace,
        }
    }
}

#[derive(Debug)]
pub enum DiagnosticsError {
    AlreadyInitialized,
    NotInitialized,
    Io(std::io::Error),
    Logger(String),
    Zip(String),
    TooLarge,
}

impl fmt::Display for DiagnosticsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialized => write!(f, "logger already initialized"),
            Self::NotInitialized => write!(f, "logger not initialized"),
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Logger(e) => write!(f, "logger error: {e}"),
            Self::Zip(e) => write!(f, "zip error: {e}"),
            Self::TooLarge => write!(f, "diagnostics too large"),
        }
    }
}

impl std::error::Error for DiagnosticsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for DiagnosticsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Clone, Debug)]
pub struct LogConfig {
    pub directory: PathBuf,
    pub verbosity: Verbosity,
    pub max_file_bytes: u64,
    pub max_files: usize,
}

impl LogConfig {
    pub fn new(directory: PathBuf, verbosity: Verbosity) -> Self {
        Self {
            directory,
            verbosity,
            max_file_bytes: 5 * 1024 * 1024,
            max_files: 5,
        }
    }
}

/// `%LOCALAPPDATA%\clipuh\logs`, fallback `<temp>/clipuh/logs`.
pub fn default_log_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("clipuh").join("logs")
}

struct State {
    handle: LoggerHandle,
    dir: PathBuf,
    verbosity: Verbosity,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

fn build_spec(v: Verbosity) -> flexi_logger::LogSpecification {
    let mut b = LogSpecBuilder::new();
    b.default(LevelFilter::Warn);
    b.module("clipper", v.filter());
    b.build()
}

fn format_line(
    w: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> Result<(), std::io::Error> {
    write!(
        w,
        "{} {:<5} [{}] {}",
        now.format("%Y-%m-%d %H:%M:%S%.3f"),
        record.level(),
        record.target(),
        record.args()
    )
}

pub fn init(config: &LogConfig) -> Result<(), DiagnosticsError> {
    let mut state = lock();
    if state.is_some() {
        return Err(DiagnosticsError::AlreadyInitialized);
    }
    std::fs::create_dir_all(&config.directory)?;
    let handle = Logger::with(build_spec(config.verbosity))
        .log_to_file(
            FileSpec::default()
                .directory(&config.directory)
                .basename("clipuh")
                .suffix("log")
                .suppress_timestamp(),
        )
        .format(format_line)
        .append()
        .rotate(
            Criterion::Size(config.max_file_bytes.max(1)),
            Naming::Numbers,
            Cleanup::KeepLogFiles(config.max_files),
        )
        .cleanup_in_background_thread(false)
        .write_mode(WriteMode::BufferAndFlushWith(
            8 * 1024,
            Duration::from_secs(1),
        ))
        .start()
        .map_err(|e| DiagnosticsError::Logger(e.to_string()))?;
    *state = Some(State {
        handle,
        dir: config.directory.clone(),
        verbosity: config.verbosity,
    });
    Ok(())
}

pub fn set_verbosity(v: Verbosity) -> Result<(), DiagnosticsError> {
    let mut state = lock();
    let s = state.as_mut().ok_or(DiagnosticsError::NotInitialized)?;
    s.handle.set_new_spec(build_spec(v));
    s.verbosity = v;
    Ok(())
}

pub fn current_verbosity() -> Option<Verbosity> {
    lock().as_ref().map(|s| s.verbosity)
}

pub fn flush() {
    if let Some(s) = lock().as_ref() {
        s.handle.flush();
    }
}

/// [`flush`] that never blocks: skipped while another thread holds the state
/// (a panic hook must not deadlock on a lock its own thread may hold).
pub(crate) fn try_flush() {
    let state = match STATE.try_lock() {
        Ok(state) => state,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    if let Some(s) = state.as_ref() {
        s.handle.flush();
    }
}

pub fn log_dir() -> Option<PathBuf> {
    lock().as_ref().map(|s| s.dir.clone())
}

pub fn shutdown() {
    if let Some(s) = lock().as_ref() {
        s.handle.flush();
        s.handle.shutdown();
    }
}

/// True if `name` is a file produced by our logger
/// (`clipuh_rCURRENT.log`, `clipuh_r00001.log`).
pub(crate) fn is_log_file_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("clipuh_r") else {
        return false;
    };
    let Some(mid) = rest.strip_suffix(".log") else {
        return false;
    };
    mid == "CURRENT" || (!mid.is_empty() && mid.bytes().all(|b| b.is_ascii_digit()))
}
