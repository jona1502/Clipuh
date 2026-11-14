//! Windows Graphics Capture backend.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::Arc;

use clipper_core::recording::{CaptureSource, SourceKind};
use clipper_platform::windows::{D3DContext, WinError};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{E_POINTER, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOTOWNER, GWL_EXSTYLE, GetAncestor, GetWindowLongW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, IInspectable, Interface};

const MAX_WINDOWS: usize = 256;
const BUFFER_COUNT: i32 = 2;
const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const MONITORINFOF_PRIMARY: u32 = 1;
const TICKS_PER_SECOND: i64 = 10_000_000;
/// Jitter margin subtracted from the minimum update interval (0.5 ms), so a
/// source at exactly twice the target rate is not halved by timing noise.
const MIN_INTERVAL_MARGIN_TICKS: i64 = 5_000;

/// Minimum time between two WGC frames for a capture paced to `fps`, in
/// 100 ns ticks: half a frame interval minus a small margin.
///
/// Without it WGC uses its built-in default (about 60 updates per second on
/// current Windows 11 builds), which starves 120 fps captures and leaves a
/// 60 fps pacer with no spare frame when one arrives late, so the slot is
/// filled with a repeated frame. Half an interval lets at most ~2x the target
/// rate through: every slot has a candidate, while a faster source (a 180 Hz
/// or 240 Hz game) is still not copied at its full rate.
pub fn min_update_interval_ticks(fps: u32) -> i64 {
    let interval = TICKS_PER_SECOND / i64::from(fps.max(1));
    (interval / 2 - MIN_INTERVAL_MARGIN_TICKS).max(interval / 4)
}

#[derive(Debug)]
pub enum CaptureError {
    /// WGC is not available on this OS build.
    Unsupported,
    /// The monitor or window no longer exists or cannot be captured.
    SourceUnavailable(String),
    Windows(WinError),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("screen capture is not supported on this system"),
            Self::SourceUnavailable(name) => write!(f, "capture source unavailable: {name}"),
            Self::Windows(error) => write!(f, "capture failed: {error}"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<WinError> for CaptureError {
    fn from(error: WinError) -> Self {
        Self::Windows(error)
    }
}

/// Notifications raised on a WGC worker thread. Handlers must not block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureSignal {
    FrameArrived,
    SourceClosed,
}

pub type SignalHandler = Box<dyn Fn(CaptureSignal) + Send + Sync + 'static>;

fn hwnd_from_id(id: u64) -> HWND {
    HWND(id as usize as *mut c_void)
}

fn hmonitor_from_id(id: u64) -> HMONITOR {
    HMONITOR(id as usize as *mut c_void)
}

fn id_of_hwnd(hwnd: HWND) -> u64 {
    hwnd.0 as usize as u64
}

fn id_of_monitor(monitor: HMONITOR) -> u64 {
    monitor.0 as usize as u64
}

struct MonitorEntry {
    handle: HMONITOR,
    primary: bool,
    width: u32,
    height: u32,
}

unsafe extern "system" fn monitor_proc(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the address of a live `Vec<MonitorEntry>` passed by
    // `enum_monitors`, used only for the duration of the enumeration.
    let out = unsafe { &mut *(data.0 as *mut Vec<MonitorEntry>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a properly sized MONITORINFOEXW; the pointer cast to
    // MONITORINFO is the documented usage (cbSize selects the extended form).
    let ok = unsafe {
        GetMonitorInfoW(
            monitor,
            &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
        )
    };
    if ok.as_bool() {
        let r = info.monitorInfo.rcMonitor;
        out.push(MonitorEntry {
            handle: monitor,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            width: (r.right - r.left).max(0) as u32,
            height: (r.bottom - r.top).max(0) as u32,
        });
    }
    TRUE
}

fn enum_monitors() -> Vec<MonitorEntry> {
    let mut out: Vec<MonitorEntry> = Vec::new();
    // SAFETY: the callback only dereferences the LPARAM as the Vec above,
    // which outlives the synchronous enumeration.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_proc),
            LPARAM(&mut out as *mut Vec<MonitorEntry> as isize),
        )
    };
    out.sort_by_key(|m| !m.primary);
    out
}

struct WindowEnum {
    out: Vec<CaptureSource>,
    own_pid: u32,
}

unsafe extern "system" fn window_proc(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the address of a live `WindowEnum` passed by
    // `enum_windows` for the duration of the enumeration.
    let state = unsafe { &mut *(data.0 as *mut WindowEnum) };
    if state.out.len() >= MAX_WINDOWS {
        return BOOL(0);
    }
    if let Some(source) = describe_window(hwnd, state.own_pid) {
        state.out.push(source);
    }
    TRUE
}

fn describe_window(hwnd: HWND, own_pid: u32) -> Option<CaptureSource> {
    // SAFETY: all calls take a window handle from EnumWindows and plain
    // out-pointers to live locals; stale handles just make calls fail.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        if GetAncestor(hwnd, GA_ROOTOWNER) != hwnd {
            return None;
        }
        let mut cloaked: u32 = 0;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == own_pid {
            return None;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut buf);
        if copied <= 0 {
            return None;
        }
        let name = String::from_utf16_lossy(&buf[..copied as usize]);

