//! Video playback for the clip editor: `IMFMediaEngine` in frame-server mode
//! (hardware decode through a DXGI device manager, audio rendered by the
//! engine itself). Frames are copied to CPU BGRA buffers for the UI.
//! Editor-supplied audio tracks replace engine audio with the bounded WASAPI
//! mixer; its scheduling follows this engine's source time.
//!
//! One dedicated thread owns every COM object; the engine's notify callback
//! (an MF worker thread) only forwards events into the thread's mailbox.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::audio_preview::AudioPreview;
use clipper_editor::AudioTrack;

use clipper_encoder::backends::media_foundation::MediaFoundation;
use clipper_platform::windows::{ComApartment, D3DContext};
use windows::Win32::Foundation::{E_POINTER, RECT, S_OK};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{BSTR, HRESULT, Interface, implement};

/// One decoded preview frame: BGRA8, alpha 255, tightly packed rows.
#[derive(Debug)]
pub struct PlayerFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub position: Duration,
}

#[derive(Clone, Debug)]
pub enum PlayerEvent {
    /// Metadata ready. width/height are the source video size.
    Loaded {
        duration: Duration,
        width: u32,
        height: u32,
        has_audio: bool,
    },
    Frame(Arc<PlayerFrame>),
    /// Playback position, ~every 100 ms while playing and after each seek.
    Position(Duration),
    Playing(bool),
    Ended,
    Error(String),
    /// Nonfatal output-device or multitrack decode failure; video keeps playing.
    AudioError(String),
}

enum Cmd {
    Play,
    Pause,
    Seek(Duration),
    AudioTracks,
    Shutdown,
}

enum Msg {
    Cmd(Cmd),
    Engine(u32, u32),
}

pub struct Player {
    tx: mpsc::Sender<Msg>,
    events: async_channel::Sender<PlayerEvent>,
    thread: Option<JoinHandle<()>>,
    audio_tracks: Arc<Mutex<Option<Vec<AudioTrack>>>>,
}

impl Player {
    /// Spawns the player thread and starts loading `path` (paused).
    /// Failures arrive as `PlayerEvent::Error`.
    pub fn open(path: &Path, max_width: u32) -> (Player, async_channel::Receiver<PlayerEvent>) {
        let (etx, erx) = async_channel::bounded(16);
        let (tx, rx) = mpsc::channel();
        let path = path.to_path_buf();
        let audio_tracks = Arc::new(Mutex::new(None));
        let thread_tracks = audio_tracks.clone();
        let (thread_tx, thread_etx) = (tx.clone(), etx.clone());
        let thread = std::thread::Builder::new()
            .name("clip-player".into())
            .spawn(move || {
                if let Err(Exit::Fail(message)) =
                    run(&path, max_width, &rx, thread_tx, &thread_etx, thread_tracks)
                {
                    let _ = thread_etx.send_blocking(PlayerEvent::Error(message));
                }
            })
            .ok();
        if thread.is_none() {
            let _ = etx.try_send(PlayerEvent::Error("The player could not start.".into()));
        }
        (
            Player {
                tx,
                events: etx,
                thread,
                audio_tracks,
            },
            erx,
        )
    }

    pub fn play(&self) {
        let _ = self.tx.send(Msg::Cmd(Cmd::Play));
    }

    /// Replaces the source-domain audio mix without creating a video session.
    pub fn set_audio_tracks(&self, tracks: Vec<AudioTrack>) {
        let notify = {
            let mut pending = self
                .audio_tracks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.replace(tracks).is_none()
        };
        // Audio gestures retain one latest snapshot and at most one wake in
        // the existing player mailbox, regardless of UI input frequency.
        if notify {
            let _ = self.tx.send(Msg::Cmd(Cmd::AudioTracks));
        }
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Msg::Cmd(Cmd::Pause));
    }

    /// Clamped to [0, duration]; a frame for the target is delivered even while paused.
    pub fn seek(&self, position: Duration) {
        let _ = self.tx.send(Msg::Cmd(Cmd::Seek(position)));
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Cmd(Cmd::Shutdown));
        // Unblocks a thread stuck in send_blocking on a full, unread channel.
        self.events.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[implement(IMFMediaEngineNotify)]
