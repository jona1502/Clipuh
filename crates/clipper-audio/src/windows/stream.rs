//! Generic event-driven capture thread shared by every backend.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED,
    AUDCLNT_E_RESOURCES_INVALIDATED, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioCaptureClient, IAudioClient, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
    GetCurrentProcess, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::{HRESULT, PCWSTR, w};

use super::engine::qpc_now_ticks;
use crate::mix;
use crate::ring::{AudioProducer, PacketHeader};
use crate::timeline::frames_to_ticks;
use crate::types::{AudioError, AudioEvent, AudioLevels, FrameFlags, SAMPLE_RATE};

/// Owned Win32 handle (event or process); closed on drop.
pub(crate) struct OwnedHandle(pub HANDLE);

// SAFETY: a kernel handle value may be used and closed from any thread.
unsafe impl Send for OwnedHandle {}
// SAFETY: waiting on / signaling a kernel handle is thread-safe.
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    /// New unnamed event, initially non-signaled.
    pub fn new_event(manual_reset: bool) -> Result<Self, AudioError> {
        // SAFETY: plain event creation without security attributes or name.
        unsafe { CreateEventW(None, manual_reset, false, PCWSTR::null()) }
            .map(Self)
            .map_err(|e| AudioError::Os(format!("CreateEventW: {e}")))
    }

    /// Duplicate handle to the same kernel object (owned separately).
    pub fn duplicate(&self) -> Result<Self, AudioError> {
        let mut out = HANDLE::default();
        // SAFETY: both process handles are the current-process pseudo handle,
        // `self.0` is a valid handle we own, `out` is a valid out pointer.
        unsafe {
            let process = GetCurrentProcess();
            DuplicateHandle(
                process,
                self.0,
                process,
                &mut out,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .map_err(|e| AudioError::Os(format!("DuplicateHandle: {e}")))?;
        Ok(Self(out))
    }

    /// Signals the event.
    pub fn signal(&self) {
        // SAFETY: valid event handle owned by `self`.
        let _ = unsafe { SetEvent(self.0) };
    }
}

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

    /// Spacing of `needs_reopen` polls. Default [`RETRY_POLL`] (1 s).
    fn poll_interval(&self) -> Duration {
        RETRY_POLL
    }

    /// Hands the source a duplicate of the thread's stop event, called once
    /// before the first `open`, so a long blocking `open` (process loopback
    /// activation) can be interrupted by stop. Default: ignored.
    fn set_stop_handle(&mut self, _stop: OwnedHandle) {}
}

/// Scope guard: joins the COM MTA on this thread and leaves it on drop only
/// when this call actually initialized COM.
pub(crate) struct ComGuard {
    uninit: bool,
}

impl ComGuard {
    pub fn init() -> Self {
        // SAFETY: CoInitializeEx has no pointer preconditions.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK / S_FALSE both need a matching CoUninitialize; failure
        // (RPC_E_CHANGED_MODE: thread already in an STA) leaves COM usable
        // but is not ours to uninitialize.
        Self { uninit: hr.is_ok() }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: balances the successful CoInitializeEx in `init`.
            unsafe { CoUninitialize() };
        }
    }
}

/// HRESULT_FROM_WIN32(ERROR_NOT_FOUND), the real `E_NOTFOUND`.
const E_NOTFOUND: HRESULT = HRESULT(0x8007_0490_u32 as i32);

/// Maps a WASAPI/MMDevice error: device gone, busy or missing is
/// `SourceUnavailable` (retryable), anything else `Os`.
pub(crate) fn map_os_error(context: &str, error: &windows::core::Error) -> AudioError {
    let code = error.code();
    if code == AUDCLNT_E_DEVICE_INVALIDATED
        || code == AUDCLNT_E_DEVICE_IN_USE
        || code == AUDCLNT_E_RESOURCES_INVALIDATED
        || code == E_NOTFOUND
    {
        AudioError::SourceUnavailable(format!("{context}: {error}"))
    } else {
        AudioError::Os(format!("{context}: {error}"))
    }
}

