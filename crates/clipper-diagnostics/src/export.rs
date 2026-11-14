use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::logger::{DiagnosticsError, is_log_file_name};
use crate::sanitize::sanitize;

const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct SystemInfo {
    pub app_version: String,
    pub windows_version: Option<String>,
    pub gpu: Option<String>,
    pub capture_backend: Option<String>,
    pub encoder: Option<String>,
    pub capture_settings: Vec<(String, String)>,
    pub recent_errors: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportSummary {
    pub files: usize,
    pub bytes: u64,
}

pub fn export_zip(
    log_dir: &Path,
    destination: &Path,
    info: &SystemInfo,
    home: Option<&Path>,
    user_name: Option<&str>,
) -> Result<ExportSummary, DiagnosticsError> {
    export_zip_with_cap(log_dir, destination, info, home, user_name, MAX_TOTAL_BYTES)
}

fn invalid(msg: &str) -> DiagnosticsError {
    DiagnosticsError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
}

fn zip_err(e: zip::result::ZipError) -> DiagnosticsError {
    DiagnosticsError::Zip(e.to_string())
}

fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn inside(dir: &Path, dest: &Path) -> bool {
    let d = canon(dir);
    let parent = dest.parent().map(canon).unwrap_or_default();
    let full = match dest.file_name() {
        Some(n) => parent.join(n),
        None => parent,
    };
    full.starts_with(&d)
}

#[doc(hidden)]
pub fn export_zip_with_cap(
    log_dir: &Path,
    destination: &Path,
    info: &SystemInfo,
    home: Option<&Path>,
    user_name: Option<&str>,
    cap: u64,
) -> Result<ExportSummary, DiagnosticsError> {
    if inside(log_dir, destination) {
        return Err(invalid("destination is inside the log directory"));
    }
    let partial = destination.with_extension("zip.partial");
    let result = write_zip(log_dir, &partial, info, home, user_name, cap).and_then(|s| {
        fs::rename(&partial, destination)?;
        Ok(s)
    });
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

struct LogFile {
    name: String,
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

fn collect_logs(dir: &Path) -> Vec<LogFile> {
    let mut v = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return v;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !is_log_file_name(&name) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        if !md.is_file() {
            continue;
        }
        v.push(LogFile {
            name,
            path: e.path(),
            len: md.len(),
            modified: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
    v.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| b.name.cmp(&a.name))
    });
    v
}

fn write_zip(
    log_dir: &Path,
    out: &Path,
    info: &SystemInfo,
    home: Option<&Path>,
    user_name: Option<&str>,
    cap: u64,
) -> Result<ExportSummary, DiagnosticsError> {
    let file = File::create(out)?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut files = 0usize;
    let mut total = 0u64;
    let mut skipped: Vec<String> = Vec::new();

    for lf in collect_logs(log_dir) {
        if total.saturating_add(lf.len) > cap {
            skipped.push(lf.name);
            continue;
        }
        let Ok(f) = File::open(&lf.path) else {
            skipped.push(format!("{} (unreadable)", lf.name));
            continue;
        };
        zip.start_file(&lf.name, opts).map_err(zip_err)?;
        let mut rd = BufReader::new(f);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = rd.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            let line = sanitize(&String::from_utf8_lossy(&buf), home, user_name);
            total += line.len() as u64;
            zip.write_all(line.as_bytes())?;
        }
        files += 1;
    }

    let text = sanitize(&system_info_text(info, &skipped, cap), home, user_name);
    zip.start_file("system-info.txt", opts).map_err(zip_err)?;
    zip.write_all(text.as_bytes())?;
    total += text.len() as u64;
    files += 1;

    let mut f = zip.finish().map_err(zip_err)?;
    f.flush()?;
    Ok(ExportSummary {
        files,
        bytes: total,
    })
}

fn system_info_text(info: &SystemInfo, skipped: &[String], cap: u64) -> String {
    let opt = |k: &str, v: &Option<String>| format!("{k}: {}\n", v.as_deref().unwrap_or("unknown"));
    let mut s = format!("App version: {}\n", info.app_version);
    s.push_str(&opt("Windows version", &info.windows_version));
    s.push_str(&opt("GPU", &info.gpu));
    s.push_str(&opt("Capture backend", &info.capture_backend));
    s.push_str(&opt("Encoder", &info.encoder));
    s.push_str("\nCapture settings:\n");
    if info.capture_settings.is_empty() {
        s.push_str("  (none)\n");
    }
    for (k, v) in &info.capture_settings {
        s.push_str(&format!("  {k}: {v}\n"));
    }
    s.push_str("\nRecent errors:\n");
    if info.recent_errors.is_empty() {
        s.push_str("  (none)\n");
    }
    for e in &info.recent_errors {
        s.push_str(&format!("  {e}\n"));
    }
    if !skipped.is_empty() {
        s.push_str(&format!(
            "\nOmitted log files (size cap {cap} bytes, or unreadable):\n"
        ));
        for n in skipped {
            s.push_str(&format!("  {n}\n"));
        }
    }
    s
}
