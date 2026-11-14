use clipper_storage::config::{AppConfig, LibrarySettings, LibraryView};
use clipper_storage::library::{Clip, LibrarySort, Selection, query, rename, scan, verify};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        loop {
            let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "clipper-storage-library-{}-{id}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test directory: {error}"),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn test_clip(id: &str, name: &str, created: u64, modified: u64, size_bytes: u64) -> Clip {
    Clip {
        id: id.to_owned(),
        path: PathBuf::from(format!("{name}.mp4")),
        name: name.to_owned(),
        source: None,
        duration: None,
        size_bytes,
        created: Some(UNIX_EPOCH + Duration::from_secs(created)),
        modified: UNIX_EPOCH + Duration::from_secs(modified),
    }
}

#[test]
fn old_config_defaults_library_settings_and_new_values_round_trip() {
    let old: AppConfig = toml::from_str("[video]\nwidth = 1920\n").expect("old config loads");
    assert_eq!(old.library, LibrarySettings::default());
    assert_eq!(old.library.view, LibraryView::Grid);
    assert_eq!(old.library.sort, LibrarySort::Newest);

    let mut config = old;
    config.library.view = LibraryView::List;
    config.library.sort = LibrarySort::NameDesc;
    config.library.favorites = BTreeSet::from(["win:01:ab".to_owned()]);
    let serialized = toml::to_string(&config).expect("config serializes");
    let restored: AppConfig = toml::from_str(&serialized).expect("new config loads");
    assert_eq!(restored.library, config.library);
}

#[test]
fn scan_finds_only_direct_finished_mp4_files_and_missing_directory_is_empty() {
    let dir = TestDir::new();
    let nested = dir.path().join("nested");
    fs::create_dir(&nested).expect("create nested directory");
    fs::write(dir.path().join("clip.mp4"), b"video").expect("write clip");
    fs::write(dir.path().join("uppercase.MP4"), b"video").expect("write uppercase extension");
    fs::write(dir.path().join("clip.mp4.partial"), b"unfinished").expect("write partial");
    fs::write(dir.path().join("notes.txt"), b"text").expect("write non-video");
    fs::write(nested.join("nested.mp4"), b"nested video").expect("write nested clip");

    let clips = scan(dir.path(), &[]).expect("scan directory");
    let names: BTreeSet<_> = clips.iter().map(|clip| clip.name.as_str()).collect();
    assert_eq!(names, BTreeSet::from(["clip", "uppercase"]));
    assert!(
        scan(&dir.path().join("missing"), &[])
            .expect("missing directory is an empty library")
            .is_empty()
    );
}

#[test]
fn scan_skips_symlinks_when_the_platform_allows_creating_them() {
    let dir = TestDir::new();
    let real = dir.path().join("real.mp4");
    let link = dir.path().join("linked.mp4");
    fs::write(&real, b"video").expect("write clip");

    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&real, &link).is_ok();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&real, &link).is_ok();
    #[cfg(not(any(unix, windows)))]
    let linked = false;

    let clips = scan(dir.path(), &[]).expect("scan directory");
    let _ = linked;
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].name, "real");
}

#[test]
fn scan_retains_cached_metadata_only_when_file_fingerprint_is_unchanged() {
    let dir = TestDir::new();
    let path = dir.path().join("clip.mp4");
    fs::write(&path, b"video").expect("write clip");

    let mut previous = scan(dir.path(), &[]).expect("initial scan");
    previous[0].source = Some("Game.exe".to_owned());
    previous[0].duration = Some(Duration::from_secs(42));
    let same = scan(dir.path(), &previous).expect("incremental scan");
    assert_eq!(same[0].id, previous[0].id);
    assert_eq!(same[0].source.as_deref(), Some("Game.exe"));
    assert_eq!(same[0].duration, Some(Duration::from_secs(42)));

    fs::write(&path, b"a different sized video").expect("change clip fingerprint");
    let changed = scan(dir.path(), &same).expect("scan changed file");
    assert_eq!(changed[0].id, same[0].id);
    assert_eq!(changed[0].source, None);
    assert_eq!(changed[0].duration, None);
}

#[test]
fn query_filters_case_insensitive_search_source_and_favorites() {
    let clips = [
        test_clip("id-1", "Second Match", 1, 1, 10),
        test_clip("id-2", "Alpha", 2, 2, 20),
        test_clip("id-3", "First MATCH", 3, 3, 30),
    ]
    .map(|mut clip| {
        clip.source = Some("Game.exe".to_owned());
        clip
    });
    let favorites = BTreeSet::from(["id-1".to_owned(), "id-3".to_owned()]);

    assert_eq!(
        query(
            &clips,
            "match",
            Some("Game.exe"),
            true,
            &favorites,
            LibrarySort::NameAsc,
        ),
        [2, 0]
    );
    assert!(
        query(
            &clips,
            "",
            Some("Other.exe"),
            false,
            &favorites,
            LibrarySort::Newest,
        )
        .is_empty()
    );
}

