//! Bounded multitrack preview output synchronized to the media engine.
#![allow(unsafe_code)]

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_only_changes_preserve_cursor_but_topology_changes_reset() {
        let original = vec![AudioTrack::new(
            1,
            "Audio".into(),
            2,
            48_000,
            Duration::from_secs(3),
        )];
        let mut changed = original.clone();
        changed[0].gain_percent = 150;
        assert!(gain_only_update(&original, &changed));
        changed[0].selected_segment = None;
        changed[0].label = "Renamed".into();
        assert!(gain_only_update(&original, &changed));
        changed[0].gain_percent = 0;
        assert!(!gain_only_update(&original, &changed));
        changed[0].gain_percent = 150;
        changed[0].muted = true;
        assert!(!gain_only_update(&original, &changed));
        changed[0].muted = false;
        changed[0].segments[0].start = Duration::from_millis(250);
        assert!(!gain_only_update(&original, &changed));
    }

    #[test]
    fn control_gain_updates_do_not_flush_audio_or_accumulate_snapshots() {
        let preview = AudioPreview {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    tracks: Arc::new(Vec::new()),
                    position: Duration::ZERO,
                    playing: false,
                    revision: 0,
                    reset: 0,
                    device: 0,
                    shutdown: false,
                }),
                changed: Condvar::new(),
                cancel: AtomicBool::new(false),
                status: AtomicU8::new(0),
                armed: AtomicBool::new(false),
            }),
            worker: None,
        };
        let mut tracks = vec![AudioTrack::new(
            1,
            "Audio".into(),
            2,
            48_000,
            Duration::from_secs(3),
        )];
        preview.set_tracks(tracks.clone());
        let initial_reset = preview.shared.lock().reset;
        for gain in 101..=200 {
            tracks[0].gain_percent = gain;
            preview.set_tracks(tracks.clone());
        }
        {
            let state = preview.shared.lock();
            assert_eq!(state.reset, initial_reset);
            assert_eq!(state.tracks[0].gain_percent, 200);
            assert_eq!(state.tracks.len(), 1);
        }
        tracks[0].selected_segment = None;
        preview.set_tracks(tracks.clone());
        assert_eq!(preview.shared.lock().reset, initial_reset);
        tracks[0].muted = true;
        preview.set_tracks(tracks);
        assert_eq!(preview.shared.lock().reset, initial_reset + 1);
    }

    #[test]
    fn buffer_budget_stays_bounded() {
        assert_eq!(buffer_budget(0, 4800), TARGET_FRAMES);
        assert_eq!(buffer_budget(TARGET_FRAMES, 4800), 0);
        assert_eq!(buffer_budget(100, 200), 100);
        assert_eq!(buffer_budget(300, 200), 0);
    }

    #[test]
    fn frame_clock_uses_media_time_only() {
        assert_eq!(source_frame(Duration::from_millis(1250)), 60_000);
        assert!(!needs_resync(60_000, 60_000, 0));
        assert!(!needs_resync(61_920, 60_000, 1920));
        assert!(needs_resync(70_000, 60_000, 1920));
        assert!(needs_resync(50_000, 60_000, 0));
    }
}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use clipper_editor::AudioTrack;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE, EDataFlow, ERole, IAudioClient,
    IAudioRenderClient, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
    MMDeviceEnumerator, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::PCWSTR;

use crate::audio::{MixDecoder, SAMPLE_RATE};
use crate::player::PlayerEvent;
use clipper_platform::windows::ComApartment;

// Forty milliseconds of output is the only playback lookahead. No wall-clock
// extrapolation is used: scheduling always starts from MediaEngine's time.
const TARGET_FRAMES: u32 = 1920;

fn source_frame(position: Duration) -> u64 {
    (position.as_nanos().saturating_mul(u128::from(SAMPLE_RATE)) / 1_000_000_000)
        .min(u128::from(u64::MAX)) as u64
}

fn buffer_budget(padding: u32, capacity: u32) -> u32 {
    TARGET_FRAMES.min(capacity).saturating_sub(padding)
}

