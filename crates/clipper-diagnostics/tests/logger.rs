//! Single test: `init` is process-global.

use std::fs;
use std::io::Read;
use std::path::Path;

use clipper_diagnostics::log::{debug, info};
use clipper_diagnostics::{
    DiagnosticsError, LogConfig, SystemInfo, Verbosity, current_verbosity, export_zip, flush, init,
    log_dir, set_verbosity, shutdown,
};

fn read_all(dir: &Path) -> String {
    let mut out = String::new();
    for e in fs::read_dir(dir).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("clipuh") && n.ends_with(".log") {
            out.push_str(&fs::read_to_string(e.path()).unwrap_or_default());
        }
    }
    out
}

#[test]
fn logger_lifecycle() {
    assert!(matches!(
        set_verbosity(Verbosity::Debug),
        Err(DiagnosticsError::NotInitialized)
    ));
    assert!(current_verbosity().is_none() && log_dir().is_none());

    let dir = std::env::temp_dir().join(format!("clipuh-diag-logger-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);

    let mut cfg = LogConfig::new(dir.clone(), Verbosity::Info);
    cfg.max_file_bytes = 2 * 1024;
    cfg.max_files = 3;
    init(&cfg).unwrap();
    assert!(matches!(
        init(&cfg),
        Err(DiagnosticsError::AlreadyInitialized)
    ));
    assert_eq!(current_verbosity(), Some(Verbosity::Info));
    assert_eq!(log_dir().as_deref(), Some(dir.as_path()));
    fs::write(dir.join("notes.txt"), "keep me").unwrap();

    info!(target: "clipper_ui::views", "hello-info");
    debug!(target: "clipper_ui::views", "hidden-debug");
    info!(target: "gpui::window", "foreign-info");
    clipper_diagnostics::log::warn!(target: "gpui::window", "foreign-warn");
    flush();
    let text = read_all(&dir);
    assert!(
        text.contains("INFO  [clipper_ui::views] hello-info"),
        "{text}"
    );
    assert!(!text.contains("hidden-debug"));
    assert!(!text.contains("foreign-info"));
    assert!(text.contains("foreign-warn"));

    set_verbosity(Verbosity::Debug).unwrap();
    assert_eq!(current_verbosity(), Some(Verbosity::Debug));
    debug!(target: "clipper_ui::views", "shown-debug");
    flush();
    assert!(read_all(&dir).contains("shown-debug"));

    // Export while logger holds the current file open.
    let out = std::env::temp_dir().join(format!("clipuh-diag-logger-out-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();
    let dest = out.join("d.zip");
    export_zip(&dir, &dest, &SystemInfo::default(), None, None).unwrap();
    let mut z = zip::ZipArchive::new(fs::File::open(&dest).unwrap()).unwrap();
    let mut found = false;
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        if f.name().unwrap().starts_with("clipuh") {
            let mut s = String::new();
            f.read_to_string(&mut s).unwrap();
            found |= s.contains("shown-debug");
        }
    }
    assert!(found);
    let _ = fs::remove_dir_all(&out);

    // Rotation under concurrency.
    let threads: Vec<_> = (0..4)
        .map(|t| {
            std::thread::spawn(move || {
                for i in 0..400 {
                    info!(target: "clipper_core::rot", "thread {t} line {i} padding padding padding padding");
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    flush();
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let logs = names.iter().filter(|n| n.ends_with(".log")).count();
    assert!((2..=4).contains(&logs), "{names:?}");
    assert!(
        names.contains(&"notes.txt".to_string()),
        "notes.txt deleted: {names:?}"
    );

    shutdown();
    let _ = fs::remove_dir_all(&dir);
}
