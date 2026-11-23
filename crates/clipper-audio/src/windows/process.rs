//! WASAPI process loopback (include / exclude a process tree).
//!
//! Requires Windows 10 build 20348+ / Windows 11 (see [`capabilities`]).
//! Process loopback clients are activated asynchronously through
//! `ActivateAudioInterfaceAsync` on the virtual device
//! `VAD\Process_Loopback`; the result is then initialized like any other
//! loopback client by [`init_capture_client`].

use std::mem::ManuallyDrop;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, STILL_ACTIVE, WAIT_OBJECT_0,
};
use windows::Win32::Media::Audio::{
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioClient, PROCESS_LOOPBACK_MODE, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{BLOB, IAgileObject, IAgileObject_Impl};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Threading::{
    CreateEventW, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, SetEvent, WaitForMultipleObjects,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows_core::{HRESULT, Interface, implement};

use super::endpoint::loopback_source;
use super::stream::{OpenedStream, OwnedHandle, RETRY_POLL, StreamSource, init_capture_client};
use crate::types::{AudioCapabilities, AudioError, ProcessTarget};

/// First Windows build with process loopback (Windows 11 / Server 2022).
const MIN_PROCESS_LOOPBACK_BUILD: u32 = 20_348;
/// Upper bound for the asynchronous activation.
const ACTIVATION_TIMEOUT_MS: u32 = 5_000;
/// Minimum spacing of process-table scans in `needs_reopen`.
const RESCAN_INTERVAL: Duration = Duration::from_secs(1);
/// Poll spacing while an exclude stream still captures the full mix because
/// its target is not running: short, so the switch to exclusion (and with it
/// the end of any overlap with the target's own track) comes quickly.
const FALLBACK_POLL: Duration = Duration::from_millis(250);

/// OS build number from `RtlGetVersion` (not subject to manifest shimming,
/// unlike `GetVersionEx`). Cached. `0` when the call fails.
fn os_build() -> u32 {
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| {
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is a valid OSVERSIONINFOW with its size field set.
        let status = unsafe { RtlGetVersion(&mut info) };
        if status.is_ok() {
            info.dwBuildNumber
        } else {
            0
        }
    })
}

/// Probes process loopback support on this Windows build (build >= 20348).
/// Needs no COM; callable from any thread.
pub fn capabilities() -> AudioCapabilities {
    AudioCapabilities {
        process_loopback: os_build() >= MIN_PROCESS_LOOPBACK_BUILD,
    }
}

/// Whether per-application capture (process loopback) works on this Windows
/// build. When `false`, application sources are reported as unavailable and
/// never planned (see [`crate::routing::RoutingWarning`]).
pub fn process_loopback_supported() -> bool {
    capabilities().process_loopback
}

/// Root process id of `target` (for executables: the instance whose parent
/// is not the same executable), if running. Needs no COM.
///
/// Discord runs many `Discord.exe` processes; the voice engine lives in a
/// child, so the root (whose process tree includes the children) is used with
/// `INCLUDE_TARGET_PROCESS_TREE`. With several roots the one with the most
/// direct same-exe children wins, then the lowest pid.
pub fn find_process(target: &ProcessTarget) -> Option<u32> {
    match target {
        ProcessTarget::Pid(pid) => is_alive(*pid).then_some(*pid),
        ProcessTarget::Executable(name) => find_executable_root(name),
    }
}

fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: plain Win32 call; the returned handle is wrapped and closed.
    let raw = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(raw) => raw,
        // Protected / anti-cheat processes refuse the open but exist.
        Err(e) => return e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0),
    };
    let process = OwnedHandle(raw);
    let mut code = 0u32;
    // SAFETY: `process` is a valid process handle, `code` a valid out pointer.
    let ok = unsafe { GetExitCodeProcess(process.0, &mut code) }.is_ok();
    ok && code == STILL_ACTIVE.0 as u32
}

fn exe_name(entry: &PROCESSENTRY32W) -> &[u16] {
    let len = entry
        .szExeFile
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(entry.szExeFile.len());
    &entry.szExeFile[..len]
}

