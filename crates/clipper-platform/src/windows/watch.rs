use std::io::{Error, ErrorKind, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_channel::{Sender, TrySendError};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FindCloseChangeNotification, FindFirstChangeNotificationW,
    FindNextChangeNotification,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows::core::PCWSTR;

const CHANGE_QUIET_PERIOD: Duration = Duration::from_millis(250);
const MAX_CHANGE_NOTIFICATION_DELAY: Duration = Duration::from_secs(1);

/// Watches a single directory for entry changes.
///
/// Dropping the returned value signals and joins its worker thread.
pub struct DirectoryWatch {
    // HANDLE wraps a raw pointer and is !Send. Store the opaque pointer value so
    // the watch can move between threads; Drop reconstructs it after the worker
    // has been joined.
    stop_event: isize,
    worker: Option<JoinHandle<()>>,
}

impl Drop for DirectoryWatch {
    fn drop(&mut self) {
        let stop_event = HANDLE(self.stop_event as *mut _);
        // SAFETY: this event remains valid until after the worker is joined.
        let _ = unsafe { SetEvent(stop_event) };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // SAFETY: this value was created by CreateEventW and is no longer used
        // by the worker after join.
        let _ = unsafe { CloseHandle(stop_event) };
    }
}

/// Watches `directory` without descending into subdirectories. Each burst of
/// changes sends a notification through `changed` after 250 ms of quiet, or
/// after at most one second during continuous writes. A full channel stays full.
pub fn watch(directory: &Path, changed: Sender<()>) -> Result<DirectoryWatch> {
    if directory.as_os_str().encode_wide().any(|unit| unit == 0) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "directory path contains an interior NUL",
        ));
    }
    let metadata = std::fs::metadata(directory)?;
    if !metadata.is_dir() {
        return Err(Error::new(
            ErrorKind::NotADirectory,
            format!("{} is not a directory", directory.display()),
        ));
    }

    let absolute_path = std::path::absolute(directory)?;
    let wide: Vec<u16> = absolute_path.as_os_str().encode_wide().chain([0]).collect();
    let change_event = unsafe {
        FindFirstChangeNotificationW(
            PCWSTR(wide.as_ptr()),
            false,
            FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_SIZE
                | FILE_NOTIFY_CHANGE_LAST_WRITE,
        )
    }
    .map_err(Error::other)?;
    let stop_event = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
        Ok(event) => event,
        Err(error) => {
            // SAFETY: `change_event` was created successfully and has not been
            // handed to a worker.
            let _ = unsafe { FindCloseChangeNotification(change_event) };
            return Err(Error::other(error));
        }
    };

    let change_handle = change_event.0 as isize;
    let stop_handle = stop_event.0 as isize;
    let worker = match thread::Builder::new()
        .name("clipper-directory-watch".into())
        .spawn(move || watch_loop(change_handle, stop_handle, changed))
    {
        Ok(worker) => worker,
        Err(error) => {
            // SAFETY: thread creation failed, so neither handle has been handed
            // to a worker.
            let _ = unsafe { FindCloseChangeNotification(change_event) };
            let _ = unsafe { CloseHandle(stop_event) };
            return Err(error);
        }
    };

    Ok(DirectoryWatch {
        stop_event: stop_handle,
        worker: Some(worker),
    })
}