        let mut rect = RECT::default();
        let dwm_ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok();
        if !dwm_ok && GetWindowRect(hwnd, &mut rect).is_err() {
            return None;
        }
        let width = (rect.right - rect.left).max(0) as u32;
        let height = (rect.bottom - rect.top).max(0) as u32;
        if width == 0 || height == 0 {
            return None;
        }
        Some(CaptureSource {
            kind: SourceKind::Window,
            id: id_of_hwnd(hwnd),
            name,
            width,
            height,
        })
    }
}

fn enum_windows() -> Vec<CaptureSource> {
    let mut state = WindowEnum {
        out: Vec::new(),
        // SAFETY: no preconditions.
        own_pid: unsafe { windows::Win32::System::Threading::GetCurrentProcessId() },
    };
    // SAFETY: the callback dereferences the LPARAM as `WindowEnum`, which
    // outlives the synchronous enumeration. Early stop (cap) makes
    // EnumWindows return an error, which is expected and ignored.
    let _ = unsafe {
        EnumWindows(
            Some(window_proc),
            LPARAM(&mut state as *mut WindowEnum as isize),
        )
    };
    state.out
}

fn ensure_supported() -> Result<(), CaptureError> {
    match GraphicsCaptureSession::IsSupported() {
        Ok(true) => Ok(()),
        Ok(false) => Err(CaptureError::Unsupported),
        Err(error) => Err(CaptureError::Windows(error)),
    }
}

/// Lists capturable monitors and top-level windows.
pub fn list_sources() -> Result<Vec<CaptureSource>, CaptureError> {
    ensure_supported()?;
    let mut sources: Vec<CaptureSource> = enum_monitors()
        .into_iter()
        .enumerate()
        .map(|(index, m)| CaptureSource {
            kind: SourceKind::Monitor,
            id: id_of_monitor(m.handle),
            name: format!("Display {} ({}x{})", index + 1, m.width, m.height),
            width: m.width,
            height: m.height,
        })
        .collect();
    sources.extend(enum_windows());
    Ok(sources)
}

fn create_item(source: &CaptureSource) -> Result<GraphicsCaptureItem, CaptureError> {
    let unavailable = || CaptureError::SourceUnavailable(source.name.clone());
    match source.kind {
        SourceKind::Monitor => {
            if !enum_monitors()
                .iter()
                .any(|m| id_of_monitor(m.handle) == source.id)
            {
                return Err(unavailable());
            }
        }
        SourceKind::Window => {
            let hwnd = hwnd_from_id(source.id);
            // SAFETY: IsWindow accepts any handle value.
            if source.id == 0 || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                return Err(unavailable());
            }
        }
    }
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    // SAFETY: handle was validated just above; a race with destruction
    // surfaces as an error from the call.
    let item = match source.kind {
        SourceKind::Monitor => unsafe {
            interop.CreateForMonitor::<GraphicsCaptureItem>(hmonitor_from_id(source.id))
        },
        SourceKind::Window => unsafe {
            interop.CreateForWindow::<GraphicsCaptureItem>(hwnd_from_id(source.id))
        },
    };
    item.map_err(|_| unavailable())
}