/// Requested buffer duration: 200 ms in 100 ns units.
const BUFFER_DURATION_HNS: i64 = 2_000_000;

/// Initializes `client` for shared-mode, event-driven capture with
/// `AUTOCONVERTPCM | SRC_DEFAULT_QUALITY` at 48 kHz f32 `channels`, plus
/// `LOOPBACK` when `loopback` is set (endpoint or process loopback).
///
/// Never calls `GetMixFormat`, so it is valid for process-loopback clients
/// (which do not support it). Windows 10+.
pub(crate) fn init_capture_client(
    client: IAudioClient,
    channels: u16,
    loopback: bool,
    description: String,
) -> Result<OpenedStream, AudioError> {
    const BYTES_PER_SAMPLE: u16 = 4;
    let block_align = channels * BYTES_PER_SAMPLE;
    let channel_mask = match channels {
        1 => 0x4, // SPEAKER_FRONT_CENTER
        2 => 0x3, // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
        _ => 0,
    };
    let format = WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * u32::from(block_align),
            nBlockAlign: block_align,
            wBitsPerSample: 32,
            cbSize: (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: channel_mask,
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    };
    let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if loopback {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    // SAFETY: `format` is a fully initialized WAVEFORMATEXTENSIBLE (layout
    // starts with WAVEFORMATEX, cbSize covers the extension) that outlives
    // the call.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            flags,
            BUFFER_DURATION_HNS,
            0,
            std::ptr::from_ref(&format).cast::<WAVEFORMATEX>(),
            None,
        )
    }
    .map_err(|e| map_os_error(&format!("IAudioClient::Initialize ({description})"), &e))?;

    let ready = OwnedHandle::new_event(false)?;
    // SAFETY: `ready.0` is a valid event that the caller keeps alive next to
    // the client inside `OpenedStream`.
    unsafe { client.SetEventHandle(ready.0) }
        .map_err(|e| map_os_error("IAudioClient::SetEventHandle", &e))?;
    // SAFETY: the client was initialized above.
    let capture = unsafe { client.GetService::<IAudioCaptureClient>() }
        .map_err(|e| map_os_error("IAudioClient::GetService(IAudioCaptureClient)", &e))?;
    Ok(OpenedStream {
        client,
        capture,
        ready,
        channels,
        reopen: None,
        description,
    })
}

/// Latest levels of a track, lock-free.
#[derive(Debug, Default)]
pub(crate) struct LevelCell {
    peak: AtomicU32,
    rms: AtomicU32,
    source_failed: AtomicU32,
}

impl LevelCell {
    pub fn store(&self, levels: AudioLevels) {
        self.peak.store(levels.peak.to_bits(), Ordering::Relaxed);
        self.rms.store(levels.rms.to_bits(), Ordering::Relaxed);
    }

    pub fn source_failed(&self) -> bool {
        self.source_failed.load(Ordering::Acquire) != 0
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
    stop: Arc<OwnedHandle>,
    thread: Option<JoinHandle<()>>,
}

impl StreamThread {
    /// Spawns the dedicated capture thread for `track`: MTA COM, MMCSS
    /// "Pro Audio" (best effort), opens `source`, waits on its event, pushes
    /// packets into `producer`, stores levels, reports `events` with
    /// `try_send` only. Reopens on device loss / reopen signal.
    #[cfg(test)]
    pub fn spawn(
        track: usize,
        channels: u16,
        source: Box<dyn StreamSource>,
        producer: AudioProducer,
        levels: Arc<LevelCell>,
        events: SyncSender<AudioEvent>,
    ) -> Result<Self, AudioError> {
        Self::spawn_with_wake(track, channels, source, producer, levels, events, None)
    }