fn find_executable_root(name: &str) -> Option<u32> {
    let wanted: Vec<u16> = name.to_lowercase().encode_utf16().collect();
    if wanted.is_empty() {
        return None;
    }
    // SAFETY: plain Win32 call; handle wrapped for closing.
    let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?);

    // (pid, parent pid) of every instance of the executable.
    let mut instances: Vec<(u32, u32)> = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: valid snapshot handle and entry with dwSize set.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
    while more {
        let exe = exe_name(&entry);
        if exe.len() == wanted.len() && exe.iter().zip(&wanted).all(|(&a, &b)| lower_utf16(a) == b)
        {
            instances.push((entry.th32ProcessID, entry.th32ParentProcessID));
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
    }

    let is_instance = |pid: u32| instances.iter().any(|&(p, _)| p == pid);
    instances
        .iter()
        .filter(|&&(pid, ppid)| ppid == pid || !is_instance(ppid))
        .map(|&(pid, _)| {
            let children = instances.iter().filter(|&&(_, pp)| pp == pid).count();
            (std::cmp::Reverse(children), pid)
        })
        .min()
        .map(|(_, pid)| pid)
}

/// Snapshot of the process table (pid, parent pid, executable name) for the
/// composed desktop remainder. Empty when the snapshot fails.
pub(crate) fn process_table() -> Vec<crate::desktop::ProcessEntry> {
    // SAFETY: plain Win32 call; handle wrapped for closing.
    let Ok(raw) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };
    let snapshot = OwnedHandle(raw);
    let mut out = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: valid snapshot handle and entry with dwSize set.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
    while more {
        out.push(crate::desktop::ProcessEntry {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            exe: String::from_utf16_lossy(exe_name(&entry)),
        });
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
    }
    out
}

/// Lowercases one UTF-16 unit (BMP simple mapping; exe names are ASCII in
/// practice, and the needle is lowercased with full Unicode rules).
fn lower_utf16(unit: u16) -> u16 {
    let Some(c) = char::from_u32(u32::from(unit)) else {
        return unit;
    };
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => u16::try_from(u32::from(l)).unwrap_or(unit),
        _ => unit,
    }
}

/// Win32 event shared between the completion handler and the waiting caller.
struct SharedEvent(HANDLE);

// SAFETY: an event handle may be signaled/waited from any thread; it is
// closed only when the last Arc is dropped.
unsafe impl Send for SharedEvent {}
// SAFETY: as above, the handle value is immutable and thread-agnostic.
unsafe impl Sync for SharedEvent {}