struct Notify(mpsc::Sender<Msg>);

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, _param1: usize, param2: u32) -> windows::core::Result<()> {
        let _ = self.0.send(Msg::Engine(event, param2));
        Ok(())
    }
}

enum Exit {
    /// Receiver gone or shutdown requested: leave quietly.
    Closed,
    Fail(String),
}

impl From<windows::core::Error> for Exit {
    fn from(error: windows::core::Error) -> Self {
        Exit::Fail(format!("This file can't be played: {}", error.message()))
    }
}

/// Preview size: aspect kept, never upscaled, even dimensions, at least 2.
fn preview_size(src_w: u32, src_h: u32, max_w: u32) -> (u32, u32) {
    let even = |v: u32| (v & !1).max(2);
    let w = src_w.min(max_w).max(2);
    let h = (u64::from(src_h) * u64::from(w) / u64::from(src_w.max(1))) as u32;
    (even(w), even(h))
}

struct Surfaces {
    target: ID3D11Texture2D,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
}

fn texture(
    d3d: &D3DContext,
    width: u32,
    height: u32,
    staging: bool,
) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: if staging {
            D3D11_USAGE_STAGING
        } else {
            D3D11_USAGE_DEFAULT
        },
        BindFlags: if staging {
            0
        } else {
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32
        },
        CPUAccessFlags: if staging {
            D3D11_CPU_ACCESS_READ.0 as u32
        } else {
            0
        },
        MiscFlags: 0,
    };
    let mut out = None;
    // SAFETY: desc and out-pointer are live locals; no initial data.
    unsafe { d3d.device().CreateTexture2D(&desc, None, Some(&mut out))? };
    out.ok_or_else(|| E_POINTER.into())
}