    pub fn spawn_with_wake(
        track: usize,
        channels: u16,
        source: Box<dyn StreamSource>,
        producer: AudioProducer,
        levels: Arc<LevelCell>,
        events: SyncSender<AudioEvent>,
        wake: Option<SyncSender<()>>,
    ) -> Result<Self, AudioError> {
        let stop = Arc::new(OwnedHandle::new_event(true)?);
        let mut source = source;
        source.set_stop_handle(stop.duplicate()?);
        let worker = Worker {
            track,
            channels,
            source,
            producer,
            levels,
            events,
            wake,
            stop: Arc::clone(&stop),
            overflow_frames: 0,
            overflow_reported: Instant::now(),
            next_timestamp: None,
        };
        let thread = std::thread::Builder::new()
            .name(format!("clipper-audio-{track}"))
            .spawn(move || worker.run())
            .map_err(|e| AudioError::Os(format!("spawn capture thread: {e}")))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Signals stop without joining, so several threads can wind down in
    /// parallel before `stop` joins each.
    pub fn signal_stop(&self) {
        self.stop.signal();
    }

    /// Signals stop and joins the thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.stop.signal();
            let _ = thread.join();
        }
    }
}

impl Drop for StreamThread {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Why a streaming session ended.
enum Exit {
    Stop,
    /// Default device changed, source asked for a reopen, or device lost.
    Reopen,
    /// Draining failed (carries the HRESULT text); reopen.
    Drain(String),
    /// Unexpected API failure; reopen after a pause.
    Fault,
}

const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// Default spacing of `StreamSource::needs_reopen` polls.
pub(crate) const RETRY_POLL: Duration = Duration::from_secs(1);
/// A session shorter than this is "short": the next open waits first.
const MIN_SESSION: Duration = Duration::from_secs(1);
/// Pause before reopening after a short session (interruptible by stop).
const REOPEN_BACKOFF: Duration = Duration::from_millis(500);
/// Without a packet for this long the published levels fall back to zero.
const LEVEL_IDLE: Duration = Duration::from_millis(200);
/// Extra attempts for a non-`SourceUnavailable` error on the first open.
const FIRST_OPEN_RETRIES: u32 = 3;

struct Worker {
    track: usize,
    channels: u16,
    source: Box<dyn StreamSource>,
    producer: AudioProducer,
    levels: Arc<LevelCell>,
    events: SyncSender<AudioEvent>,
    wake: Option<SyncSender<()>>,
    stop: Arc<OwnedHandle>,
    overflow_frames: u64,
    overflow_reported: Instant,
    /// Predicted timestamp of the next packet (previous + its duration), used
    /// when WASAPI reports a timestamp error. Reset on every open.
    next_timestamp: Option<i64>,
}

/// MMCSS registration, reverted on drop.
struct Mmcss(Option<HANDLE>);

impl Mmcss {
    fn join_pro_audio() -> Self {
        let mut index = 0u32;
        // SAFETY: valid task name and out pointer.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) }.ok();
        Self(handle)
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            // SAFETY: handle came from AvSetMmThreadCharacteristicsW on this thread.
            let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        }
    }
}

impl Worker {
    fn emit(&self, event: AudioEvent) {
        if matches!(
            event,
            AudioEvent::Failed { .. } | AudioEvent::SourceLost { .. }
        ) {
            self.levels.source_failed.store(1, Ordering::Release);
        }
        let _ = self.events.try_send(event);
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }

    /// Waits up to `duration` for stop; true when stop was signaled.
    fn wait_for(&self, duration: Duration) -> bool {
        // SAFETY: valid event handle owned by `self.stop`.
        let result = unsafe { WaitForSingleObject(self.stop.0, duration.as_millis() as u32) };
        result == WAIT_OBJECT_0
    }

    /// Waits up to `RETRY_INTERVAL` for stop; true when stop was signaled.
    fn wait_stop(&self) -> bool {
        self.wait_for(RETRY_INTERVAL)
    }

    /// Thread entry. Every COM object (source, streams) is owned by
    /// `run_in_com`, which consumes `self`, so all of them are released
    /// before `_com` calls `CoUninitialize`.
    fn run(self) {
        let _com = ComGuard::init();
        self.run_in_com();
    }