impl Drop for SharedEvent {
    fn drop(&mut self) {
        // SAFETY: we own the handle; closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Completion handler. Must be agile: the callback arrives on an MTA worker
/// thread, so it also implements `IAgileObject` (otherwise activation fails).
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct ActivationHandler {
    done: Arc<SharedEvent>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _operation: windows_core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows_core::Result<()> {
        // SAFETY: the event is kept alive by the Arc held in `self`.
        unsafe { SetEvent(self.done.0) }
    }
}

impl IAgileObject_Impl for ActivationHandler_Impl {}

/// Activates a process loopback `IAudioClient` for `pid`. The wait for the
/// asynchronous completion is interrupted by `stop` (`SourceUnavailable`).
fn activate_process_client(
    pid: u32,
    mode: PROCESS_LOOPBACK_MODE,
    stop: Option<&OwnedHandle>,
) -> Result<IAudioClient, AudioError> {
    let os = |what: &str, e: windows_core::Error| AudioError::Os(format!("{what}: {e}"));

    // SAFETY: plain Win32 call.
    let event =
        unsafe { CreateEventW(None, true, false, None) }.map_err(|e| os("CreateEventW", e))?;
    let done = Arc::new(SharedEvent(event));
    let handler: IActivateAudioInterfaceCompletionHandler =
        ActivationHandler { done: done.clone() }.into();

    // Heap-allocated so that, if the activation outlives this call (timeout /
    // stop), the memory it may still read can be leaked instead of freed.
    let params_ptr = Box::into_raw(Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: mode,
            },
        },
    }));
    // The PROPVARIANT only borrows the params blob; ManuallyDrop + never
    // calling PropVariantClear guarantees nothing tries to free it as COM
    // memory. Boxed for the same leak-on-timeout reason as the params.
    let prop = Box::new(ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: params_ptr.cast::<u8>(),
                    },
                },
            }),
        },
    }));

    // SAFETY: `prop` points at the heap `params_ptr` block, both alive for the
    // call; `handler` is a valid agile handler.
    let started = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&**prop),
            &handler,
        )
    };
    let operation = match started {
        Ok(operation) => operation,
        Err(e) => {
            // SAFETY: the call failed, so nothing retains the params block;
            // it came from `Box::into_raw` above and is freed once.
            drop(unsafe { Box::from_raw(params_ptr) });
            return Err(os("ActivateAudioInterfaceAsync", e));
        }
    };

    let mut waits = [done.0, HANDLE::default()];
    let count = match stop {
        Some(stop) => {
            waits[1] = stop.0;
            2
        }
        None => 1,
    };
    // SAFETY: valid event handles: `done` is kept alive by the Arc, `stop` by
    // the caller's reference.
    let wait = unsafe { WaitForMultipleObjects(&waits[..count], false, ACTIVATION_TIMEOUT_MS) };
    if wait != WAIT_OBJECT_0 {
        // The activation may still be running and reading the params / prop
        // blob. Leak both on purpose: a few bytes are cheaper than a
        // use-after-free in the audio service callback.
        Box::leak(prop);
        let _ = params_ptr;
        return Err(if wait.0 == WAIT_OBJECT_0.0 + 1 {
            AudioError::SourceUnavailable("stopping".to_owned())
        } else {
            AudioError::Os("process loopback activation timed out".to_owned())
        });
    }
    // SAFETY: activation completed, so nothing reads the blob any more; the
    // pointer came from `Box::into_raw` above and is freed exactly once.
    drop(unsafe { Box::from_raw(params_ptr) });
    drop(prop);

    let mut hr = windows_core::HRESULT(0);
    let mut unknown: Option<windows_core::IUnknown> = None;
    // SAFETY: valid out pointers; the operation has completed.
    unsafe { operation.GetActivateResult(&mut hr, &mut unknown) }
        .map_err(|e| os("GetActivateResult", e))?;
    hr.ok().map_err(|e| os("process loopback activation", e))?;
    unknown
        .ok_or_else(|| AudioError::Os("activation returned no interface".to_owned()))?
        .cast::<IAudioClient>()
        .map_err(|e| os("IAudioClient cast", e))
}

/// Opens a handle that signals when the process exits and checks the process
/// is still running. `Ok(None)`: the process exists but the handle cannot be
/// obtained (protected process); callers then poll liveness instead.
/// `Err(SourceUnavailable)`: the process is gone.
fn open_exit_handle(pid: u32) -> Result<Option<OwnedHandle>, AudioError> {
    // SAFETY: plain Win32 call.
    let raw = match unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
    } {
        Ok(raw) => raw,
        Err(e) if e.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
            return Err(AudioError::SourceUnavailable(format!(
                "process {pid} is not running"
            )));
        }
        Err(_) => return Ok(None),
    };
    let handle = OwnedHandle(raw);
    let mut code = 0u32;
    // SAFETY: `handle` is a valid process handle, `code` a valid out pointer.
    let queried = unsafe { GetExitCodeProcess(handle.0, &mut code) }.is_ok();
    if queried && code != STILL_ACTIVE.0 as u32 {
        return Err(AudioError::SourceUnavailable(format!(
            "process {pid} has exited"
        )));
    }
    Ok(Some(handle))
}

fn describe(target: &ProcessTarget, pid: u32) -> String {
    match target {
        ProcessTarget::Executable(name) => format!("{name} (pid {pid})"),
        ProcessTarget::Pid(_) => format!("process {pid}"),
    }
}

fn target_label(target: &ProcessTarget) -> String {
    match target {
        ProcessTarget::Executable(name) => name.clone(),
        ProcessTarget::Pid(pid) => format!("process {pid}"),
    }
}

/// Opens the exit handle first (and verifies the process is alive), then
/// activates for that pid and uses the handle as the stream's reopen signal.
fn open_process_stream(
    target: &ProcessTarget,
    pid: u32,
    mode: PROCESS_LOOPBACK_MODE,
    channels: u16,
    stop: Option<&OwnedHandle>,
) -> Result<OpenedStream, AudioError> {
    let exit = open_exit_handle(pid)?;
    let client = activate_process_client(pid, mode, stop)?;
    let mut stream = init_capture_client(client, channels, true, describe(target, pid))?;
    stream.reopen = exit;
    Ok(stream)
}

