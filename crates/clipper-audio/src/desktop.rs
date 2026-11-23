//! Desktop remainder when two or more applications are recorded separately.
//!
//! WASAPI process loopback (`AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`) takes ONE
//! `TargetProcessId`: `EXCLUDE_TARGET_PROCESS_TREE` can leave out one process
//! tree, never two. "Everything except Spotify and Discord" is therefore
//! composed the other way round: one `INCLUDE_TARGET_PROCESS_TREE` stream per
//! process that owns an audio session and is not separated, summed in the
//! mixer. No signal is subtracted, so nothing depends on sample alignment of
//! independent loopback copies.
//!
//! This module decides WHICH processes to include (pure, platform
//! independent, unit tested); the Windows backend enumerates audio sessions
//! and the process table and runs the streams.
//!
//! Rules, all conservative towards "never record a separated app twice":
//! - a session process inside an excluded tree (itself, or any ancestor,
//!   matches an excluded target) is skipped: that audio has its own track;
//! - a session process whose own process tree contains an excluded process
//!   cannot be included (its INCLUDE stream would carry the excluded app
//!   too) and is reported as `unseparable`: that audio is missing from the
//!   desktop track instead of being duplicated;
//! - a process whose ancestor is already included is covered by that tree;
//! - the Windows system sounds session has no capturable process (pid 0) and
//!   is reported as `unseparable`;
//! - at most [`MAX_PARTS`] streams; the rest is reported.
//!
//! Assumption (verify on hardware): Windows resolves "process tree" from the
//! parent process ids that the process table reports.

use crate::types::ProcessTarget;

/// Upper bound of per-process streams of one composed desktop track.
pub const MAX_PARTS: usize = 16;

/// Label used for the Windows system sounds session (pid 0).
pub const SYSTEM_SOUNDS: &str = "Windows system sounds";

/// One row of the process table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent: u32,
    /// Executable file name, e.g. `"Spotify.exe"`.
    pub exe: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composition {
    /// Process ids to capture with INCLUDE_TARGET_PROCESS_TREE, ascending.
    pub include: Vec<u32>,
    /// Audio that cannot be part of the desktop track without duplicating a
    /// separated application, by executable name (sorted, deduplicated).
    pub unseparable: Vec<String>,
}

struct Table<'a> {
    processes: &'a [ProcessEntry],
}

impl<'a> Table<'a> {
    fn get(&self, pid: u32) -> Option<&'a ProcessEntry> {
        self.processes.iter().find(|p| p.pid == pid)
    }

    /// Strict ancestors of `pid`, nearest first. Stops at a missing parent,
    /// a self-parent (idle/system) or a cycle (pid reuse), bounded by the
    /// table size.
    fn ancestors(&self, pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut current = pid;
        while out.len() < self.processes.len() {
            let Some(entry) = self.get(current) else {
                break;
            };
            let parent = entry.parent;
            if parent == current || parent == pid || out.contains(&parent) {
                break;
            }
            if self.get(parent).is_none() {
                break;
            }
            out.push(parent);
            current = parent;
        }
        out
    }
}

fn matches(entry: &ProcessEntry, target: &ProcessTarget) -> bool {
    match target {
        ProcessTarget::Pid(pid) => entry.pid == *pid,
        ProcessTarget::Executable(name) => entry.exe.eq_ignore_ascii_case(name),
    }
}

fn is_target(entry: &ProcessEntry, excluded: &[ProcessTarget]) -> bool {
    excluded.iter().any(|t| matches(entry, t))
}