#[test]
fn query_search_includes_the_actual_filename_and_extension() {
    let mut clip = test_clip("id-1", "friendly label", 1, 1, 10);
    clip.path = PathBuf::from("Clipper_Replay_2026-11-14.mp4");
    let clips = [clip];
    let favorites = BTreeSet::new();

    assert_eq!(
        query(&clips, ".MP4", None, false, &favorites, LibrarySort::Newest),
        [0]
    );
    assert_eq!(
        query(
            &clips,
            "replay_2026",
            None,
            false,
            &favorites,
            LibrarySort::Newest,
        ),
        [0]
    );
}

#[test]
fn query_sorts_by_selected_field_with_stable_id_ties_and_created_fallback() {
    let mut clips = [
        test_clip("z", "same", 10, 50, 100),
        test_clip("a", "same", 20, 20, 300),
        test_clip("m", "Alpha", 20, 40, 200),
        test_clip("b", "No Created Date", 0, 60, 50),
    ];
    clips[3].created = None;
    clips[0].duration = Some(Duration::from_secs(5));
    clips[1].duration = Some(Duration::from_secs(2));
    clips[2].duration = Some(Duration::from_secs(2));
    clips[3].duration = None;
    let favorites = BTreeSet::new();

    assert_eq!(
        query(&clips, "", None, false, &favorites, LibrarySort::Newest),
        [3, 1, 2, 0]
    );
    assert_eq!(
        query(&clips, "", None, false, &favorites, LibrarySort::Oldest),
        [0, 1, 2, 3]
    );
    assert_eq!(
        query(&clips, "", None, false, &favorites, LibrarySort::NameAsc),
        [2, 3, 1, 0]
    );
    assert_eq!(
        query(&clips, "", None, false, &favorites, LibrarySort::NameDesc),
        [1, 0, 3, 2]
    );
    assert_eq!(
        query(&clips, "", None, false, &favorites, LibrarySort::Duration),
        [1, 2, 0, 3]
    );
    assert_eq!(
        query(&clips, "", None, false, &favorites, LibrarySort::Size),
        [3, 0, 2, 1]
    );
}

#[test]
fn selection_supports_single_toggle_range_select_all_retain_and_clear() {
    let ordered = ["a", "b", "c", "d"].map(str::to_owned);
    let mut selection = Selection::default();

    selection.select("b", &ordered, false, false);
    assert_eq!(selection.ids, BTreeSet::from(["b".to_owned()]));
    assert_eq!(selection.anchor.as_deref(), Some("b"));

    selection.select("b", &ordered, true, false);
    assert!(selection.ids.is_empty());
    selection.select("a", &ordered, false, false);
    selection.select("c", &ordered, false, true);
    assert_eq!(
        selection.ids,
        BTreeSet::from(["a".to_owned(), "b".to_owned(), "c".to_owned()])
    );
    selection.select("d", &ordered, true, true);
    assert_eq!(
        selection.ids,
        BTreeSet::from([
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "d".to_owned()
        ])
    );

    selection.retain(&[
        test_clip("a", "a", 1, 1, 1),
        test_clip("c", "c", 1, 1, 1),
        test_clip("d", "d", 1, 1, 1),
    ]);
    assert_eq!(
        selection.ids,
        BTreeSet::from(["a".to_owned(), "c".to_owned(), "d".to_owned()])
    );
    assert_eq!(selection.anchor.as_deref(), Some("a"));

    selection.all(&ordered);
    assert_eq!(selection.ids, ordered.into_iter().collect());
    selection.clear();
    assert!(selection.ids.is_empty());
    assert!(selection.anchor.is_none());
}

