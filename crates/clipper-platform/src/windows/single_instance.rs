//! One running app per user session: a named mutex marks the first instance,
//! and a named auto-reset event lets a later start ask it to come to the
//! front instead of starting a second replay, tray icon and hotkey.

use std::io::{Error, Result};
use std::thread::{self, JoinHandle};

use async_channel::{Sender, TrySendError};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForMultipleObjects,
};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
use windows::core::HSTRING;

/// Outcome of [`acquire`].
pub enum Instance {
    /// This process is the first instance; keep the lock alive while running.
    Primary(InstanceLock),
    /// Another instance of the same `name` runs in this session.
    AlreadyRunning(Activator),
}

/// Held by the first instance. Dropping it releases the name.
pub struct InstanceLock {
    // HANDLEs are !Send; the opaque values are stored so the lock can move.
    mutex: isize,
    activate: isize,
}

/// Lets a later start bring the first instance to the front.
pub struct Activator {
    activate: isize,
}

/// Delivers activation requests; dropping it stops and joins its thread.
pub struct ActivationListener {
    stop: isize,
    worker: Option<JoinHandle<()>>,
}

/// Claims `name` for this user session (`Local\` kernel namespace).
pub fn acquire(name: &str) -> Result<Instance> {
    // The event exists before the mutex, so a start that sees the mutex can
    // always open the event.
    let event_name = HSTRING::from(format!("Local\\{name}-activate"));
    // SAFETY: valid NUL-terminated name; default security; auto-reset,
    // initially unsignalled. An existing event is opened instead.
    let activate =
        unsafe { CreateEventW(None, false, false, &event_name) }.map_err(Error::other)?;
    let mutex_name = HSTRING::from(format!("Local\\{name}-instance"));
    // SAFETY: valid NUL-terminated name; not owned. GetLastError is read
    // right after the call, before anything else can change it.
    let (mutex, existed) = match unsafe { CreateMutexW(None, false, &mutex_name) } {
        Ok(mutex) => (mutex, unsafe { GetLastError() } == ERROR_ALREADY_EXISTS),
        Err(error) => {
            // SAFETY: the event was created above and is not shared yet.
            let _ = unsafe { CloseHandle(activate) };
            return Err(Error::other(error));
        }
    };
    if existed {
        // SAFETY: only the first instance's handle keeps the name alive.
        let _ = unsafe { CloseHandle(mutex) };
        return Ok(Instance::AlreadyRunning(Activator {
            activate: activate.0 as isize,
        }));
    }
    Ok(Instance::Primary(InstanceLock {
        mutex: mutex.0 as isize,
        activate: activate.0 as isize,
    }))
}

impl InstanceLock {
    /// Sends `()` on `activated` for every activation request (a full channel
    /// drops the request; one pending request is enough).
    pub fn listen(&self, activated: Sender<()>) -> Result<ActivationListener> {
        // SAFETY: unnamed manual-reset event, initially unsignalled.
        let stop = unsafe { CreateEventW(None, true, false, None) }.map_err(Error::other)?;
        let (stop_handle, activate_handle) = (stop.0 as isize, self.activate);
        let worker = thread::Builder::new()
            .name("clipuh-single-instance".into())
            .spawn(move || listen_loop(activate_handle, stop_handle, activated));
        match worker {
            Ok(worker) => Ok(ActivationListener {
                stop: stop_handle,
                worker: Some(worker),
            }),
            Err(error) => {
                // SAFETY: the thread did not start, so nothing else uses it.
                let _ = unsafe { CloseHandle(stop) };
                Err(error)
            }
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // SAFETY: both handles were created in `acquire` and are owned here.
        // A listener must be dropped first (it borrows the event by value);
        // the app drops it before the lock.
        unsafe {
            let _ = CloseHandle(HANDLE(self.mutex as *mut _));
            let _ = CloseHandle(HANDLE(self.activate as *mut _));
        }
    }
}

impl Activator {
    /// Asks the first instance to show its window and lets it take the
    /// foreground (only the foreground process may hand that right on).
    pub fn activate(&self) -> Result<()> {
        // SAFETY: plain value argument; failure only means no foreground.
        let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        // SAFETY: the event handle is owned by this activator.
        unsafe { SetEvent(HANDLE(self.activate as *mut _)) }.map_err(Error::other)
    }
}

impl Drop for Activator {
    fn drop(&mut self) {
        // SAFETY: created in `acquire`, owned here.
        let _ = unsafe { CloseHandle(HANDLE(self.activate as *mut _)) };
    }
}

impl Drop for ActivationListener {
    fn drop(&mut self) {
        let stop = HANDLE(self.stop as *mut _);
        // SAFETY: the stop event stays valid until after the worker is joined.
        let _ = unsafe { SetEvent(stop) };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // SAFETY: created in `listen`; the worker no longer uses it.
        let _ = unsafe { CloseHandle(stop) };
    }
}

fn listen_loop(activate: isize, stop: isize, activated: Sender<()>) {
    let handles = [HANDLE(stop as *mut _), HANDLE(activate as *mut _)];
    loop {
        // SAFETY: both handles outlive this thread (see the Drop impls).
        let result = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        if result.0 != WAIT_OBJECT_0.0 + 1 {
            // Stop requested, or the wait failed: nothing more to deliver.
            return;
        }
        if let Err(TrySendError::Closed(())) = activated.try_send(()) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    fn unique_name() -> String {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        format!(
            "clipuh-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn second_acquire_sees_the_first_and_drop_frees_the_name() {
        let name = unique_name();
        let first = acquire(&name).unwrap();
        assert!(matches!(first, Instance::Primary(_)));
        assert!(matches!(
            acquire(&name).unwrap(),
            Instance::AlreadyRunning(_)
        ));
        drop(first);
        assert!(matches!(acquire(&name).unwrap(), Instance::Primary(_)));
    }

    #[test]
    fn activation_reaches_the_listener() {
        let name = unique_name();
        let Instance::Primary(lock) = acquire(&name).unwrap() else {
            panic!("expected the first instance");
        };
        let (tx, rx) = async_channel::bounded(1);
        let listener = lock.listen(tx).unwrap();
        let Instance::AlreadyRunning(activator) = acquire(&name).unwrap() else {
            panic!("expected a running instance");
        };
        activator.activate().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while rx.try_recv().is_err() {
            assert!(std::time::Instant::now() < deadline, "no activation");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(listener);
        drop(lock);
    }

    #[test]
    fn request_before_listening_is_kept() {
        let name = unique_name();
        let Instance::Primary(lock) = acquire(&name).unwrap() else {
            panic!("expected the first instance");
        };
        let Instance::AlreadyRunning(activator) = acquire(&name).unwrap() else {
            panic!("expected a running instance");
        };
        activator.activate().unwrap();
        let (tx, rx) = async_channel::bounded(1);
        let _listener = lock.listen(tx).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while rx.try_recv().is_err() {
            assert!(std::time::Instant::now() < deadline, "request lost");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn dropping_the_listener_stops_it() {
        let name = unique_name();
        let Instance::Primary(lock) = acquire(&name).unwrap() else {
            panic!("expected the first instance");
        };
        let (tx, _rx) = async_channel::bounded(1);
        let listener = lock.listen(tx).unwrap();
        drop(listener); // joins; would hang if the stop event were ignored
    }
}