fn watch_loop(change_handle: isize, stop_handle: isize, changed: Sender<()>) {
    let change_event = HANDLE(change_handle as *mut _);
    let stop_event = HANDLE(stop_handle as *mut _);
    let handles = [stop_event, change_event];
    let mut burst_started: Option<Instant> = None;
    let mut last_change: Option<Instant> = None;

    loop {
        // Idle waits are infinite. During a burst, the native wait sleeps until
        // either another change arrives or the quiet/max debounce deadline.
        let timeout = match (burst_started, last_change) {
            (Some(start), Some(last)) => {
                let deadline =
                    (last + CHANGE_QUIET_PERIOD).min(start + MAX_CHANGE_NOTIFICATION_DELAY);
                let remaining = deadline.saturating_duration_since(Instant::now());
                remaining
                    .as_nanos()
                    .div_ceil(1_000_000)
                    .min(u32::MAX as u128) as u32
            }
            _ => INFINITE,
        };
        // SAFETY: both handles remain valid for this worker's lifetime and the
        // slice contains exactly two handles. A finite timeout is used only
        // after a real change to finish a debounce window, never for idle work.
        let result = unsafe { WaitForMultipleObjects(&handles, false, timeout) };
        if result == WAIT_OBJECT_0 || result == WAIT_FAILED {
            break;
        }
        if result == WAIT_TIMEOUT {
            if burst_started.take().is_some() {
                last_change = None;
                if !notify_change(&changed) {
                    break;
                }
            }
            continue;
        }
        if result.0 != WAIT_OBJECT_0.0 + 1 {
            break;
        }

        let now = Instant::now();
        if let (Some(start), Some(last)) = (burst_started, last_change) {
            if now.duration_since(last) >= CHANGE_QUIET_PERIOD
                || now.duration_since(start) >= MAX_CHANGE_NOTIFICATION_DELAY
            {
                if !notify_change(&changed) {
                    break;
                }
                burst_started = Some(now);
            }
        } else {
            burst_started = Some(now);
        }
        last_change = Some(now);
        // The native notification must be rearmed after each signaled change.
        // Any changes made before this call are retained by the OS.
        // SAFETY: `change_event` is a live notification handle owned by this
        // worker and is rearmed once per successful wait.
        if unsafe { FindNextChangeNotification(change_event) }.is_err() {
            break;
        }
    }

    // SAFETY: the worker owns this handle and does not use it after closing.
    let _ = unsafe { FindCloseChangeNotification(change_event) };
}

fn notify_change(changed: &Sender<()>) -> bool {
    !matches!(changed.try_send(()), Err(TrySendError::Closed(())))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use async_channel::Receiver;

    use super::watch;

    fn temp_directory() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "clipper-platform-watch-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn wait_for_change(receiver: &Receiver<()>) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if receiver.try_recv().is_ok() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "directory change was not reported"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn missing_directory_returns_not_found() {
        let path = std::env::temp_dir().join(format!(
            "clipper-platform-missing-watch-{}",
            std::process::id()
        ));
        let (sender, _receiver) = async_channel::bounded(1);

        assert_eq!(
            watch(&path, sender).err().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn reports_external_create_rename_and_delete() {
        let directory = temp_directory();
        let (sender, receiver) = async_channel::bounded(1);
        let watcher = watch(&directory, sender).unwrap();
        let original = directory.join("before.txt");
        let renamed = directory.join("after.txt");

        fs::write(&original, b"created outside watcher").unwrap();
        wait_for_change(&receiver);
        fs::rename(&original, &renamed).unwrap();
        wait_for_change(&receiver);
        fs::remove_file(&renamed).unwrap();
        wait_for_change(&receiver);

        drop(watcher);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn coalesces_a_continuous_write_burst() {
        let directory = temp_directory();
        let file = directory.join("recording.partial.mp4");
        fs::write(&file, b"baseline").unwrap();
        let (sender, receiver) = async_channel::bounded(1);
        let watcher = watch(&directory, sender).unwrap();
        let started = Instant::now();

        let collector = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(2200);
            let mut notifications = Vec::new();
            while Instant::now() < deadline {
                if receiver.try_recv().is_ok() {
                    notifications.push(started.elapsed());
                } else {
                    thread::sleep(Duration::from_millis(2));
                }
            }
            notifications
        });

        for size in 1..=130 {
            fs::write(&file, vec![b'x'; size]).unwrap();
            thread::sleep(Duration::from_millis(10));
        }

        let notifications = collector.join().unwrap();
        assert!(
            !notifications.is_empty(),
            "the write burst was not reported"
        );
        assert!(
            notifications[0] < Duration::from_millis(1200),
            "continuous writes should produce a notification within about one second"
        );
        assert!(
            notifications.len() <= 3,
            "one write burst should be coalesced, got {} notifications",
            notifications.len()
        );

        drop(watcher);
        fs::remove_file(file).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn drop_stops_the_worker_promptly() {
        let directory = temp_directory();
        let (sender, _receiver) = async_channel::bounded(1);
        let watcher = watch(&directory, sender).unwrap();
        let (done, dropped) = mpsc::channel();

        thread::spawn(move || {
            drop(watcher);
            done.send(()).unwrap();
        });

        dropped
            .recv_timeout(Duration::from_secs(1))
            .expect("dropping a watcher should promptly stop its worker");
        fs::remove_dir(directory).unwrap();
    }
}