#[test]
fn scan_ids_survive_rename_and_replacement_does_not_reuse_the_old_id() {
    let dir = TestDir::new();
    let original = dir.path().join("original.mp4");
    let replacement = dir.path().join("replacement.mp4");
    fs::write(&original, b"original bytes").expect("write original");
    fs::write(&replacement, b"replacement bytes").expect("write replacement");

    let first = scan(dir.path(), &[]).expect("initial scan");
    let original_clip = first
        .iter()
        .find(|clip| clip.name == "original")
        .expect("original clip")
        .clone();
    let renamed_path = rename(&original_clip, "renamed").expect("rename clip");
    let after_rename = scan(dir.path(), &[]).expect("scan renamed clip");
    let renamed = after_rename
        .iter()
        .find(|clip| clip.path == renamed_path)
        .expect("renamed clip");
    if original_clip.id.starts_with("win:") {
        assert_eq!(renamed.id, original_clip.id);
    }

    let replaced_source = dir.path().join("replacement.mp4");
    let selected_path = renamed.path.clone();
    fs::remove_file(&selected_path).expect("remove selected file");
    fs::rename(&replaced_source, &selected_path).expect("move replacement into old path");
    let after_replacement = scan(dir.path(), &[]).expect("scan replacement");
    let replacement_clip = after_replacement
        .iter()
        .find(|clip| clip.path == selected_path)
        .expect("replacement at selected path");
    assert_ne!(replacement_clip.id, renamed.id);
}

#[test]
fn rename_rejects_invalid_names_conflicts_and_replaced_paths() {
    let dir = TestDir::new();
    let source = dir.path().join("clip.mp4");
    fs::write(&source, b"selected video").expect("write selected clip");
    let clip = scan(dir.path(), &[]).expect("scan clip").remove(0);

    for invalid in [
        "",
        "  ",
        "CON",
        "com1",
        "bad:name",
        "trailing.",
        "trailing ",
    ] {
        let error = rename(&clip, invalid).expect_err(invalid);
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{invalid}");
    }

    let conflict = dir.path().join("occupied.mp4");
    fs::write(&conflict, b"keep me").expect("write conflict");
    assert!(rename(&clip, "occupied").is_err());
    assert_eq!(fs::read(&source).expect("read original"), b"selected video");
    assert_eq!(fs::read(&conflict).expect("read conflict"), b"keep me");

    let replacement = dir.path().join("replacement.mp4");
    fs::write(&replacement, b"different selected file").expect("write replacement");
    fs::remove_file(&source).expect("remove scanned file");
    fs::rename(&replacement, &source).expect("replace scanned path");
    let error = rename(&clip, "must-not-move").expect_err("replaced path must be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(source.exists());
    assert!(!dir.path().join("must-not-move.mp4").exists());
}

#[test]
fn verify_rejects_a_missing_or_replaced_path() {
    let dir = TestDir::new();
    let selected_path = dir.path().join("selected.mp4");
    let replacement_path = dir.path().join("replacement.mp4");
    fs::write(&selected_path, b"selected video").expect("write selected clip");
    let clip = scan(dir.path(), &[]).expect("scan selected clip").remove(0);
    verify(&clip).expect("indexed clip verifies");

    fs::write(&replacement_path, b"different replacement").expect("write replacement");
    fs::remove_file(&selected_path).expect("remove selected clip");
    fs::rename(&replacement_path, &selected_path).expect("replace selected path");
    let error = verify(&clip).expect_err("replacement must not pass verification");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
#[test]
fn scan_never_publishes_recorder_partial_mp4_files() {
    let dir = TestDir::new();
    fs::write(dir.path().join("capture.partial.mp4"), b"still recording").unwrap();
    fs::write(dir.path().join("capture.PARTIAL.MP4"), b"still recording").unwrap();
    assert!(scan(dir.path(), &[]).unwrap().is_empty());
}

#[cfg(windows)]
#[test]
fn scan_deduplicates_hard_links_by_file_identity() {
    let dir = TestDir::new();
    let first = dir.path().join("a.mp4");
    fs::write(&first, b"video").unwrap();
    fs::hard_link(&first, dir.path().join("b.mp4")).unwrap();
    let found = scan(dir.path(), &[]).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "a");
}
#[test]
fn compound_query_handles_ten_thousand_indexed_clips_without_filesystem_access() {
    let mut clips = Vec::with_capacity(10_000);
    let mut favorites = BTreeSet::new();
    for n in 0..10_000 {
        let id = format!("id_{n:05}");
        let mut clip = test_clip(&id, &format!("clip_{n:05}"), n, n, n);
        clip.source = Some(if n % 2 == 0 { "Game A" } else { "Game B" }.into());
        if n % 100 == 0 {
            favorites.insert(id);
        }
        clips.push(clip);
    }
    let started = std::time::Instant::now();
    let found = query(
        &clips,
        "CLIP_0",
        Some("game a"),
        true,
        &favorites,
        LibrarySort::Size,
    );
    assert_eq!(found.len(), 100);
    assert_eq!(clips[found[0]].size_bytes, 0);
    assert!(
        found
            .windows(2)
            .all(|p| clips[p[0]].size_bytes <= clips[p[1]].size_bytes)
    );
    eprintln!("10,000-entry compound query: {:?}", started.elapsed());
}