fn needs_resync(cursor: u64, now: u64, padding: u32) -> bool {
    cursor.abs_diff(now.saturating_add(u64::from(padding))) > u64::from(SAMPLE_RATE / 20)
}

/// Gain edits preserve the streaming reader only while the audible source
/// identities and source-time masks stay unchanged. UI selection and labels
/// never alter playback topology.
fn gain_only_update(before: &[AudioTrack], after: &[AudioTrack]) -> bool {
    before.len() == after.len()
        && before.iter().all(|old| {
            after
                .iter()
                .find(|new| new.stream_index == old.stream_index)
                .is_some_and(|new| {
                    old.channels == new.channels
                        && old.sample_rate == new.sample_rate
                        && old.enabled == new.enabled
                        && old.muted == new.muted
                        && old.solo == new.solo
                        && old.segments == new.segments
                        && (clipper_editor::effective_gain(before, old.stream_index) > 0.0)
                            == (clipper_editor::effective_gain(after, new.stream_index) > 0.0)
                })
        })
}

#[derive(Clone)]
struct State {
    tracks: Arc<Vec<AudioTrack>>,
    position: Duration,
    playing: bool,
    revision: u64,
    reset: u64,
    device: u64,
    shutdown: bool,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    cancel: AtomicBool,
    status: AtomicU8,
    armed: AtomicBool,
}
impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn endpoint_changed(&self) {
        let mut state = self.lock();
        state.device = state.device.wrapping_add(1);
        state.revision = state.revision.wrapping_add(1);
        self.changed.notify_one();
    }
}

/// Only a coalesced control snapshot crosses threads. Decoded PCM is consumed
/// immediately by WASAPI, so neither slow decoding nor UI stalls grow a queue.
pub(crate) struct AudioPreview {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl AudioPreview {
    pub(crate) fn open(
        path: &Path,
        events: async_channel::Sender<PlayerEvent>,
    ) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                tracks: Arc::new(Vec::new()),
                position: Duration::ZERO,
                playing: false,
                revision: 0,
                reset: 0,
                device: 0,
                shutdown: false,
            }),
            changed: Condvar::new(),
            cancel: AtomicBool::new(false),
            status: AtomicU8::new(0),
            armed: AtomicBool::new(false),
        });
        let thread_shared = shared.clone();
        let path = path.to_owned();
        let worker = std::thread::Builder::new()
            .name("clip-audio-preview".into())
            .spawn(move || {
                if let Err(error) = run(path, &thread_shared, &events) {
                    thread_shared.status.store(2, Ordering::Release);
                    let _ = events.send_blocking(PlayerEvent::AudioError(error));
                }
            })
            .map_err(|error| format!("Could not start preview audio: {error}"))?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    /// True after initialization or an explicit failure. Failed custom audio
    /// stays silent rather than leaking an unmuted/untrimmed engine track.
    pub(crate) fn owns_audio(&self) -> bool {
        self.shared.status.load(Ordering::Acquire) != 0
    }
    /// The engine owner calls this only after SetMuted has succeeded. Custom
    /// PCM cannot start before then, preventing duplicate engine/WASAPI audio.
    pub(crate) fn activate(&self) {
        if !self.shared.armed.swap(true, Ordering::AcqRel) {
            let mut state = self.shared.lock();
            state.revision = state.revision.wrapping_add(1);
            self.shared.changed.notify_one();
        }
    }
    pub(crate) fn set_tracks(&self, tracks: Vec<AudioTrack>) {
        let mut state = self.shared.lock();
        if state.tracks.as_ref() == &tracks {
            return;
        }
        if !gain_only_update(&state.tracks, &tracks) {
            state.reset = state.reset.wrapping_add(1);
        }
        state.tracks = Arc::new(tracks);
        state.revision = state.revision.wrapping_add(1);
        self.shared.changed.notify_one();
    }
    pub(crate) fn synchronize(&self, position: Duration, playing: bool, reset: bool) {
        let mut state = self.shared.lock();
        state.position = position;
        if reset || state.playing != playing {
            state.reset = state.reset.wrapping_add(1);
        }
        state.playing = playing;
        state.revision = state.revision.wrapping_add(1);
        self.shared.changed.notify_one();
    }
}
impl Drop for AudioPreview {
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::Release);
        {
            let mut state = self.shared.lock();
            state.shutdown = true;
            self.shared.changed.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[windows_core::implement(IMMNotificationClient)]
struct Watcher(Arc<Shared>);
impl IMMNotificationClient_Impl for Watcher_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.0.endpoint_changed();
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender && role == eConsole {
            self.0.endpoint_changed();
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}
struct Follower {
    enumerator: IMMDeviceEnumerator,
    watcher: IMMNotificationClient,
}
impl Drop for Follower {
    fn drop(&mut self) {
        // SAFETY: watcher was registered with this enumerator and remains live.
        let _ = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.watcher)
        };
    }
}