/// Polls the engine for a new frame; `None` when none is ready. `force`
/// transfers the current frame anyway: while paused the engine reports no new
/// frame after a seek, although the current frame is already at the target.
fn grab(
    engine: &IMFMediaEngine,
    d3d: &D3DContext,
    s: &Surfaces,
    force: bool,
) -> windows::core::Result<Option<Arc<PlayerFrame>>> {
    let mut pts = 0i64;
    // SAFETY: valid engine; the high-level wrapper cannot tell S_OK from
    // S_FALSE (no frame), so call the vtable entry directly.
    let hr = unsafe {
        (Interface::vtable(engine).OnVideoStreamTick)(Interface::as_raw(engine), &mut pts)
    };
    if hr.is_err() {
        return Err(hr.into());
    }
    if hr != S_OK {
        if !force {
            return Ok(None);
        }
        // SAFETY: valid engine.
        pts = (unsafe { engine.GetCurrentTime() }.max(0.0) * 1e7) as i64;
    }
    let dst = RECT {
        left: 0,
        top: 0,
        right: s.width as i32,
        bottom: s.height as i32,
    };
    let black = MFARGB {
        rgbBlue: 0,
        rgbGreen: 0,
        rgbRed: 0,
        rgbAlpha: 255,
    };
    let context = d3d.context();
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    let row = s.width as usize * 4;
    let mut bgra = vec![0u8; row * s.height as usize];
    // SAFETY: target is a live render-target texture of this device; rect
    // and colour point at live locals.
    unsafe { engine.TransferVideoFrame(&s.target, None, &dst, Some(&black))? };
    // SAFETY: both textures belong to this device and share a description.
    unsafe { context.CopyResource(&s.staging, &s.target) };
    // SAFETY: staging has CPU read access; each row read stays within the
    // mapped RowPitch * height bytes and the mapping is released below.
    unsafe {
        context.Map(&s.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let pitch = mapped.RowPitch as usize;
        let src = mapped.pData as *const u8;
        for (y, out) in bgra.chunks_exact_mut(row).enumerate() {
            out.copy_from_slice(std::slice::from_raw_parts(src.add(y * pitch), row));
        }
        context.Unmap(&s.staging, 0);
    }
    Ok(Some(Arc::new(PlayerFrame {
        width: s.width,
        height: s.height,
        bgra,
        position: Duration::from_nanos(pts.max(0) as u64 * 100),
    })))
}

fn engine_error_message(engine: &IMFMediaEngine, event_hr: u32) -> String {
    // SAFETY: valid engine; GetError returns an owned IMFMediaError whose
    // extended code arrives as the Err side of the binding.
    let (code, extended) = unsafe {
        match engine.GetError() {
            Ok(e) => (e.GetErrorCode(), e.GetExtendedErrorCode().err()),
            Err(_) => (0, None),
        }
    };
    let hr = (event_hr != 0)
        .then_some(HRESULT(event_hr as i32))
        .or_else(|| extended.map(|e| e.code()));
    match (code, hr) {
        (_, Some(hr)) => format!(
            "This file can't be played: {}",
            windows::core::Error::from(hr).message()
        ),
        (4, _) => "This file can't be played: the format is not supported.".into(),
        (3, _) => "This file can't be played: it could not be decoded.".into(),
        _ => "This file can't be played.".into(),
    }
}

/// Calls `Shutdown` on the engine when the player thread leaves, on every path.
struct EngineGuard(IMFMediaEngine);

impl Drop for EngineGuard {
    fn drop(&mut self) {
        // SAFETY: valid engine, used on the thread that created it.
        let _ = unsafe { self.0.Shutdown() };
    }
}

fn run(
    path: &Path,
    max_width: u32,
    rx: &mpsc::Receiver<Msg>,
    self_tx: mpsc::Sender<Msg>,
    events: &async_channel::Sender<PlayerEvent>,
    audio_tracks: Arc<Mutex<Option<Vec<AudioTrack>>>>,
) -> Result<(), Exit> {
    if !path.is_file() {
        return Err(Exit::Fail("The clip file could not be found.".into()));
    }
    let send = |event: PlayerEvent| events.send_blocking(event).map_err(|_| Exit::Closed);

    // Locals drop in reverse: engine, then MF shutdown, then COM uninit.
    let _com = ComApartment::init_multithreaded()?;
    let _mf = MediaFoundation::start().map_err(|e| Exit::Fail(e.to_string()))?;
    let d3d = D3DContext::new()?;

    let mut token = 0u32;
    let mut manager = None;
    // SAFETY: out-pointers reference live locals.
    unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager)? };
    let manager: IMFDXGIDeviceManager = manager.ok_or(windows::core::Error::from(E_POINTER))?;
    // SAFETY: the device is a live, multithread-protected D3D11 device.
    unsafe { manager.ResetDevice(d3d.device(), token)? };

    let notify: IMFMediaEngineNotify = Notify(self_tx).into();
    let mut attrs = None;
    // SAFETY: out-pointer references a live local.
    unsafe { MFCreateAttributes(&mut attrs, 3)? };
    let attrs: IMFAttributes = attrs.ok_or(windows::core::Error::from(E_POINTER))?;
    // SAFETY: attrs is valid; the attribute store keeps the objects alive.
    unsafe {
        attrs.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
        attrs.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager)?;
        attrs.SetUINT32(
            &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
            DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
        )?;
    }
    // SAFETY: standard in-proc COM activation of the engine class factory.
    let factory: IMFMediaEngineClassFactory =
        unsafe { CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)? };
    // SAFETY: attrs holds the callback and DXGI manager; flags 0 = normal mode.
    let engine = EngineGuard(unsafe { factory.CreateInstance(0, &attrs)? });
    let engine = &engine.0;

    // SAFETY: valid engine; the BSTR outlives the call.
    unsafe {
        engine.SetAutoPlay(false)?;
        engine.SetPreload(MF_MEDIA_ENGINE_PRELOAD_AUTOMATIC)?;
        engine.SetSource(&BSTR::from(path.to_string_lossy().as_ref()))?;
    }

    let mut surfaces: Option<Surfaces> = None;
    let mut duration = Duration::ZERO;
    let (mut playing, mut ended) = (false, false);
    let mut settle: Option<Instant> = None;
    // Paused and the engine reached a new position: show its frame now.
    let mut force_frame = false;
    let mut last_position = Instant::now();
    let mut audio: Option<AudioPreview> = None;
    let mut audio_muted = false;
    let mut audio_seeking = false;

    loop {
        let msg = if playing || settle.is_some() {
            let wait = Duration::from_millis(if playing { 8 } else { 10 });
            match rx.recv_timeout(wait) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        } else {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => return Ok(()),
            }
        };

        match msg {
            Some(Msg::Cmd(Cmd::Shutdown)) => return Ok(()),
            Some(Msg::Cmd(Cmd::AudioTracks)) => {
                let Some(tracks) = audio_tracks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                else {
                    continue;
                };
                if audio.is_none() {
                    match AudioPreview::open(path, events.clone()) {
                        Ok(preview) => audio = Some(preview),
                        Err(error) => {
                            // SAFETY: valid engine; a failed custom mix must not
                            // leak the engine's unedited default audio stream.
                            unsafe {
                                engine.SetMuted(true)?;
                            }
                            audio_muted = true;
                            let _ = events.try_send(PlayerEvent::AudioError(error));
                        }
                    }
                }
                if let Some(audio) = &audio {
                    audio.set_tracks(tracks);
                    // SAFETY: engine time is the only authoritative clock.
                    let now = unsafe { engine.GetCurrentTime() };
                    audio.synchronize(
                        Duration::from_secs_f64(now.max(0.0)),
                        playing && !audio_seeking,
                        false,
                    );
                }
            }
            Some(Msg::Cmd(Cmd::Play)) if surfaces.is_some() => {
                // SAFETY: valid engine.
                unsafe {
                    if ended {
                        engine.SetCurrentTime(0.0)?;
                        audio_seeking = true;
                        ended = false;
                    }
                    engine.Play()?;
                }
                playing = true;
                if let Some(audio) = &audio {
                    // SAFETY: valid engine; uses the post-play/restart time.
                    let now = unsafe { engine.GetCurrentTime() };
                    audio.synchronize(Duration::from_secs_f64(now.max(0.0)), !audio_seeking, true);
                }
                send(PlayerEvent::Playing(true))?;
            }
            Some(Msg::Cmd(Cmd::Pause)) => {
                // SAFETY: valid engine.
                unsafe { engine.Pause()? };
                if let Some(audio) = &audio {
                    // SAFETY: valid engine.
                    let now = unsafe { engine.GetCurrentTime() };
                    audio.synchronize(Duration::from_secs_f64(now.max(0.0)), false, true);
                }
                if playing {
                    playing = false;
                    send(PlayerEvent::Playing(false))?;
                }
            }
            // shortcut: play/seek before metadata loads are ignored; the UI waits for Loaded.
            Some(Msg::Cmd(Cmd::Seek(position))) if surfaces.is_some() => {
                ended = false;
                audio_seeking = true;
                // SAFETY: valid engine.
                unsafe { engine.SetCurrentTime(position.min(duration).as_secs_f64())? };
                if let Some(audio) = &audio {
                    audio.synchronize(position.min(duration), false, true);
                }
            }
            Some(Msg::Engine(event, extended)) => match event {
                e if e == MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA.0 as u32 => {
                    let (mut w, mut h) = (0u32, 0u32);
                    // SAFETY: valid engine; out-pointers reference live locals.
                    let (seconds, has_audio) = unsafe {
                        engine.GetNativeVideoSize(Some(&mut w), Some(&mut h))?;
                        (engine.GetDuration(), engine.HasAudio().as_bool())
                    };
                    if !seconds.is_finite() || seconds < 0.0 || w == 0 || h == 0 {
                        return Err(Exit::Fail(
                            "This file can't be played: it has no usable video.".into(),
                        ));
                    }
                    duration = Duration::from_secs_f64(seconds);
                    let (pw, ph) = preview_size(w, h, max_width);
                    surfaces = Some(Surfaces {
                        target: texture(&d3d, pw, ph, false)?,
                        staging: texture(&d3d, pw, ph, true)?,
                        width: pw,
                        height: ph,
                    });
                    send(PlayerEvent::Loaded {
                        duration,
                        width: w,
                        height: h,
                        has_audio,
                    })?;
                }
                e if e == MF_MEDIA_ENGINE_EVENT_LOADEDDATA.0 as u32 => {
                    settle = Some(Instant::now() + Duration::from_millis(300));
                    force_frame = !playing;
                }
                // In frame-server mode a paused seek only completes while
                // OnVideoStreamTick is being polled.
                e if e == MF_MEDIA_ENGINE_EVENT_SEEKING.0 as u32 => {
                    audio_seeking = true;
                    settle = Some(Instant::now() + Duration::from_secs(2));
                }
                e if e == MF_MEDIA_ENGINE_EVENT_SEEKED.0 as u32 => {
                    audio_seeking = false;
                    settle = Some(Instant::now() + Duration::from_millis(300));
                    force_frame = !playing;
                    // SAFETY: valid engine.
                    let now = unsafe { engine.GetCurrentTime() };
                    if let Some(audio) = &audio {
                        audio.synchronize(Duration::from_secs_f64(now.max(0.0)), playing, true);
                    }
                    send(PlayerEvent::Position(Duration::from_secs_f64(now.max(0.0))))?;
                }
                e if e == MF_MEDIA_ENGINE_EVENT_ENDED.0 as u32 => {
                    playing = false;
                    ended = true;
                    if let Some(audio) = &audio {
                        audio.synchronize(duration, false, true);
                    }
                    send(PlayerEvent::Playing(false))?;
                    send(PlayerEvent::Position(duration))?;
                    send(PlayerEvent::Ended)?;
                }
                e if e == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32 => {
                    return Err(Exit::Fail(engine_error_message(engine, extended)));
                }
                _ => {}
            },
            _ => {}
        }

        if let Some(audio) = &audio {
            if audio.owns_audio() {
                if !audio_muted {
                    // SAFETY: custom output is initialized (or explicitly failed).
                    unsafe {
                        engine.SetMuted(true)?;
                    }
                    audio_muted = true;
                }
                audio.activate();
            }
            // The audio worker consumes a bounded lookahead of this exact
            // engine position. It never advances a second playback clock.
            // SAFETY: valid engine on its owning thread.
            let now = unsafe { engine.GetCurrentTime() };
            let seeking = audio_seeking || unsafe { engine.IsSeeking().as_bool() };
            audio.synchronize(
                Duration::from_secs_f64(now.max(0.0)),
                playing && !seeking,
                false,
            );
        }
        if let (Some(s), true) = (&surfaces, playing || settle.is_some()) {
            if let Some(frame) = grab(engine, &d3d, s, std::mem::take(&mut force_frame))? {
                settle = None;
                if !playing {
                    // The only frame for this paused position: never drop it.
                    send(PlayerEvent::Frame(frame))?;
                } else if events.len() < 4 {
                    let _ = events.try_send(PlayerEvent::Frame(frame));
                }
            } else if settle.is_some_and(|d| Instant::now() >= d) {
                settle = None;
            }
        }
        if playing && last_position.elapsed() >= Duration::from_millis(100) {
            last_position = Instant::now();
            // SAFETY: valid engine.
            let now = unsafe { engine.GetCurrentTime() };
            // Lossy on purpose: a stalled UI must not block the player.
            let _ = events.try_send(PlayerEvent::Position(Duration::from_secs_f64(now.max(0.0))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a Windows default render endpoint and hardware media engine"]
    fn multitrack_preview_device_smoke() {
        let dir = crate::trim::tests::temp_dir("audio-preview-device");
        let path = dir.join("source.mp4");
        crate::trim::tests::write_test_clip(&path, 3, 2);
        let (player, events) = Player::open(&path, 320);
        let mut tracks = crate::audio::discover(&path).unwrap();
        assert_eq!(tracks.len(), 2);
        // Commands before metadata must remain applied after the player loads.
        tracks[0].muted = true;
        tracks[1].gain_percent = 50;
        player.set_audio_tracks(tracks.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut loaded = false;
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(PlayerEvent::Loaded { .. }) => {
                    loaded = true;
                    break;
                }
                Ok(PlayerEvent::Error(error) | PlayerEvent::AudioError(error)) => panic!("{error}"),
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(loaded, "player failed to load");
        player.play();
        let deadline = Instant::now() + Duration::from_millis(600);
        let mut positions = 0;
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(PlayerEvent::Error(error) | PlayerEvent::AudioError(error)) => panic!("{error}"),
                Ok(PlayerEvent::Position(_)) => positions += 1,
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert!(positions > 0);
        // Repeated gain gestures must not recreate the endpoint or seek the
        // streaming decoder. Exercise live updates while video keeps running.
        for gain in 51..=90 {
            tracks[1].gain_percent = gain;
            player.set_audio_tracks(tracks.clone());
            std::thread::sleep(Duration::from_millis(2));
        }
        player.pause();
        tracks[0].muted = false;
        tracks[0].solo = true;
        tracks[1].segments.clear();
        player.set_audio_tracks(tracks);
        player.seek(Duration::from_millis(1250));
        player.play();
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(PlayerEvent::Error(error) | PlayerEvent::AudioError(error)) => panic!("{error}"),
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        let started = Instant::now();
        drop(player);
        assert!(started.elapsed() < Duration::from_secs(3));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gain_commands_coalesce_into_one_pending_snapshot() {
        let (tx, commands) = mpsc::channel();
        let (events, _) = async_channel::bounded(16);
        let player = Player {
            tx,
            events,
            thread: None,
            audio_tracks: Arc::new(Mutex::new(None)),
        };
        let mut tracks = vec![AudioTrack::new(
            1,
            "Audio".into(),
            2,
            48_000,
            Duration::from_secs(3),
        )];
        for gain in 1..=200 {
            tracks[0].gain_percent = gain;
            player.set_audio_tracks(tracks.clone());
        }
        assert_eq!(commands.try_iter().count(), 1);
        assert_eq!(
            player.audio_tracks.lock().unwrap().as_ref().unwrap()[0].gain_percent,
            200
        );
    }

    #[test]
    fn preview_size_cases() {
        assert_eq!(preview_size(1920, 1080, 960), (960, 540));
        assert_eq!(preview_size(1280, 720, 1920), (1280, 720));
        assert_eq!(preview_size(1001, 563, 501), (500, 280));
        assert_eq!(preview_size(1, 1, 10), (2, 2));
    }

    #[test]
    fn missing_file_reports_error_and_drops_promptly() {
        let started = Instant::now();
        let (player, rx) = Player::open(Path::new("Z:/definitely/not/here.mp4"), 640);
        let event = rx.recv_blocking().expect("event");
        assert!(matches!(event, PlayerEvent::Error(m) if m.contains("could not be found")));
        drop(player);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