    fn run_in_com(mut self) {
        let _mmcss = Mmcss::join_pro_audio();
        let mut first = true;
        let mut first_failures = 0u32;
        let mut lost_reported = false;
        let mut last_description: Option<String> = None;
        loop {
            let stream = match self.source.open(self.channels) {
                Ok(stream) => stream,
                Err(error) => {
                    let reason = match error {
                        AudioError::SourceUnavailable(reason) => reason,
                        other if first => {
                            first_failures += 1;
                            if first_failures > FIRST_OPEN_RETRIES {
                                self.emit(AudioEvent::Failed {
                                    track: self.track,
                                    message: other.to_string(),
                                });
                                return;
                            }
                            if self.wait_stop() {
                                return;
                            }
                            continue;
                        }
                        other => other.to_string(),
                    };
                    first = false;
                    if !lost_reported {
                        lost_reported = true;
                        self.emit(AudioEvent::SourceLost {
                            track: self.track,
                            reason,
                        });
                    }
                    if self.wait_stop() {
                        return;
                    }
                    continue;
                }
            };
            first = false;
            // SAFETY: the client is initialized; Start has no other preconditions.
            if let Err(error) = unsafe { stream.client.Start() } {
                if !lost_reported {
                    lost_reported = true;
                    self.emit(AudioEvent::SourceLost {
                        track: self.track,
                        reason: format!("IAudioClient::Start: {error}"),
                    });
                }
                drop(stream);
                if self.wait_stop() {
                    return;
                }
                continue;
            }
            lost_reported = false;
            self.next_timestamp = None;
            self.emit(AudioEvent::TrackStarted { track: self.track });
            if last_description
                .as_deref()
                .is_some_and(|previous| previous != stream.description)
            {
                self.emit(AudioEvent::DeviceChanged {
                    track: self.track,
                    device: stream.description.clone(),
                });
            }
            last_description = Some(stream.description.clone());
            let started = Instant::now();
            let exit = self.stream_loop(&stream);
            // SAFETY: the stream was started; errors on Stop are irrelevant.
            let _ = unsafe { stream.client.Stop() };
            drop(stream);
            match exit {
                Exit::Stop => return,
                Exit::Reopen => {
                    if started.elapsed() < MIN_SESSION && self.wait_for(REOPEN_BACKOFF) {
                        return;
                    }
                }
                Exit::Drain(reason) => {
                    if !lost_reported {
                        lost_reported = true;
                        self.emit(AudioEvent::SourceLost {
                            track: self.track,
                            reason,
                        });
                    }
                    if started.elapsed() < MIN_SESSION && self.wait_for(REOPEN_BACKOFF) {
                        return;
                    }
                }
                Exit::Fault => {
                    self.emit(AudioEvent::SourceLost {
                        track: self.track,
                        reason: "WaitForMultipleObjects returned an unexpected result".into(),
                    });
                    if self.wait_stop() {
                        return;
                    }
                }
            }
        }
    }

    fn stream_loop(&mut self, stream: &OpenedStream) -> Exit {
        let mut handles = [self.stop.0, stream.ready.0, HANDLE::default()];
        let count = match &stream.reopen {
            Some(reopen) => {
                handles[2] = reopen.0;
                3
            }
            None => 2,
        };
        let mut last_poll = Instant::now();
        let mut last_packet = Instant::now();
        let mut levels_idle = false;
        loop {
            // SAFETY: all handles are valid events that outlive this call
            // (stop is owned by self, the others by `stream`).
            let wait = unsafe {
                WaitForMultipleObjects(&handles[..count], false, LEVEL_IDLE.as_millis() as u32)
            };
            if wait == WAIT_TIMEOUT {
                if !levels_idle && last_packet.elapsed() >= LEVEL_IDLE {
                    levels_idle = true;
                    self.levels.store(AudioLevels::default());
                }
            } else {
                match wait.0.wrapping_sub(WAIT_OBJECT_0.0) {
                    0 => return Exit::Stop,
                    1 => {
                        last_packet = Instant::now();
                        levels_idle = false;
                        if let Err(reason) = self.drain(stream) {
                            return Exit::Drain(reason);
                        }
                    }
                    2 => return Exit::Reopen,
                    _ => return Exit::Fault,
                }
            }
            if last_poll.elapsed() >= self.source.poll_interval() {
                last_poll = Instant::now();
                if self.source.needs_reopen() {
                    return Exit::Reopen;
                }
                self.report_overflow();
            }
        }
    }