struct Output {
    client: IAudioClient,
    render: IAudioRenderClient,
    capacity: u32,
    started: bool,
}
impl Output {
    fn open(enumerator: &IMMDeviceEnumerator) -> Result<Self, String> {
        // SAFETY: COM is initialized on this worker. The standard IEEE float
        // stereo format is converted to the endpoint's shared-mode mix format.
        unsafe {
            let endpoint = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(os)?;
            let client: IAudioClient = endpoint.Activate(CLSCTX_ALL, None).map_err(os)?;
            let format = WAVEFORMATEX {
                wFormatTag: 3,
                nChannels: 2,
                nSamplesPerSec: SAMPLE_RATE,
                nAvgBytesPerSec: SAMPLE_RATE * 8,
                nBlockAlign: 8,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    1_000_000,
                    0,
                    &format,
                    None,
                )
                .map_err(os)?;
            let capacity = client.GetBufferSize().map_err(os)?;
            let render = client.GetService::<IAudioRenderClient>().map_err(os)?;
            Ok(Self {
                client,
                render,
                capacity,
                started: false,
            })
        }
    }
    fn reset(&mut self) -> Result<(), String> {
        // SAFETY: only this worker accesses its client; Reset follows Stop.
        unsafe {
            self.client.Stop().map_err(os)?;
            self.client.Reset().map_err(os)?;
        }
        self.started = false;
        Ok(())
    }
    fn padding(&self) -> Result<u32, String> {
        // SAFETY: initialized client owned by this worker.
        unsafe { self.client.GetCurrentPadding().map_err(os) }
    }
    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        let frames = (samples.len() / 2) as u32;
        if frames == 0 {
            return Ok(());
        }
        // SAFETY: budget never exceeds available frames. GetBuffer returns
        // interleaved stereo float memory for exactly this number of frames.
        unsafe {
            let output = self.render.GetBuffer(frames).map_err(os)?;
            std::ptr::copy_nonoverlapping(samples.as_ptr(), output.cast::<f32>(), samples.len());
            self.render.ReleaseBuffer(frames, 0).map_err(os)?;
            if !self.started {
                self.client.Start().map_err(os)?;
                self.started = true;
            }
        }
        Ok(())
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        // SAFETY: client remains valid until after this destructor.
        let _ = unsafe { self.client.Stop() };
    }
}
fn os(error: windows::core::Error) -> String {
    format!("Preview audio unavailable: {}", error.message())
}