/// Fallback liveness check for a stream without an exit handle: about once a
/// second, re-resolves the target and reports a change of the running pid.
#[derive(Default)]
struct LivenessWatch {
    /// Pid being streamed; `None` while no handle-less stream is active.
    pid: Option<u32>,
    last_scan: Option<Instant>,
}

impl LivenessWatch {
    fn arm(&mut self, pid: u32, has_exit_handle: bool) {
        self.pid = (!has_exit_handle).then_some(pid);
        self.last_scan = Some(Instant::now());
    }

    fn changed(&mut self, target: &ProcessTarget) -> bool {
        let Some(pid) = self.pid else {
            return false;
        };
        let now = Instant::now();
        if self
            .last_scan
            .is_some_and(|t| now.duration_since(t) < RESCAN_INTERVAL)
        {
            return false;
        }
        self.last_scan = Some(now);
        find_process(target) != Some(pid)
    }
}

struct IncludeSource {
    target: ProcessTarget,
    stop: Option<OwnedHandle>,
    watch: LivenessWatch,
}

impl StreamSource for IncludeSource {
    fn open(&mut self, channels: u16) -> Result<OpenedStream, AudioError> {
        let Some(pid) = find_process(&self.target) else {
            return Err(AudioError::SourceUnavailable(format!(
                "{} is not running",
                target_label(&self.target)
            )));
        };
        let stream = open_process_stream(
            &self.target,
            pid,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            channels,
            self.stop.as_ref(),
        )?;
        self.watch.arm(pid, stream.reopen.is_some());
        Ok(stream)
    }

    fn needs_reopen(&mut self) -> bool {
        self.watch.changed(&self.target)
    }

    fn set_stop_handle(&mut self, stop: OwnedHandle) {
        self.stop = Some(stop);
    }
}

struct ExcludeSource {
    target: ProcessTarget,
    stop: Option<OwnedHandle>,
    /// Endpoint loopback (follows default-device changes) used while the
    /// target is not running; `Some` exactly while in fallback mode.
    fallback: Option<Box<dyn StreamSource>>,
    last_scan: Option<Instant>,
    watch: LivenessWatch,
}

impl ExcludeSource {
    /// Full mix through endpoint loopback (the target is not running).
    fn open_fallback(&mut self, channels: u16) -> Result<OpenedStream, AudioError> {
        self.watch = LivenessWatch::default();
        self.fallback
            .get_or_insert_with(loopback_source)
            .open(channels)
    }
}

impl StreamSource for ExcludeSource {
    fn open(&mut self, channels: u16) -> Result<OpenedStream, AudioError> {
        self.last_scan = Some(Instant::now());
        let Some(pid) = find_process(&self.target) else {
            return self.open_fallback(channels);
        };
        // Drops the endpoint follower (on this COM thread) if any.
        self.fallback = None;
        match open_process_stream(
            &self.target,
            pid,
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            channels,
            self.stop.as_ref(),
        ) {
            Ok(stream) => {
                self.watch.arm(pid, stream.reopen.is_some());
                Ok(stream)
            }
            // The target exited between the scan and the activation: the
            // remainder is the full mix again. Reporting the track as lost
            // would fail a manual recording over a normal game exit.
            Err(AudioError::SourceUnavailable(_)) => self.open_fallback(channels),
            Err(error) => Err(error),
        }
    }

    fn needs_reopen(&mut self) -> bool {
        if self.fallback.is_none() {
            return self.watch.changed(&self.target);
        }
        let now = Instant::now();
        if self
            .last_scan
            .is_some_and(|t| now.duration_since(t) < FALLBACK_POLL)
        {
            return false;
        }
        self.last_scan = Some(now);
        find_process(&self.target).is_some()
    }

    fn poll_interval(&self) -> Duration {
        if self.fallback.is_some() {
            FALLBACK_POLL
        } else {
            RETRY_POLL
        }
    }

    fn set_stop_handle(&mut self, stop: OwnedHandle) {
        self.stop = Some(stop);
    }
}

/// Captures only the target's process tree; survives restarts of
/// `ProcessTarget::Executable` targets.
pub(crate) fn include_source(target: ProcessTarget) -> Box<dyn StreamSource> {
    Box::new(IncludeSource {
        target,
        stop: None,
        watch: LivenessWatch::default(),
    })
}