    /// Moves every pending packet into the ring. `Err` = stream unusable,
    /// with the failing call and HRESULT text.
    fn drain(&mut self, stream: &OpenedStream) -> Result<(), String> {
        let channels = usize::from(stream.channels);
        loop {
            // SAFETY: the capture client belongs to a started stream.
            let pending = unsafe { stream.capture.GetNextPacketSize() }
                .map_err(|e| format!("GetNextPacketSize: {e}"))?;
            if pending == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut qpc = 0u64;
            // SAFETY: out pointers are valid locals; the returned buffer is
            // valid until ReleaseBuffer below and only read in between.
            unsafe {
                stream
                    .capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
            }
            .map_err(|e| format!("GetBuffer: {e}"))?;
            if frames == 0 {
                // AUDCLNT_S_BUFFER_EMPTY: nothing acquired, nothing to release.
                return Ok(());
            }
            let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null();
            let mut discontinuity = flags & (AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0;
            // A flagged or zero QPC position is unusable: continue from the
            // previous packet's end (or "now" for the very first packet).
            let timestamp =
                if flags & (AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32) != 0 || qpc == 0 {
                    discontinuity = true;
                    self.next_timestamp.unwrap_or_else(qpc_now_ticks)
                } else {
                    qpc as i64
                };
            self.next_timestamp =
                Some(timestamp.saturating_add(frames_to_ticks(i64::from(frames))));
            let header = PacketHeader {
                timestamp,
                frames,
                flags: FrameFlags {
                    silent,
                    discontinuity,
                },
            };
            let pushed = if silent {
                self.levels.store(AudioLevels::default());
                self.producer.push(header, None)
            } else {
                // SAFETY: the stream was initialized as f32, so WASAPI gives
                // `frames * channels` aligned f32 samples at `data`, valid
                // until ReleaseBuffer.
                let samples = unsafe {
                    std::slice::from_raw_parts(data.cast::<f32>(), frames as usize * channels)
                };
                self.levels.store(mix::measure(samples));
                self.producer.push(header, Some(samples))
            };
            // SAFETY: releases exactly the frames acquired by GetBuffer.
            unsafe { stream.capture.ReleaseBuffer(frames) }
                .map_err(|e| format!("ReleaseBuffer: {e}"))?;
            if let Some(wake) = &self.wake {
                let _ = wake.try_send(());
            }
            if !pushed {
                self.overflow_frames += u64::from(frames);
                self.report_overflow();
            }
        }
    }

    /// Emits at most one `Overflow` per second.
    fn report_overflow(&mut self) {
        if self.overflow_frames > 0 && self.overflow_reported.elapsed() >= RETRY_INTERVAL {
            self.overflow_reported = Instant::now();
            let frames = std::mem::take(&mut self.overflow_frames);
            self.emit(AudioEvent::Overflow {
                track: self.track,
                frames,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::sync::mpsc::sync_channel;

    use super::*;
    use crate::ring::audio_ring;
    use crate::types::MicrophoneChoice;
    use crate::windows::endpoint::{loopback_source, microphone_channels, microphone_source};

    /// Drains `consumer` for up to `max`, returning (packets, frames, peak).
    fn collect(
        consumer: &mut crate::ring::AudioConsumer,
        max: Duration,
        stop_at_peak: Option<f32>,
    ) -> (u64, u64, f32) {
        let start = Instant::now();
        let (mut packets, mut frames, mut peak) = (0u64, 0u64, 0f32);
        while start.elapsed() < max {
            while let Some((n, p)) = consumer.pop_with(|header, samples| {
                (
                    u64::from(header.frames),
                    samples.iter().fold(0f32, |m, s| m.max(s.abs())),
                )
            }) {
                packets += 1;
                frames += n;
                peak = peak.max(p);
            }
            if stop_at_peak.is_some_and(|t| peak >= t) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        (packets, frames, peak)
    }

    fn write_sine_wav(path: &std::path::Path) {
        let rate = 48_000u32;
        let seconds = 2u32;
        let frames = rate * seconds;
        let mut data = Vec::with_capacity(frames as usize * 4);
        for n in 0..frames {
            let t = n as f32 / rate as f32;
            let s = (t * 880.0 * std::f32::consts::TAU).sin() * 0.5;
            let v = (s * f32::from(i16::MAX)) as i16;
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
        std::fs::write(path, wav).expect("write wav");
    }

    #[test]
    #[ignore = "needs audio hardware; plays a test tone through the speakers"]
    fn hardware_loopback_captures_played_sound() {
        let wav = std::env::temp_dir().join("clipper-audio-test-sine.wav");
        write_sine_wav(&wav);
        let (producer, mut consumer) = audio_ring(2, 500);
        let levels = Arc::new(LevelCell::default());
        let (tx, rx) = sync_channel(64);
        let thread =
            StreamThread::spawn(0, 2, loopback_source(), producer, Arc::clone(&levels), tx)
                .expect("spawn");
        let mut player = Command::new("powershell")
            .args([
                "-NoProfile",
                "-c",
                &format!(
                    "(New-Object Media.SoundPlayer '{}').PlaySync()",
                    wav.display()
                ),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("powershell");
        let (packets, frames, peak) = collect(&mut consumer, Duration::from_secs(6), Some(0.05));
        let level_peak = levels.load().peak;
        let _ = player.kill();
        let _ = player.wait();
        thread.stop();
        let events: Vec<_> = rx.try_iter().collect();
        println!(
            "loopback: packets={packets} frames={frames} peak={peak} level_peak={level_peak} events={events:?}"
        );
        assert!(events.contains(&AudioEvent::TrackStarted { track: 0 }));
        assert!(packets > 0, "no packets");
        assert!(peak > 0.05, "no audible signal captured, peak={peak}");
    }

    #[test]
    #[ignore = "needs audio hardware; records from the microphone"]
    fn hardware_microphone_delivers_packets() {
        let devices = crate::windows::list_devices(crate::types::DeviceFlow::Capture)
            .expect("list capture devices");
        if devices.is_empty() {
            println!("microphone test skipped: no active capture device");
            return;
        }
        let channels = microphone_channels(&MicrophoneChoice::Default);
        let (producer, mut consumer) = audio_ring(channels, 500);
        let levels = Arc::new(LevelCell::default());
        let (tx, rx) = sync_channel(64);
        let thread = StreamThread::spawn(
            1,
            channels,
            microphone_source(MicrophoneChoice::Default),
            producer,
            Arc::clone(&levels),
            tx,
        )
        .expect("spawn");
        let (packets, frames, peak) = collect(&mut consumer, Duration::from_millis(1500), None);
        thread.stop();
        let events: Vec<_> = rx.try_iter().collect();
        println!(
            "mic: channels={channels} packets={packets} frames={frames} peak={peak} events={events:?}"
        );
        assert!(events.contains(&AudioEvent::TrackStarted { track: 1 }));
        assert!(packets > 0, "no packets from microphone");
    }

    #[test]
    fn missing_device_reports_source_lost_once_and_stops_cleanly() {
        let (producer, _consumer) = audio_ring(2, 100);
        let (tx, rx) = sync_channel(64);
        let source = microphone_source(MicrophoneChoice::Device("{no-such-device}".to_owned()));
        let thread =
            StreamThread::spawn(2, 2, source, producer, Arc::new(LevelCell::default()), tx)
                .expect("spawn");
        std::thread::sleep(Duration::from_millis(2500));
        thread.stop();
        let lost = rx
            .try_iter()
            .filter(|e| matches!(e, AudioEvent::SourceLost { track: 2, .. }))
            .count();
        assert_eq!(lost, 1);
    }
}
