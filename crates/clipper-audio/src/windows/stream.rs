//! Generic event-driven capture thread shared by every backend.
//! CONTRACT STUB — owned by the WASAPI agent.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{IAudioCaptureClient, IAudioClient};

use crate::ring::AudioProducer;
use crate::types::{AudioError, AudioEvent, AudioLevels};

/// Owned Win32 handle (event or process); closed on drop.
pub(crate) struct OwnedHandle(pub HANDLE);

// SAFETY: a kernel handle value may be used and closed from any thread.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we own the handle and close it exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// An initialized (not yet started) shared-mode capture stream delivering
/// 48 kHz interleaved f32 with the requested channel count.
pub(crate) struct OpenedStream {
    pub client: IAudioClient,
    pub capture: IAudioCaptureClient,
    /// Event passed to `IAudioClient::SetEventHandle`.
    pub ready: OwnedHandle,
    pub channels: u16,
    /// Signaled when the stream must be reopened (target process exited,
    /// default device changed). Optional.
    pub reopen: Option<OwnedHandle>,
    /// Human-readable source description for events (device/process name).
    pub description: String,
}

/// Produces capture streams for one track. `open` runs on the track's own
/// capture thread, which has joined the COM MTA.
pub(crate) trait StreamSource: Send {
    /// Opens a stream with exactly `channels` channels at 48 kHz f32.
    /// `Err(SourceUnavailable)` makes the thread retry later;
    /// `Err(Unsupported | Os)` on the first attempt fails the track.
    fn open(&mut self, channels: u16) -> Result<OpenedStream, AudioError>;

    /// Polled about once per second while streaming; `true` forces a reopen
    /// (e.g. an excluded process just started). Must be cheap.
    fn needs_reopen(&mut self) -> bool {
        false
    }
}

/// Initializes `client` for shared-mode, event-driven capture with
/// `AUTOCONVERTPCM | SRC_DEFAULT_QUALITY` at 48 kHz f32 `channels`, plus
/// `LOOPBACK` when `loopback` is set (endpoint or process loopback).
pub(crate) fn init_capture_client(
    _client: IAudioClient,
    _channels: u16,
    _loopback: bool,
    _description: String,
) -> Result<OpenedStream, AudioError> {
    todo!("WASAPI agent")
}

/// Latest levels of a track, lock-free.
#[derive(Debug, Default)]
pub(crate) struct LevelCell {
    peak: AtomicU32,
    rms: AtomicU32,
}

impl LevelCell {
    pub fn store(&self, levels: AudioLevels) {
        self.peak.store(levels.peak.to_bits(), Ordering::Relaxed);
        self.rms.store(levels.rms.to_bits(), Ordering::Relaxed);
    }

    pub fn load(&self) -> AudioLevels {
        AudioLevels {
            peak: f32::from_bits(self.peak.load(Ordering::Relaxed)),
            rms: f32::from_bits(self.rms.load(Ordering::Relaxed)),
        }
    }
}

/// A running capture thread. Dropping it signals stop and joins.
pub(crate) struct StreamThread {
    _stop: OwnedHandle,
    _thread: Option<JoinHandle<()>>,
}

impl StreamThread {
    /// Spawns the dedicated capture thread for `track`: MTA COM, MMCSS
    /// "Pro Audio" (best effort), opens `source`, waits on its event, pushes
    /// packets into `producer`, stores levels, reports `events` with
    /// `try_send` only. Reopens on device loss / reopen signal.
    pub fn spawn(
        _track: usize,
        _channels: u16,
        _source: Box<dyn StreamSource>,
        _producer: AudioProducer,
        _levels: Arc<LevelCell>,
        _events: SyncSender<AudioEvent>,
    ) -> Result<Self, AudioError> {
        todo!("WASAPI agent")
    }

    /// Signals stop and joins the thread.
    pub fn stop(self) {
        todo!("WASAPI agent")
    }
}