fn run(
    path: PathBuf,
    shared: &Arc<Shared>,
    events: &async_channel::Sender<PlayerEvent>,
) -> Result<(), String> {
    let _com = ComApartment::init_multithreaded().map_err(os)?;
    // SAFETY: standard COM activation on this initialized worker.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(os)?;
    let watcher: IMMNotificationClient = Watcher(shared.clone()).into();
    // SAFETY: the callback remains owned by Follower until unregistering.
    unsafe { enumerator.RegisterEndpointNotificationCallback(&watcher) }.map_err(os)?;
    let _follower = Follower {
        enumerator: enumerator.clone(),
        watcher,
    };
    let mut output: Option<Output> = None;
    let mut decoder: Option<MixDecoder> = None;
    let mut tracks: Option<Arc<Vec<AudioTrack>>> = None;
    let (mut revision, mut reset, mut device) = (u64::MAX, u64::MAX, u64::MAX);
    let mut cursor = None;
    let mut failed = false;
    loop {
        let state = {
            let mut state = shared.lock();
            while state.revision == revision && !state.shutdown {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if state.shutdown {
                return Ok(());
            }
            state.clone()
        };
        revision = state.revision;
        let changed = tracks
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, &state.tracks));
        if device != state.device || (failed && (changed || reset != state.reset)) {
            if failed {
                tracks = None;
            }
            output = None;
            failed = false;
            device = state.device;
            cursor = None;
        }
        let result = (|| -> Result<(), String> {
            if failed {
                return Ok(());
            }
            if state.tracks.is_empty() {
                output = None;
                decoder = None;
                tracks = Some(state.tracks.clone());
                cursor = None;
                reset = state.reset;
                shared.status.store(1, Ordering::Release);
                return Ok(());
            }
            if output.is_none() {
                output = Some(Output::open(&enumerator)?);
            }
            let mut rebuilt = false;
            if tracks
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &state.tracks))
            {
                let gain_updated = tracks
                    .as_ref()
                    .is_some_and(|old| gain_only_update(old, &state.tracks))
                    && decoder
                        .as_mut()
                        .is_some_and(|decoder| decoder.update_gains(&state.tracks));
                if !gain_updated {
                    decoder = Some(MixDecoder::open(&path, &state.tracks)?);
                    rebuilt = true;
                }
                tracks = Some(state.tracks.clone());
            }
            let output = output.as_mut().expect("output opened above");
            if reset != state.reset || rebuilt {
                output.reset()?;
                cursor = None;
                reset = state.reset;
            }
            shared.status.store(1, Ordering::Release);
            if !state.playing || !shared.armed.load(Ordering::Acquire) {
                return Ok(());
            }
            let padding = output.padding()?;
            let now = source_frame(state.position);
            if cursor.is_some_and(|cursor| needs_resync(cursor, now, padding)) {
                output.reset()?;
                cursor = None;
            }
            if cursor.is_none() {
                if let Some(decoder) = &mut decoder {
                    decoder.seek(state.position)?;
                }
                cursor = Some(now);
            }
            let budget = buffer_budget(output.padding()?, output.capacity);
            if budget == 0 {
                return Ok(());
            }
            let mut mixed = vec![0.0; budget as usize * 2];
            if let Some(decoder) = &mut decoder {
                let mut written = 0;
                while written < mixed.len() && !shared.cancel.load(Ordering::Acquire) {
                    let Some(chunk) = decoder.next((mixed.len() - written) / 2, &shared.cancel)?
                    else {
                        break;
                    };
                    if chunk.samples.is_empty() {
                        break;
                    }
                    let amount = chunk.samples.len().min(mixed.len() - written);
                    mixed[written..written + amount].copy_from_slice(&chunk.samples[..amount]);
                    written += amount;
                }
            }
            if shared.cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            {
                let current = shared.lock();
                if current.reset != state.reset || current.device != state.device {
                    // A seek, pause or edit arrived while decoding. Never
                    // publish PCM from the superseded control snapshot.
                    return Ok(());
                }
            }
            output.write(&mixed)?;
            cursor = cursor.map(|cursor| cursor.saturating_add(u64::from(budget)));
            Ok(())
        })();
        if let Err(error) = result {
            output = None;
            decoder = None;
            tracks = Some(state.tracks.clone());
            cursor = None;
            failed = true;
            reset = state.reset;
            shared.status.store(2, Ordering::Release);
            let _ = events.send_blocking(PlayerEvent::AudioError(error));
        }
    }
}
