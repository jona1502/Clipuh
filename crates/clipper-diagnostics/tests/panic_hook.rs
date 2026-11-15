//! The panic hook is process-wide, so it gets its own test binary.

use std::sync::{Arc, Mutex};

use clipper_diagnostics::{LogConfig, PanicReport, Verbosity};

#[test]
fn panic_is_logged_flushed_and_reported() {
    let dir = std::env::temp_dir().join(format!("clipuh-diag-panic-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    clipper_diagnostics::init(&LogConfig::new(dir.clone(), Verbosity::Info)).unwrap();

    let reports: Arc<Mutex<Vec<PanicReport>>> = Arc::default();
    let seen = Arc::clone(&reports);
    clipper_diagnostics::install_panic_hook(move |report| {
        seen.lock().unwrap().push(report.clone());
    });

    let result = std::thread::Builder::new()
        .name("worker-under-test".into())
        .spawn(|| panic!("boom {}", 42))
        .unwrap()
        .join();
    assert!(result.is_err());

    let reports = reports.lock().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].thread, "worker-under-test");
    assert_eq!(reports[0].message, "boom 42");
    let location = reports[0].location.as_deref().unwrap();
    assert!(location.contains("panic_hook.rs"), "{location}");

    // The hook flushed already: the line is on disk before shutdown.
    let log: String = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
        .collect();
    assert!(
        log.contains("thread 'worker-under-test' panicked at")
            && log.contains("boom 42")
            && log.contains("backtrace:"),
        "{log}"
    );
    clipper_diagnostics::shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