/// Captures everything except the target's process tree. While the target is
/// not running this is exactly the full mix, so it uses endpoint loopback and
/// switches to exclude mode once the target appears.
///
/// Known window: the switch happens within about 250 ms (poll interval) plus
/// the activation time after the target starts; if the target's own track
/// opens first, its audio is in both tracks (and the mix) for that moment.
pub(crate) fn exclude_source(target: ProcessTarget) -> Box<dyn StreamSource> {
    Box::new(ExcludeSource {
        target,
        stop: None,
        fallback: None,
        last_scan: None,
        watch: LivenessWatch::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    #[test]
    fn capabilities_reports_build() {
        let caps = capabilities();
        println!("build {} -> {caps:?}", os_build());
        assert!(os_build() > 0);
    }

    #[test]
    fn find_process_cases() {
        assert!(find_process(&ProcessTarget::Executable("explorer.exe".into())).is_some());
        assert!(find_process(&ProcessTarget::Executable("EXPLORER.EXE".into())).is_some());
        assert!(find_process(&ProcessTarget::Executable("no_such_thing_xyz.exe".into())).is_none());
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(find_process(&ProcessTarget::Pid(pid)).is_none());
        assert!(find_process(&ProcessTarget::Pid(std::process::id())).is_some());
    }

    fn write_sine_wav(path: &std::path::Path) {
        let rate = 48_000u32;
        let frames = rate * 8;
        let mut data = Vec::with_capacity(frames as usize * 4);
        for n in 0..frames {
            let s = (f64::from(n) * 1000.0 * std::f64::consts::TAU / f64::from(rate)).sin();
            let v = (s * 0.5 * 32767.0) as i16;
            data.extend_from_slice(&v.to_le_bytes());
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        std::fs::write(path, wav).unwrap();
    }

    /// Drains all pending packets and returns their peak.
    fn drain_peak(stream: &OpenedStream) -> f32 {
        let mut peak = 0f32;
        loop {
            // SAFETY: standard capture-client packet loop.
            unsafe {
                let n = stream.capture.GetNextPacketSize().unwrap();
                if n == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                stream
                    .capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .unwrap();
                if flags & 2 == 0 && !data.is_null() {
                    let s = std::slice::from_raw_parts(
                        data.cast::<f32>(),
                        frames as usize * usize::from(stream.channels),
                    );
                    peak = s.iter().fold(peak, |m, v| m.max(v.abs()));
                }
                stream.capture.ReleaseBuffer(frames).unwrap();
            }
        }
        peak
    }

    #[test]
    #[ignore = "needs audio hardware; plays a test tone through the speakers"]
    fn include_vs_exclude_tone() {
        // SAFETY: test thread joins the MTA.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let path = std::env::temp_dir().join("clipper_sine_1k.wav");
        write_sine_wav(&path);
        let mut child = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-c",
                &format!(
                    "(New-Object Media.SoundPlayer '{}').PlaySync()",
                    path.display()
                ),
            ])
            .spawn()
            .unwrap();
        let target = ProcessTarget::Pid(child.id());
        let mut inc = include_source(target.clone());
        let mut exc = exclude_source(target);
        let a = inc.open(2);
        let b = exc.open(2);
        let (a, b) = match (a, b) {
            (Ok(a), Ok(b)) => (a, b),
            (a, b) => {
                let _ = child.kill();
                panic!("open failed: {:?} / {:?}", a.err(), b.err());
            }
        };
        // SAFETY: initialized clients with event handles set.
        unsafe {
            a.client.Start().unwrap();
            b.client.Start().unwrap();
        }
        std::thread::sleep(Duration::from_millis(1000));
        let (mut pi, mut pe) = (0f32, 0f32);
        let end = Instant::now() + Duration::from_millis(1000);
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            pi = pi.max(drain_peak(&a));
            pe = pe.max(drain_peak(&b));
        }
        let _ = child.kill();
        let _ = child.wait();
        println!("include peak {pi} exclude peak {pe}");
        assert!(pi > 0.05, "include peak too low");
        assert!(pi > 4.0 * pe, "exclude contains the tone");
    }
}
