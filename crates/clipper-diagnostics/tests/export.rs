use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use clipper_diagnostics::{SystemInfo, export_zip, export_zip_with_cap, sanitize};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("clipuh-diag-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn read_zip(p: &Path) -> Vec<(String, String)> {
    let mut z = zip::ZipArchive::new(fs::File::open(p).unwrap()).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut s = String::new();
            f.read_to_string(&mut s).unwrap();
            (f.name().unwrap().to_string(), s)
        })
        .collect()
}

fn info() -> SystemInfo {
    SystemInfo {
        app_version: "1.2.3".into(),
        gpu: Some("Test GPU".into()),
        ..Default::default()
    }
}

#[test]
fn sanitize_home_both_separators_and_case() {
    let home = Path::new(r"C:\Users\Bobby");
    let s = sanitize(
        r"a C:\Users\Bobby\clips b c:/users/bobby/x d C:\\Users\\Bobby\\y",
        Some(home),
        None,
    );
    assert_eq!(
        s,
        r"a %USERPROFILE%\clips b %USERPROFILE%/x d %USERPROFILE%\\y"
    );
    // longer component not swallowed
    assert_eq!(
        sanitize(r"C:\Users\Bobby2\x", Some(home), None),
        r"C:\Users\Bobby2\x"
    );
}

#[test]
fn sanitize_user_name_word_boundary() {
    let s = sanitize("hi Alice, alice! xalicex Alices ALICE", None, Some("Alice"));
    assert_eq!(s, "hi <user>, <user>! xalicex Alices <user>");
}

#[test]
fn sanitize_short_or_empty_user_ignored() {
    assert_eq!(sanitize("al bo al", None, Some("al")), "al bo al");
    assert_eq!(sanitize("abc", None, Some("")), "abc");
    assert_eq!(sanitize("abc", None, Some("  ")), "abc");
    assert_eq!(sanitize("abc", Some(Path::new("")), None), "abc");
    assert_eq!(sanitize("abc", None, None), "abc");
}

fn make_log_dir(tag: &str) -> PathBuf {
    let d = temp_dir(tag);
    fs::write(
        d.join("clipuh_rCURRENT.log"),
        "line C:\\Users\\Bobby\\a user=bobby\n",
    )
    .unwrap();
    fs::write(d.join("clipuh_r00001.log"), "old line\n").unwrap();
    fs::write(d.join("notes.txt"), "secret notes\n").unwrap();
    fs::write(d.join("clipuh_r00002.log.partial"), "partial\n").unwrap();
    fs::write(d.join("video.mp4"), "xx").unwrap();
    d
}

#[test]
fn export_contains_sanitized_logs_only() {
    let logs = make_log_dir("exp");
    let out_dir = temp_dir("exp-out");
    let dest = out_dir.join("diag.zip");
    let sum = export_zip(
        &logs,
        &dest,
        &info(),
        Some(Path::new(r"C:\Users\Bobby")),
        Some("bobby"),
    )
    .unwrap();
    let entries = read_zip(&dest);
    let names: Vec<_> = entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(sum.files, 3);
    assert!(names.contains(&"system-info.txt"));
    assert!(names.contains(&"clipuh_rCURRENT.log"));
    assert!(names.contains(&"clipuh_r00001.log"));
    assert_eq!(names.len(), 3, "{names:?}");
    let cur = &entries
        .iter()
        .find(|e| e.0 == "clipuh_rCURRENT.log")
        .unwrap()
        .1;
    assert_eq!(cur, "line %USERPROFILE%\\a user=<user>\n");
    let si = &entries.iter().find(|e| e.0 == "system-info.txt").unwrap().1;
    assert!(si.contains("App version: 1.2.3") && si.contains("GPU: Test GPU"));
    assert!(!out_dir.join("diag.zip.partial").exists());
    let _ = fs::remove_dir_all(&logs);
    let _ = fs::remove_dir_all(&out_dir);
}

#[test]
fn export_refuses_destination_inside_log_dir() {
    let logs = make_log_dir("inside");
    let dest = logs.join("diag.zip");
    let err = export_zip(&logs, &dest, &info(), None, None).unwrap_err();
    match err {
        clipper_diagnostics::DiagnosticsError::Io(e) => {
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput)
        }
        e => panic!("unexpected {e:?}"),
    }
    assert!(!dest.exists());
    let _ = fs::remove_dir_all(&logs);
}

#[test]
fn export_failure_leaves_no_partial() {
    let logs = make_log_dir("fail");
    let base = temp_dir("fail-out");
    let dest = base.join("missing").join("diag.zip");
    assert!(export_zip(&logs, &dest, &info(), None, None).is_err());
    assert!(!base.join("missing").exists());
    // rename failure after partial written: destination is an existing directory
    let dest2 = base.join("dirdest.zip");
    fs::create_dir_all(&dest2).unwrap();
    assert!(export_zip(&logs, &dest2, &info(), None, None).is_err());
    assert!(!base.join("dirdest.zip.partial").exists());
    let _ = fs::remove_dir_all(&logs);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn export_size_cap_skips_oldest() {
    let d = temp_dir("cap");
    let old = d.join("clipuh_r00001.log");
    fs::write(&old, "o".repeat(600)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(d.join("clipuh_rCURRENT.log"), "n".repeat(600)).unwrap();
    let out = temp_dir("cap-out");
    let dest = out.join("d.zip");
    export_zip_with_cap(&d, &dest, &info(), None, None, 1000).unwrap();
    let entries = read_zip(&dest);
    let names: Vec<_> = entries.iter().map(|e| e.0.as_str()).collect();
    assert!(names.contains(&"clipuh_rCURRENT.log"), "{names:?}");
    assert!(!names.contains(&"clipuh_r00001.log"), "{names:?}");
    let si = &entries.iter().find(|e| e.0 == "system-info.txt").unwrap().1;
    assert!(si.contains("clipuh_r00001.log"));
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&out);
}

#[test]
fn export_missing_log_dir_still_has_system_info() {
    let base = temp_dir("nolog");
    let dest = base.join("d.zip");
    let sum = export_zip(&base.join("nope"), &dest, &info(), None, None).unwrap();
    assert_eq!(sum.files, 1);
    let entries = read_zip(&dest);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "system-info.txt");
    let _ = fs::remove_dir_all(&base);
}