/// Decides the per-process streams of a desktop remainder that leaves out
/// `excluded`. `session_pids`: process ids of the audio sessions on the
/// render endpoints (`0` for the system sounds session).
pub fn compose(
    processes: &[ProcessEntry],
    session_pids: &[u32],
    excluded: &[ProcessTarget],
) -> Composition {
    let table = Table { processes };
    let mut unseparable: Vec<String> = Vec::new();
    let in_excluded_tree = |pid: u32| {
        table.get(pid).is_some_and(|e| is_target(e, excluded))
            || table
                .ancestors(pid)
                .into_iter()
                .any(|a| table.get(a).is_some_and(|e| is_target(e, excluded)))
    };
    let excluded_pids: Vec<u32> = processes
        .iter()
        .filter(|p| is_target(p, excluded))
        .map(|p| p.pid)
        .collect();

    let mut candidates: Vec<u32> = Vec::new();
    for &pid in session_pids {
        if candidates.contains(&pid) {
            continue;
        }
        if pid == 0 {
            if !unseparable.iter().any(|n| n == SYSTEM_SOUNDS) {
                unseparable.push(SYSTEM_SOUNDS.to_owned());
            }
            continue;
        }
        // Exited since the session was listed: nothing to capture.
        let Some(entry) = table.get(pid) else {
            continue;
        };
        if in_excluded_tree(pid) {
            continue;
        }
        // Its tree would carry a separated app as well.
        if excluded_pids
            .iter()
            .any(|&x| table.ancestors(x).contains(&pid))
        {
            unseparable.push(entry.exe.clone());
            continue;
        }
        candidates.push(pid);
    }

    // A candidate under another candidate is already in that tree. A mutual
    // "ancestry" (pid reuse made a cycle) covers neither: both stay.
    let mut include: Vec<u32> = candidates
        .iter()
        .copied()
        .filter(|&pid| {
            !table
                .ancestors(pid)
                .iter()
                .any(|&a| candidates.contains(&a) && !table.ancestors(a).contains(&pid))
        })
        .collect();
    include.sort_unstable();
    if include.len() > MAX_PARTS {
        for pid in include.split_off(MAX_PARTS) {
            if let Some(entry) = table.get(pid) {
                unseparable.push(entry.exe.clone());
            }
        }
    }
    unseparable.sort_by_key(|n| n.to_lowercase());
    unseparable.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    Composition {
        include,
        unseparable,
    }
}

