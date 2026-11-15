# Clip save reliability

Status: audit of `main` at `4ab4130` plus the fixes in this change. Verified
with deterministic tests on macOS and on the GitHub Windows runners. **No clip
was saved on a physical Windows PC for this report**; the manual plan at the
end is still to be run.

## Save pipelines

### Instant replay (`clipper-replay::export`, export thread)

```text
SaveReplay (recorder worker)
  -> ReplayBuffer::snapshot (Arc clones, no I/O)
  -> name reserved in memory (ClipNames), ReplaySaving sent
  -> Exporter queue (1 running + 3 queued; full -> "Already saving clips")
export thread:
  prepare: create folder, pick a unique name (final and .partial must be free)
  export_clip:
    plan (keyframe start, trim)          -> NotReady / Invalid
    final name free?                     -> "clip already exists"
    free space >= estimate + 16 MiB?     -> DiskFull
    create <name>.mp4.partial (create_new; a stale one of a crashed run is replaced)
    write_mp4 into a 1 MiB BufWriter     -> DiskFull / AccessDenied / Io
    flush, sync_all (FlushFileBuffers)   -> "flushing clip" / "syncing clip"
    check_mp4 (box headers)              -> Incomplete           [new]
    commit_no_replace -> <name>.mp4      -> "clip already exists" / "finalizing clip"
  done(result) -> ReplaySaved{path} only for Ok, else ReplaySaveFailed{message}
UI: Saved toast + clip-saved sound only for ReplaySaved; Failed toast + error sound otherwise
```

### Manual recording (`clipper-recorder::windows_worker`, recorder worker)

```text
StartRecording: unique Clipper_<time>.mp4 (final and .partial free), create the .partial
  (precheck), Media Foundation sink writer writes into it
StopRecording: capture stops, encoder.finish() (drain + finalize), audio flushed
  classify_stop: no frames or finish error -> RecordingFailed, partial deleted
  name_audio_tracks (best effort)
  publish_recording:                                             [new]
    check_mp4 -> sync_all -> commit_no_replace (taken -> <stem>_2.mp4)
    not publishable -> RecordingFailed, partial kept (never deleted)
  RecordingFinished{path} with the actual published path
```

### Library (`clipper-storage::library::scan`)

The Library has no separate index that must be written for a clip to exist:
each refresh lists the clip folder and only completed `*.mp4` files are clips.
Cached metadata (duration, source) is keyed by file identity and only reused
for unchanged files. Partial files are never listed.

## Findings

Pre-existing guarantees that were confirmed and kept:

- Replay saves report success only after `export_clip` returned `Ok`, i.e.
  after write, flush, `sync_all` and the commit. `ReplaySaved` is sent from the
  export thread's `done` callback with exactly that result. The clip-saved
  sound is tied to `ReplaySaved` (`notifications.rs`, `sound_for`); failures
  play the system error sound.
- Partial files use a different name (`.mp4.partial`) and are ignored by the
  Library. Names are unique per save (in-memory reservation plus on-disk check
  on the export thread), and each save carries its own snapshot and result.
- Disk-full and permission errors are mapped to clear messages
  (`ReplayError::io`). The free-space check is only a pre-check; write errors
  are handled independently.
- Shutdown lets accepted saves finish (up to 10 s) and fails the rest with
  `Shutdown`; queued jobs never write anything.

Defects found and fixed:

| # | Defect | Effect | Fix | Regression test |
| --- | --- | --- | --- | --- |
| 1 | Manual recordings were committed with `fs::rename`, which **replaces** an existing file (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` on Windows, `rename(2)` elsewhere). | A file that took the final name during the recording (a Library clip renamed to it, a copied file) was silently overwritten when the recording stopped. | `publish_recording`: non-replacing commit; a taken name gets the next free `<stem>_N.mp4`. | `logic::tests::taken_name_is_never_overwritten` (fails with `fs::rename`) |
| 2 | Replay commit fell back to the replacing `fs::rename` (after an `exists()` check) when hard links fail. | On FAT/exFAT (USB drives, SD cards: no hard links), a file created between the check and the rename would be overwritten. | `publish::commit_no_replace`: `MoveFileW` on Windows (fails if the target exists, on every local file system). | `publish::tests::commit_publishes_and_never_replaces`, `export::tests::h_name_taken_at_commit_never_overwrites` (fails with a replacing rename). The FAT case itself needs a FAT volume (manual plan). |
| 3 | A panic during export (a bug, not an I/O error) skipped the partial cleanup; the worker caught the panic. | A `.mp4.partial` stayed until the next replay start's sweep (if older than 1 h). | `PartialGuard` removes the partial unless the commit succeeded, on any exit. | 7 export tests fail without the guard, among them `export::tests::j_panic_while_writing_removes_the_partial` |
| 4 | No check of what reached the disk; a commit could publish a truncated or damaged file if a lower layer lost data without an error. The manual path published whatever the sink writer left. | A broken MP4 reported as saved. | `publish::check_mp4`: top-level box walk (ftyp first, mdat, moov, sizes cover the file exactly) before every commit. | `export::tests::e_failure_while_finalizing_the_container` (silent truncation, same-length corruption), `logic::tests::unfinalized_recording_is_kept_not_published` |
| 5 | A transient lock on the finished partial (antivirus, indexer, sync client) failed the commit at once. For replays, the partial was then deleted. | A complete clip lost to a brief scan. | Bounded retry of transient errors (`PermissionDenied`, `ERROR_SHARING_VIOLATION`, `ERROR_LOCK_VIOLATION`): 4 retries, 375 ms in total, on the export/recorder worker. | `publish::tests::transient_errors_are_retried_a_bounded_number_of_times`, `export::tests::g_commit_failures_and_transient_locks` |
| 6 | Manual recordings were not flushed to the device before the commit (replays were). | After a power loss the published file could hold less than the sink writer wrote. | `sync_all` in `publish_recording`. | Covered by the publish tests (behavioural, not a power-loss test). |
| 7 | `library::scan` aborted on the first entry whose metadata could not be read, e.g. a clip deleted while the folder was listed. | The whole Library refresh failed and showed an error instead of the other clips. | Per-entry failures skip the entry; only a failing directory listing is an error. | `library::tests::vanished_or_unreadable_entries_do_not_hide_committed_clips` |

Not changed: the encoder, muxer, packet handling, capture, audio, naming
scheme, events and the notification sound logic.

## Guarantees and their limits

| Property | Replay clip | Manual recording |
| --- | --- | --- |
| Success reported only for a finalized, checked, committed file | yes | yes |
| Partial output never visible under a clip name, never listed | yes | yes |
| Existing file never replaced | yes (`MoveFileW`; elsewhere hard link) | yes |
| Atomic visibility (complete or absent) | yes, same volume (partial is next to the final file) | yes |
| File contents on the device before the commit | yes (`FlushFileBuffers`) | yes (new) |
| Commit (directory entry) durable across power loss | **no**: the rename is not flushed | **no** |
| Output decodable | **not verified**: structure only, no decode | same |

Durability: `FlushFileBuffers` pushes the file's data and metadata to the
device, but the new directory entry of the commit is not forced to disk. After
a power loss right after a save, the clip may still appear under its partial
name or be missing. A write cache that ignores flush commands weakens even
the content guarantee. Network shares and some removable-drive drivers may
implement flush and rename differently; a commit failure there is reported,
never hidden.

Validation cost: one `open` and about 3-4 reads of 8-16 bytes (top-level box
headers) per save. No decode, no checksum, nothing per frame. The retry only
adds time when the commit fails.

## Crash and restart

- A crash or forced termination while saving leaves `<name>.mp4.partial`. It
  is never shown in the Library or Recent Clips (both read `library::scan`).
- Replay partials (`Clipper_Replay_*.mp4.partial`) older than 1 hour are
  removed in the clip folder when a replay starts. Only files with exactly that
  pattern are touched, never clips or other files. The single-instance lock
  keeps a second clipuh from sweeping a running instance's partials.
- Manual recording partials (`Clipper_*.mp4.partial`) are not swept. One left
  by a crash (no `moov`) can still hold recoverable footage. A recording that
  finished but could not be published is kept and named in the failure
  message. The partial is only deleted when the recording itself failed (no
  frames encoded, or the encoder could not finish).
- A media repair tool for unfinalized files is out of scope.

## Concurrency

All replay exports run on one export thread in submission order; there is no
shared muxing state between jobs. Each job owns its snapshot, final name and
`done` callback, so a failure never affects another save's result. Manual
recording and replay never overlap (the recorder refuses one while the other
runs). A recording's stop-time commit runs on the recorder worker.

## Tests

| Test | Where | Scenario |
| --- | --- | --- |
| A | `export::tests::a_successful_save_publishes_one_checked_clip`, `exports_and_reports_real_values`, `logic::tests::finished_recording_is_published_under_its_name` | Success: exactly one new, checked clip |
| B | `export::tests::b_create_failure_reports_access_denied` | Partial cannot be created |
| C, D | `export::tests::c_d_write_failures_publish_nothing` | Device error, disk full (StorageFull, WriteZero) at 0, 100 bytes and mid-file |
| E | `export::tests::e_failure_while_finalizing_the_container`, `logic::tests::unfinalized_recording_is_kept_not_published` | Failure while writing the index; silent truncation; same-length corruption |
| F | `export::tests::f_flush_and_sync_failures_are_no_success` | Flush, sync |
| G | `export::tests::g_commit_failures_and_transient_locks` | Permanent and transient commit failures |
| H | `export::tests::h_name_taken_at_commit_never_overwrites`, `refuses_to_overwrite_existing_clip`, `logic::tests::taken_name_is_never_overwritten` | Existing final name |
| I | `export::tests::concurrent_submissions_complete_exactly_once`, `same_interval_saved_twice_gives_two_independent_clips`, `failed_save_does_not_hold_up_the_next_one` (existing) | Concurrent requests |
| J | `export::tests::j_panic_while_writing_removes_the_partial`; `tests/library.rs` partial tests and `logic::tests::sweep_removes_only_old_replay_partials` (existing) | Interrupted save, restart |
| K | `library::tests::vanished_or_unreadable_entries_do_not_hide_committed_clips` | Library refresh failures |
| L | `notifications::tests::only_a_finished_save_plays_the_clip_saved_sound`, `feedback::tests::save_success_only_after_saved_event` (existing) plus the `Ok`-only invariant of every export test | Success sound only after success |
| M | `export::tests::queued_jobs_get_shutdown_on_drop` (existing) | Cancellation: queued jobs write nothing. A running export is not cancelled; shutdown waits for it. |
| N | `export::tests::n_invalid_destination_fails_cleanly`, `missing_directory_fails_without_partial` | Destination is a file or missing |
| O | `export::tests::o_repeated_saves_never_leak_state` | 600 saves cycling through all faults |

The fault tests run the production `export_clip_with` with a small file-system
seam (`ExportFs`). Real files in a temporary folder whose name contains spaces
and non-ASCII characters. Each test checks the invariant: `Ok` means exactly
one new complete clip; `Err` means no new file, no partial, and the
pre-existing clip unchanged. Each fix was verified by reverting it and
confirming that the listed tests fail.

## Manual Windows validation (not yet run)

1. Save a 30 s and a 60 s replay; play both in Windows Media Player and VLC.
2. Save five clips within two seconds; all five appear, none overwritten.
3. Save while a game holds the GPU at 100 %.
4. Fill the clip drive to about 50 MB free, save: a disk-space failure toast,
   error sound, no `.mp4` and no `.partial` left.
5. Save to an exFAT USB stick (exercises fix 2), then to an NTFS one.
6. Pull the USB stick during a save: a failure, no success sound; nothing on
   the stick is presented as a clip after reinsertion.
7. Remove write permission on the clip folder: an access-denied failure.
8. End `clipuh.exe` in Task Manager during a save and during a manual
   recording; relaunch: no partial in the Library, the replay partial is
   swept after an hour, the recording partial is kept.
9. Rename a Library clip to the exact name of the running manual recording
   (`Clipper_<time>.mp4`), then stop the recording: both files exist (fix 1).
10. With real-time antivirus scanning the clip folder, save 20 clips.
11. Library and Recent Clips show exactly the saved clips after each step.
12. The clip-saved sound plays only for successful saves.
13. Previously saved clips are byte-identical before and after all steps.