/// An active capture of one source. Dropping it stops capture and releases
/// the frame pool and session.
pub struct CaptureSession {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    device: IDirect3DDevice,
    frame_token: i64,
    closed_token: i64,
    size: (u32, u32),
    /// Every frame taken from the pool, including superseded ones.
    delivered: u64,
}

impl CaptureSession {
    pub fn start(
        d3d: &D3DContext,
        source: &CaptureSource,
        signal: SignalHandler,
    ) -> Result<Self, CaptureError> {
        ensure_supported()?;
        let item = create_item(source)?;
        let item_size = item.Size()?;
        let device = d3d.winrt_device().clone();
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &device,
            PIXEL_FORMAT,
            BUFFER_COUNT,
            item_size,
        )?;

        let signal: Arc<dyn Fn(CaptureSignal) + Send + Sync> = Arc::from(signal);
        let on_frame = Arc::clone(&signal);
        let frame_token = pool.FrameArrived(&TypedEventHandler::<
            Direct3D11CaptureFramePool,
            IInspectable,
        >::new(move |_, _| {
            on_frame(CaptureSignal::FrameArrived);
            Ok(())
        }))?;
        let on_closed = Arc::clone(&signal);
        let closed_token = match item.Closed(
            &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                on_closed(CaptureSignal::SourceClosed);
                Ok(())
            }),
        ) {
            Ok(token) => token,
            Err(error) => {
                let _ = pool.RemoveFrameArrived(frame_token);
                let _ = pool.Close();
                return Err(error.into());
            }
        };

        let cleanup = |pool: &Direct3D11CaptureFramePool| {
            let _ = pool.RemoveFrameArrived(frame_token);
            let _ = item.RemoveClosed(closed_token);
            let _ = pool.Close();
        };
        let session = match pool.CreateCaptureSession(&item) {
            Ok(session) => session,
            Err(error) => {
                cleanup(&pool);
                return Err(error.into());
            }
        };
        // Optional features: older Windows builds reject these.
        let _ = session.SetIsCursorCaptureEnabled(true);
        let _ = session.SetIsBorderRequired(false);
        if let Err(error) = session.StartCapture() {
            let _ = session.Close();
            cleanup(&pool);
            return Err(error.into());
        }

        Ok(Self {
            item,
            pool,
            session,
            device,
            frame_token,
            closed_token,
            size: (
                item_size.Width.max(0) as u32,
                item_size.Height.max(0) as u32,
            ),
            delivered: 0,
        })
    }

    /// Current source content size in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Paces WGC to the target frame rate (see [`min_update_interval_ticks`]).
    /// Returns `false` on Windows builds without
    /// `GraphicsCaptureSession.MinUpdateInterval`; WGC then keeps its default.
    pub fn set_target_fps(&self, fps: u32) -> bool {
        self.session
            .SetMinUpdateInterval(TimeSpan {
                Duration: min_update_interval_ticks(fps),
            })
            .is_ok()
    }

    /// Frames WGC delivered so far, including the ones superseded by a newer
    /// frame before they were used. Diagnostics only.
    pub fn frames_delivered(&self) -> u64 {
        self.delivered
    }

    fn poll(&mut self) -> Result<Option<Direct3D11CaptureFrame>, CaptureError> {
        match self.pool.TryGetNextFrame() {
            Ok(frame) => {
                self.delivered += 1;
                Ok(Some(frame))
            }
            // WinRT yields a null/empty result when no frame is pending.
            Err(error) if error.code().is_ok() || error.code() == E_POINTER => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Returns the newest pending frame, if any. Handles source resize.
    pub fn try_next_frame(&mut self) -> Result<Option<CapturedFrame>, CaptureError> {
        let Some(mut frame) = self.poll()? else {
            return Ok(None);
        };
        // Keep only the newest frame so pool slots are recycled promptly.
        while let Some(newer) = self.poll()? {
            let _ = frame.Close();
            frame = newer;
        }

        let content = frame.ContentSize()?;
        let content_size = (content.Width.max(0) as u32, content.Height.max(0) as u32);
        if content_size != self.size && content_size.0 > 0 && content_size.1 > 0 {
            self.pool.Recreate(
                &self.device,
                PIXEL_FORMAT,
                BUFFER_COUNT,
                SizeInt32 {
                    Width: content.Width,
                    Height: content.Height,
                },
            )?;
            self.size = content_size;
        }

        let timestamp = frame.SystemRelativeTime()?.Duration;
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        // SAFETY: the surface is backed by a D3D11 texture on our device;
        // GetInterface returns an owned reference to it.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
        Ok(Some(CapturedFrame {
            frame,
            texture,
            timestamp,
            content_size,
        }))
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.frame_token);
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// A captured frame. The texture belongs to the WGC frame pool and is
/// returned to it when this value is dropped, so consumers must copy it on
/// the GPU before dropping.
pub struct CapturedFrame {
    frame: Direct3D11CaptureFrame,
    texture: ID3D11Texture2D,
    timestamp: i64,
    content_size: (u32, u32),
}

impl CapturedFrame {
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    /// QPC-based system relative time in 100 ns ticks.
    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    /// Valid content size inside the texture.
    pub fn content_size(&self) -> (u32, u32) {
        self.content_size
    }
}

impl Drop for CapturedFrame {
    fn drop(&mut self) {
        let _ = self.frame.Close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn min_update_interval_allows_about_twice_the_target_rate() {
        // 60 fps: 8.33 ms - 0.5 ms margin.
        assert_eq!(min_update_interval_ticks(60), 83_333 - 5_000);
        // 120 fps: 4.17 ms - 0.5 ms margin.
        assert_eq!(min_update_interval_ticks(120), 41_666 - 5_000);
        for fps in [1, 24, 30, 60, 120, 144, 240, 1000] {
            let interval = TICKS_PER_SECOND / i64::from(fps);
            let min = min_update_interval_ticks(fps);
            // Never slower than the target, never below a quarter interval.
            assert!(min < interval / 2 + 1, "fps {fps}");
            assert!(min >= interval / 4, "fps {fps}");
        }
        // 0 is treated as 1 fps instead of dividing by zero.
        assert_eq!(min_update_interval_ticks(0), min_update_interval_ticks(1));
    }

    #[test]
    #[ignore = "needs a monitor"]
    fn lists_at_least_one_monitor() {
        let sources = list_sources().expect("list");
        let monitor = sources
            .iter()
            .find(|s| s.kind == SourceKind::Monitor)
            .expect("monitor");
        assert!(monitor.width > 0 && monitor.height > 0);
    }

    #[test]
    #[ignore = "needs a D3D11 GPU device"]
    fn invalid_window_is_unavailable() {
        let d3d = D3DContext::new().expect("d3d");
        let source = CaptureSource {
            kind: SourceKind::Window,
            id: 0x7fff_fff0,
            name: "ghost".into(),
            width: 10,
            height: 10,
        };
        let result = CaptureSession::start(&d3d, &source, Box::new(|_| {}));
        assert!(matches!(result, Err(CaptureError::SourceUnavailable(_))));
    }

    #[test]
    #[ignore = "needs a GPU and a monitor"]
    fn monitor_capture_delivers_frame() {
        let d3d = D3DContext::new().expect("d3d");
        let source = list_sources()
            .expect("list")
            .into_iter()
            .find(|s| s.kind == SourceKind::Monitor)
            .expect("monitor");
        let signals = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&signals);
        let mut session = CaptureSession::start(
            &d3d,
            &source,
            Box::new(move |s| {
                if s == CaptureSignal::FrameArrived {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            }),
        )
        .expect("start");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut got = None;
        while Instant::now() < deadline {
            if let Some(frame) = session.try_next_frame().expect("frame") {
                got = Some((frame.content_size(), frame.timestamp()));
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let (size, ts) = got.expect("no frame within 2s");
        assert!(size.0 > 0 && size.1 > 0 && ts > 0);
        assert!(signals.load(Ordering::Relaxed) > 0);
    }
}
