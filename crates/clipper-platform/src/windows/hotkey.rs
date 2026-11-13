//! System-wide hotkey on a dedicated message-pump thread (zero CPU when idle).

use std::fmt;
use std::sync::mpsc;
use std::thread::JoinHandle;

use clipper_core::hotkey::Hotkey;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP, WM_HOTKEY, WM_QUIT,
};
use windows::core::HRESULT;

const WM_REQUEST: u32 = WM_APP + 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterError {
    InUse,
    Failed(String),
    Stopped,
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InUse => f.write_str("Another app already uses this shortcut."),
            Self::Failed(m) => f.write_str(m),
            Self::Stopped => f.write_str("The hotkey service is not running."),
        }
    }
}

impl std::error::Error for RegisterError {}

type Reply = async_channel::Sender<Result<(), RegisterError>>;
type Request = (Option<Hotkey>, Reply);

/// Owner of the hotkey thread; dropping it unregisters and stops the thread.
pub struct GlobalHotkey {
    handle: HotkeyHandle,
    thread: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug)]
pub struct HotkeyHandle {
    thread_id: u32,
    tx: mpsc::Sender<Request>,
}

impl GlobalHotkey {
    /// `on_press` runs on the hotkey thread for each press and must not block.
    pub fn spawn(on_press: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Request>();
        let (ready_tx, ready_rx) = mpsc::channel::<u32>();
        let thread = std::thread::Builder::new()
            .name("clipper-hotkey".into())
            .spawn(move || run(rx, ready_tx, on_press))?;
        match ready_rx.recv() {
            Ok(thread_id) => Ok(Self {
                handle: HotkeyHandle { thread_id, tx },
                thread: Some(thread),
            }),
            Err(_) => Err(std::io::Error::other("hotkey thread failed to start")),
        }
    }

    pub fn handle(&self) -> HotkeyHandle {
        self.handle.clone()
    }
}

impl Drop for GlobalHotkey {
    fn drop(&mut self) {
        post(self.handle.thread_id, WM_QUIT);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn post(thread_id: u32, msg: u32) -> bool {
    // SAFETY: plain value arguments; fails harmlessly if the thread is gone.
    unsafe { PostThreadMessageW(thread_id, msg, WPARAM(0), LPARAM(0)) }.is_ok()
}

impl HotkeyHandle {
    /// Replaces the registered hotkey (None unregisters). The new one is
    /// registered before the old is released, so failure keeps the old one.
    pub fn set(
        &self,
        hotkey: Option<Hotkey>,
    ) -> async_channel::Receiver<Result<(), RegisterError>> {
        let (reply, rx) = async_channel::bounded(1);
        if self.tx.send((hotkey, reply.clone())).is_err() || !post(self.thread_id, WM_REQUEST) {
            let _ = reply.try_send(Err(RegisterError::Stopped));
        }
        rx
    }
}

fn modifiers(h: &Hotkey) -> HOT_KEY_MODIFIERS {
    let m = h.modifiers;
    let mut f = MOD_NOREPEAT;
    if m.ctrl {
        f |= MOD_CONTROL;
    }
    if m.alt {
        f |= MOD_ALT;
    }
    if m.shift {
        f |= MOD_SHIFT;
    }
    if m.win {
        f |= MOD_WIN;
    }
    f
}

fn register(id: i32, h: &Hotkey) -> Result<(), RegisterError> {
    // SAFETY: thread-level registration (no hwnd); plain values.
    unsafe { RegisterHotKey(None, id, modifiers(h), u32::from(h.key.virtual_key())) }.map_err(|e| {
        // ERROR_HOTKEY_ALREADY_REGISTERED
        if e.code() == HRESULT::from_win32(1409) {
            RegisterError::InUse
        } else {
            RegisterError::Failed(e.message())
        }
    })
}

fn run(rx: mpsc::Receiver<Request>, ready: mpsc::Sender<u32>, on_press: impl Fn()) {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid out-pointer; PM_NOREMOVE only forces queue creation.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_APP, WM_APP, PM_NOREMOVE);
        let _ = ready.send(GetCurrentThreadId());
    }
    let mut active: Option<(i32, Hotkey)> = None;
    loop {
        // SAFETY: `msg` is a valid out-pointer; None = any message for this thread.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        match msg.message {
            WM_HOTKEY if active.is_some_and(|(id, _)| msg.wParam.0 == id as usize) => on_press(),
            WM_REQUEST => {
                while let Ok((want, reply)) = rx.try_recv() {
                    let _ = reply.try_send(apply(&mut active, want));
                }
            }
            _ => {}
        }
    }
    if let Some((id, _)) = active {
        // SAFETY: id was registered by this thread.
        let _ = unsafe { UnregisterHotKey(None, id) };
    }
}