/// Parts to stop and start when the composition changes from `running` to
/// `wanted` (both pid lists): `(stop, start)`.
pub fn part_changes(running: &[u32], wanted: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let stop = running
        .iter()
        .copied()
        .filter(|p| !wanted.contains(p))
        .collect();
    let start = wanted
        .iter()
        .copied()
        .filter(|p| !running.contains(p))
        .collect();
    (stop, start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, exe: &str) -> ProcessEntry {
        ProcessEntry {
            pid,
            parent,
            exe: exe.into(),
        }
    }

    fn exe(name: &str) -> ProcessTarget {
        ProcessTarget::Executable(name.into())
    }

    /// explorer (10) launched Steam (20) -> game (21); Spotify (30) and
    /// Discord (40, voice child 41) were started by updaters that exited
    /// (parent pids 99/98 are gone). Chrome (50) plays through its audio
    /// utility child (51).
    fn table() -> Vec<ProcessEntry> {
        vec![
            p(4, 0, "System"),
            p(10, 9, "explorer.exe"),
            p(20, 10, "steam.exe"),
            p(21, 20, "game.exe"),
            p(30, 99, "Spotify.exe"),
            p(31, 30, "Spotify.exe"),
            p(40, 98, "Discord.exe"),
            p(41, 40, "Discord.exe"),
            p(50, 10, "chrome.exe"),
            p(51, 50, "chrome.exe"),
        ]
    }

    fn spotify_discord() -> Vec<ProcessTarget> {
        vec![exe("Spotify.exe"), exe("Discord.exe")]
    }

    /// Bug 1 scenario: game, Spotify, Discord all play. The desktop track
    /// keeps the game; Spotify and Discord (incl. the voice child) are left
    /// to their own tracks.
    #[test]
    fn game_stays_in_desktop_while_two_apps_are_separated() {
        let c = compose(&table(), &[21, 31, 41], &spotify_discord());
        assert_eq!(c.include, [21]);
        assert!(c.unseparable.is_empty());
    }

    #[test]
    fn excluded_names_match_case_insensitively_and_cover_children() {
        let c = compose(
            &table(),
            &[30, 31, 40, 41, 51],
            &[exe("spotify.EXE"), exe("discord.exe")],
        );
        assert_eq!(c.include, [51]);
    }

    #[test]
    fn nested_sessions_use_the_topmost_process_once() {
        let c = compose(&table(), &[50, 51, 20, 21], &spotify_discord());
        assert_eq!(c.include, [20, 50]);
    }

    /// A process whose tree contains a separated app would duplicate it:
    /// reported, never included.
    #[test]
    fn ancestor_of_a_separated_app_is_unseparable() {
        let mut processes = table();
        processes.push(p(60, 10, "launcher.exe"));
        processes.push(p(61, 60, "Spotify.exe"));
        let c = compose(&processes, &[60, 21, 61], &spotify_discord());
        assert_eq!(c.include, [21]);
        assert_eq!(c.unseparable, ["launcher.exe"]);
        // explorer launched Spotify here: explorer sounds cannot be kept.
        let c = compose(&processes, &[10], &spotify_discord());
        assert!(c.include.is_empty());
        assert_eq!(c.unseparable, ["explorer.exe"]);
    }

    #[test]
    fn exited_processes_and_duplicates_are_ignored() {
        let c = compose(&table(), &[21, 21, 777], &spotify_discord());
        assert_eq!(c.include, [21]);
        assert!(c.unseparable.is_empty());
    }

    #[test]
    fn system_sounds_are_reported_not_captured() {
        let c = compose(&table(), &[0, 21], &spotify_discord());
        assert_eq!(c.include, [21]);
        assert_eq!(c.unseparable, [SYSTEM_SOUNDS]);
    }

    #[test]
    fn pid_targets_exclude_exactly_that_tree() {
        let c = compose(
            &table(),
            &[31, 41],
            &[ProcessTarget::Pid(30), exe("Discord.exe")],
        );
        assert!(c.include.is_empty());
    }

    /// A separated app that restarts gets a new pid; matching by executable
    /// keeps excluding it, and an unrelated process that reuses the old pid
    /// is not mistaken for it.
    #[test]
    fn restart_with_new_pid_and_pid_reuse() {
        let mut processes: Vec<ProcessEntry> = table()
            .into_iter()
            .filter(|e| e.pid != 30 && e.pid != 31)
            .collect();
        processes.push(p(130, 99, "Spotify.exe"));
        processes.push(p(30, 10, "notepad.exe"));
        let c = compose(&processes, &[130, 30, 21], &spotify_discord());
        assert_eq!(c.include, [21, 30]);
        assert!(c.unseparable.is_empty());
    }

    #[test]
    fn parent_cycles_terminate() {
        let processes = vec![p(1, 2, "a.exe"), p(2, 1, "b.exe"), p(3, 3, "c.exe")];
        let c = compose(&processes, &[1, 2, 3], &[exe("x.exe"), exe("y.exe")]);
        assert_eq!(c.include, [1, 2, 3]);
    }

    #[test]
    fn part_count_is_bounded_and_overflow_is_reported() {
        let mut processes = Vec::new();
        let mut sessions = Vec::new();
        for i in 0..(MAX_PARTS as u32 + 3) {
            processes.push(p(1000 + i, 1, &format!("app{i}.exe")));
            sessions.push(1000 + i);
        }
        let c = compose(&processes, &sessions, &spotify_discord());
        assert_eq!(c.include.len(), MAX_PARTS);
        assert_eq!(c.unseparable.len(), 3);
    }

    #[test]
    fn no_sessions_means_a_silent_but_valid_desktop() {
        assert_eq!(
            compose(&table(), &[], &spotify_discord()),
            Composition::default()
        );
    }

    #[test]
    fn part_changes_start_and_stop_only_differences() {
        assert_eq!(part_changes(&[1, 2, 3], &[2, 3, 4]), (vec![1], vec![4]));
        assert_eq!(part_changes(&[5], &[5]), (vec![], vec![]));
    }
}