fn apply(active: &mut Option<(i32, Hotkey)>, want: Option<Hotkey>) -> Result<(), RegisterError> {
    if active.map(|(_, h)| h) == want {
        return Ok(());
    }
    let new = match want {
        Some(h) => {
            let id = active.map_or(1, |(id, _)| 3 - id);
            register(id, &h)?;
            Some((id, h))
        }
        None => None,
    };
    if let Some((old, _)) = std::mem::replace(active, new) {
        // SAFETY: old id was registered by this thread.
        let _ = unsafe { UnregisterHotKey(None, old) };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipper_core::hotkey::{Key, Modifiers};

    fn hk(f: u8) -> Hotkey {
        Hotkey {
            modifiers: Modifiers {
                ctrl: true,
                alt: true,
                shift: true,
                win: false,
            },
            key: Key::Function(f),
        }
    }

    fn set(h: &HotkeyHandle, k: Option<Hotkey>) -> Result<(), RegisterError> {
        h.set(k).recv_blocking().unwrap()
    }

    #[test]
    fn register_same_twice_and_unset() {
        let g = GlobalHotkey::spawn(|| {}).unwrap();
        let h = g.handle();
        assert_eq!(set(&h, Some(hk(24))), Ok(()));
        assert_eq!(set(&h, Some(hk(24))), Ok(()));
        assert_eq!(set(&h, None), Ok(()));
    }

    #[test]
    fn change_registers_new_and_releases_old() {
        let first = GlobalHotkey::spawn(|| {}).unwrap();
        let other = GlobalHotkey::spawn(|| {}).unwrap();
        assert_eq!(set(&first.handle(), Some(hk(21))), Ok(()));
        assert_eq!(set(&first.handle(), Some(hk(20))), Ok(()));
        // The old combo is free again, the new one is held.
        assert_eq!(set(&other.handle(), Some(hk(21))), Ok(()));
        assert_eq!(
            set(&other.handle(), Some(hk(20))),
            Err(RegisterError::InUse)
        );
    }

    #[test]
    fn conflict_keeps_previous() {
        let a = GlobalHotkey::spawn(|| {}).unwrap();
        let b = GlobalHotkey::spawn(|| {}).unwrap();
        let (ha, hb) = (a.handle(), b.handle());
        assert_eq!(set(&ha, Some(hk(23))), Ok(()));
        assert_eq!(set(&hb, Some(hk(22))), Ok(()));
        assert_eq!(set(&hb, Some(hk(23))), Err(RegisterError::InUse));
        // b still holds F22, so a cannot take it.
        assert_eq!(set(&ha, Some(hk(22))), Err(RegisterError::InUse));
        assert_eq!(set(&hb, None), Ok(()));
        assert_eq!(set(&ha, Some(hk(22))), Ok(()));
    }

    #[test]
    fn stopped_after_drop() {
        let g = GlobalHotkey::spawn(|| {}).unwrap();
        let h = g.handle();
        drop(g);
        assert_eq!(set(&h, None), Err(RegisterError::Stopped));
    }

    /// Measures key-down to `on_press` latency with injected input. Needs an
    /// interactive desktop session, so it is opt-in:
    /// `cargo test -p clipper-platform -- --ignored --nocapture hotkey_latency`.
    #[test]
    #[ignore = "injects keyboard input; needs an interactive desktop"]
    fn hotkey_latency() {
        use std::time::{Duration, Instant};
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
            SendInput, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
        };

        let (tx, rx) = mpsc::channel();
        let g = GlobalHotkey::spawn(move || {
            let _ = tx.send(Instant::now());
        })
        .unwrap();
        let hotkey = hk(22);
        assert_eq!(set(&g.handle(), Some(hotkey)), Ok(()));

        let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    ..Default::default()
                },
            },
        };
        let target = VIRTUAL_KEY(hotkey.key.virtual_key());
        let mods = [VK_CONTROL, VK_MENU, VK_SHIFT];
        let mut samples = Vec::new();
        for _ in 0..20 {
            let mut down: Vec<INPUT> = mods.iter().map(|&vk| key(vk, false)).collect();
            down.push(key(target, false));
            let mut up = vec![key(target, true)];
            up.extend(mods.iter().rev().map(|&vk| key(vk, true)));
            let sent = Instant::now();
            // SAFETY: valid INPUT array and matching element size.
            unsafe { SendInput(&down, size_of::<INPUT>() as i32) };
            let fired = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("hotkey fired");
            // SAFETY: as above; releases every key pressed.
            unsafe { SendInput(&up, size_of::<INPUT>() as i32) };
            samples.push(fired - sent);
            std::thread::sleep(Duration::from_millis(30));
        }
        samples.sort();
        println!(
            "hotkey latency over {} presses: p50 {:?}, p95 {:?}, max {:?}",
            samples.len(),
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100],
            samples[samples.len() - 1]
        );
    }
}
