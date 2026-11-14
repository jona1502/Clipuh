//! Streaming audio decoding, track discovery, and bounded waveform extraction.
#![allow(unsafe_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use crate::trim;
use clipper_editor::{AudioTrack, effective_gain};
use clipper_encoder::backends::media_foundation::MediaFoundation;
use clipper_platform::windows::ComApartment;
use windows::Win32::Media::MediaFoundation::*;
use windows::core::GUID;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const MAX_FRAMES: usize = 8192;
pub const MAX_BUCKETS: usize = 16_384;
const MAX_TRACKS: usize = 32;
const CACHE_FILES: usize = 4;
const MAX_PCM_BYTES: usize = SAMPLE_RATE as usize * 8;
const CANCELLED: &str = "audio operation cancelled";

/// Stereo, interleaved 48 kHz float samples in the original source time domain.
/// Decoded chunks retain their actual stream index. Mixed chunks use u32::MAX.
#[derive(Debug)]
pub struct PcmChunk {
    pub stream_index: u32,
    pub timestamp: Duration,
    pub samples: Vec<f32>,
}

fn duration(reader: &IMFSourceReader) -> Result<Duration, String> {
    // SAFETY: reader is live; PROPVARIANT owns the returned value.
    let value = unsafe {
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
    }
    .map_err(|error| error.to_string())?;
    let ticks = u64::try_from(&value).map_err(|error| error.to_string())?;
    ticks
        .checked_mul(100)
        .map(Duration::from_nanos)
        .filter(|value| !value.is_zero())
        .ok_or_else(|| "invalid media duration".into())
}

/// Discovers actual audio streams, including their original indices and format.
/// Missing semantic labels never become fabricated game/microphone identities.
pub fn discover(path: &Path) -> Result<Vec<AudioTrack>, String> {
    if !path.is_file() {
        return Err("the clip file could not be found".into());
    }
    let _init = trim::init().map_err(|error| error.to_string())?;
    let reader = trim::open_reader(path).map_err(|error| error.to_string())?;
    discover_reader(&reader)
}

fn discover_reader(reader: &IMFSourceReader) -> Result<Vec<AudioTrack>, String> {
    let duration = duration(reader)?;
    let mut tracks = Vec::new();
    // SAFETY: reader is thread-owned; stream enumeration ends at its documented
    // invalid-index error. All attribute access is read-only.
    unsafe {
        for stream_index in 0u32.. {
            let native = match reader.GetNativeMediaType(stream_index, 0) {
                Ok(native) => native,
                Err(error) if error.code() == MF_E_INVALIDSTREAMNUMBER => break,
                Err(error) => return Err(error.to_string()),
            };
            if native.GetMajorType().map_err(|error| error.to_string())? != MFMediaType_Audio {
                continue;
            }
            if tracks.len() >= MAX_TRACKS {
                return Err("more than 32 audio streams are unsupported".into());
            }
            let channels = native
                .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
                .map_err(|error| error.to_string())?;
            let sample_rate = native
                .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
                .map_err(|error| error.to_string())?;
            if channels == 0 || channels > 32 || sample_rate == 0 || sample_rate > 384_000 {
                return Err("invalid source audio format".into());
            }
            let label = reader
                .GetPresentationAttribute(stream_index, &MF_SD_STREAM_NAME)
                .ok()
                .and_then(|value| {
                    windows::core::BSTR::try_from(&value)
                        .ok()
                        .map(|text| text.to_string())
                })
                .map(|value| value.trim().chars().take(128).collect::<String>())
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            tracks.push(AudioTrack::new(
                stream_index,
                label,
                channels as u16,
                sample_rate,
                duration,
            ));
        }
    }
    let single = tracks.len() == 1;
    for (index, track) in tracks.iter_mut().enumerate() {
        if track.label.is_empty() {
            track.label = if single {
                "Audio (single stream)".into()
            } else {
                format!("Audio {}", index + 1)
            };
        }
    }
    Ok(tracks)
}

struct Pending {
    stream_index: u32,
    timestamp_ticks: i64,
    buffer: IMFMediaBuffer,
    offset: usize,
    frames: usize,
}

/// A synchronous source reader owned and used by its creating worker thread.
/// COM, decoder buffers, pending PCM and stream count all have explicit bounds.
pub struct Decoder {
    reader: IMFSourceReader,
    pending: HashMap<u32, Pending>,
    streams: HashSet<u32>,
    ended: HashSet<u32>,
    pub duration: Duration,
    _mf: MediaFoundation,
    _com: ComApartment,
}

impl Decoder {
    pub fn open(path: &Path, streams: &[u32]) -> Result<Self, String> {
        if !path.is_file() {
            return Err("the clip file could not be found".into());
        }
        if streams.is_empty() || streams.len() > MAX_TRACKS {
            return Err("select between 1 and 32 audio streams".into());
        }
        let (com, mf) = trim::init().map_err(|error| error.to_string())?;
        let reader = trim::open_reader(path).map_err(|error| error.to_string())?;
        let duration = duration(&reader)?;
        let available = discover_reader(&reader)?;
        let streams: HashSet<u32> = streams.iter().copied().collect();
        if streams
            .iter()
            .any(|index| !available.iter().any(|track| track.stream_index == *index))
        {
            return Err("a selected audio stream does not exist".into());
        }
        // SAFETY: reader and all newly created media types are thread-owned.
        // Windows 8+ Source Reader inserts its audio resampler for this complete
        // output type; callers get one common format regardless of source rate.
        unsafe {
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .map_err(|error| error.to_string())?;
            for &index in &streams {
                let media = MFCreateMediaType().map_err(|error| error.to_string())?;
                media
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                    .map_err(|error| error.to_string())?;
                media
                    .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)
                    .map_err(|error| error.to_string())?;
                media
                    .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(CHANNELS))
                    .map_err(|error| error.to_string())?;
                media
                    .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)
                    .map_err(|error| error.to_string())?;
                media
                    .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32)
                    .map_err(|error| error.to_string())?;
                media
                    .SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 8)
                    .map_err(|error| error.to_string())?;
                media
                    .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE * 8)
                    .map_err(|error| error.to_string())?;
                reader
                    .SetCurrentMediaType(index, None, &media)
                    .map_err(|error| format!("audio normalization failed: {error}"))?;
                validate_pcm(&reader, index)?;
                reader
                    .SetStreamSelection(index, true)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(Self {
            reader,
            pending: HashMap::new(),
            streams,
            ended: HashSet::new(),
            duration,
            _mf: mf,
            _com: com,
        })
    }

    pub fn seek(&mut self, at: Duration) -> Result<(), String> {
        let ticks = i64::try_from(at.min(self.duration).as_nanos() / 100)
            .map_err(|_| "audio seek overflow")?;
        self.pending.clear();
        self.ended.clear();
        // SAFETY: reader belongs to this worker; no asynchronous reads exist.
        unsafe {
            self.reader
                .Flush(MF_SOURCE_READER_ALL_STREAMS.0 as u32)
                .map_err(|error| error.to_string())?;
            for &stream in &self.streams {
                self.reader
                    .SetStreamSelection(stream, true)
                    .map_err(|error| error.to_string())?;
            }
            self.reader
                .SetCurrentPosition(&GUID::zeroed(), &ticks.into())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn next(&mut self, cancel: &AtomicBool) -> Result<Option<PcmChunk>, String> {
        self.next_cancelled(|| cancel.load(Ordering::Relaxed))
    }

    fn next_cancelled(&mut self, cancelled: impl Fn() -> bool) -> Result<Option<PcmChunk>, String> {
        let requested = self
            .pending
            .keys()
            .next()
            .copied()
            .unwrap_or(MF_SOURCE_READER_ANY_STREAM.0 as u32);
        self.read(requested, cancelled)
    }

    pub fn next_stream(
        &mut self,
        stream_index: u32,
        cancel: &AtomicBool,
    ) -> Result<Option<PcmChunk>, String> {
        if !self.streams.contains(&stream_index) {
            return Err("audio stream is not selected".into());
        }
        self.read(stream_index, || cancel.load(Ordering::Relaxed))
    }

    fn read(
        &mut self,
        requested: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<PcmChunk>, String> {
        for _ in 0..4096 {
            if cancelled() {
                return Err(CANCELLED.into());
            }
            let pending_index = if requested == MF_SOURCE_READER_ANY_STREAM.0 as u32 {
                self.pending.keys().next().copied()
            } else {
                Some(requested).filter(|index| self.pending.contains_key(index))
            };
            if let Some(index) = pending_index {
                let pending = self
                    .pending
                    .get_mut(&index)
                    .expect("existing pending audio");
                let result = pending_chunk(pending)?;
                if pending.offset >= pending.frames {
                    self.pending.remove(&index);
                }
                return Ok(Some(result));
            }
            if self.ended.len() == self.streams.len() || self.ended.contains(&requested) {
                return Ok(None);
            }
            let (mut stream, mut flags, mut timestamp, mut sample) = (0u32, 0u32, 0i64, None);
            // SAFETY: synchronous reads use live local out-pointers.
            unsafe {
                self.reader
                    .ReadSample(
                        requested,
                        0,
                        Some(&mut stream),
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .map_err(|error| error.to_string())?;
            }
            if !self.streams.contains(&stream) {
                return Err("decoder returned an unselected audio stream".into());
            }
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err("audio decoder failed".into());
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                validate_pcm(&self.reader, stream)?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.ended.insert(stream);
                // SAFETY: the stream index came from this reader.
                unsafe { self.reader.SetStreamSelection(stream, false) }
                    .map_err(|error| error.to_string())?;
            }
            if let Some(sample) = sample {
                // SAFETY: a decoded sample owns the returned contiguous buffer.
                let buffer = unsafe { sample.ConvertToContiguousBuffer() }
                    .map_err(|error| error.to_string())?;
                let bytes = unsafe { buffer.GetCurrentLength() }
                    .map_err(|error| error.to_string())? as usize;
                if bytes > MAX_PCM_BYTES || !bytes.is_multiple_of(8) {
                    return Err("invalid or oversized PCM sample".into());
                }
                let frames = bytes / 8;
                let offset = if timestamp < 0 {
                    ((-(timestamp as i128) * i128::from(SAMPLE_RATE) + 9_999_999) / 10_000_000)
                        as usize
                } else {
                    0
                };
                if offset >= frames {
                    continue;
                }
                self.pending.insert(
                    stream,
                    Pending {
                        stream_index: stream,
                        timestamp_ticks: timestamp,
                        buffer,
                        offset,
                        frames,
                    },
                );
            }
        }
        Err("audio decoder made no progress".into())
    }
}

fn validate_pcm(reader: &IMFSourceReader, stream: u32) -> Result<(), String> {
    // SAFETY: read-only attributes from the negotiated output media type.
    let valid = unsafe {
        let media = reader
            .GetCurrentMediaType(stream)
            .map_err(|error| error.to_string())?;
        media.GetMajorType().map_err(|error| error.to_string())? == MFMediaType_Audio
            && media
                .GetGUID(&MF_MT_SUBTYPE)
                .map_err(|error| error.to_string())?
                == MFAudioFormat_Float
            && media
                .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
                .map_err(|error| error.to_string())?
                == 2
            && media
                .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
                .map_err(|error| error.to_string())?
                == SAMPLE_RATE
            && media
                .GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE)
                .map_err(|error| error.to_string())?
                == 32
            && media
                .GetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT)
                .map_err(|error| error.to_string())?
                == 8
    };
    if valid {
        Ok(())
    } else {
        Err("audio decoder output format changed".into())
    }
}

fn pending_chunk(pending: &mut Pending) -> Result<PcmChunk, String> {
    let frames = (pending.frames - pending.offset).min(MAX_FRAMES);
    let (mut pointer, mut length) = (std::ptr::null_mut(), 0u32);
    // SAFETY: the buffer is locked only within this call. Length and aligned
    // byte slices are validated before access; no typed aligned pointer is used.
    unsafe { pending.buffer.Lock(&mut pointer, None, Some(&mut length)) }
        .map_err(|error| error.to_string())?;
    let result = if pointer.is_null() || (length as usize) < (pending.offset + frames) * 8 {
        Err("short PCM sample buffer".into())
    } else {
        let bytes =
            unsafe { std::slice::from_raw_parts(pointer.add(pending.offset * 8), frames * 8) };
        let samples = bytes
            .chunks_exact(4)
            .map(|bytes| {
                let value = f32::from_le_bytes(bytes.try_into().expect("four PCM bytes"));
                if value.is_finite() { value } else { 0.0 }
            })
            .collect();
        let base_ns = i128::from(pending.timestamp_ticks) * 100
            + pending.offset as i128 * 1_000_000_000 / i128::from(SAMPLE_RATE);
        Ok(PcmChunk {
            stream_index: pending.stream_index,
            timestamp: Duration::from_nanos(base_ns.max(0) as u64),
            samples,
        })
    };
    // SAFETY: this call balances the successful Lock above, even on validation errors.
    let unlocked = unsafe { pending.buffer.Unlock() }.map_err(|error| error.to_string());
    unlocked?;
    if result.is_ok() {
        pending.offset += frames;
    }
    result
}

fn source_frame(at: Duration) -> u64 {
    ((at.as_nanos() * u128::from(SAMPLE_RATE) + 500_000_000) / 1_000_000_000)
        .min(u128::from(u64::MAX)) as u64
}
fn frame_time(frame: u64) -> Duration {
    Duration::from_nanos(
        (u128::from(frame) * 1_000_000_000 / u128::from(SAMPLE_RATE)).min(u128::from(u64::MAX))
            as u64,
    )
}

struct MixTrack {
    decoder: Decoder,
    track: AudioTrack,
    gain: f32,
    chunk: Option<PcmChunk>,
    ended: bool,
}

/// Shared preview/export mixer. All edits are masks in source time; absent
/// samples are silence. Summed gain is clamped once after every track is mixed.
pub struct MixDecoder {
    configuration: Vec<AudioTrack>,
    tracks: Vec<MixTrack>,
    cursor: u64,
    end: u64,
}
impl MixDecoder {
    pub fn open(path: &Path, tracks: &[AudioTrack]) -> Result<Self, String> {
        let actual = discover(path)?;
        if tracks.len() > MAX_TRACKS
            || tracks.iter().enumerate().any(|(index, track)| {
                !actual
                    .iter()
                    .any(|candidate| candidate.stream_index == track.stream_index)
                    || tracks[..index]
                        .iter()
                        .any(|other| other.stream_index == track.stream_index)
            })
        {
            return Err("mix refers to missing or duplicate audio streams".into());
        }
        let info = trim::probe(path).map_err(|error| error.to_string())?;
        let audible: Vec<_> = tracks
            .iter()
            .filter(|track| {
                effective_gain(tracks, track.stream_index) > 0.0 && !track.segments.is_empty()
            })
            .map(|track| track.stream_index)
            .collect();
        let configuration = tracks.to_vec();
        // Each decoder selects only its own stream. A sparse track cannot
        // accumulate another track's demux packets inside a shared MF reader.
        let tracks = tracks
            .iter()
            .filter(|track| audible.contains(&track.stream_index))
            .map(|track| {
                Ok(MixTrack {
                    decoder: Decoder::open(path, &[track.stream_index])?,
                    track: track.clone(),
                    gain: effective_gain(tracks, track.stream_index),
                    chunk: None,
                    ended: false,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let end = info
            .duration
            .as_nanos()
            .saturating_mul(u128::from(SAMPLE_RATE))
            .div_ceil(1_000_000_000)
            .min(u128::from(u64::MAX)) as u64;
        Ok(Self {
            configuration,
            tracks,
            cursor: 0,
            end,
        })
    }
    /// Updates linear gains without reopening readers, discarding PCM, or
    /// changing the source cursor. Changes requiring a different decoder set or
    /// source masks return false without modifying the active mix.
    pub fn update_gains(&mut self, tracks: &[AudioTrack]) -> bool {
        if tracks.len() != self.configuration.len()
            || self.configuration.iter().any(|previous| {
                tracks
                    .iter()
                    .find(|track| track.stream_index == previous.stream_index)
                    .is_none_or(|track| track.segments != previous.segments)
            })
        {
            return false;
        }
        let audible: Vec<_> = tracks
            .iter()
            .filter(|track| {
                effective_gain(tracks, track.stream_index) > 0.0 && !track.segments.is_empty()
            })
            .map(|track| track.stream_index)
            .collect();
        if audible.len() != self.tracks.len()
            || self
                .tracks
                .iter()
                .any(|track| !audible.contains(&track.track.stream_index))
        {
            return false;
        }
        for track in &mut self.tracks {
            track.gain = effective_gain(tracks, track.track.stream_index);
        }
        true
    }

    pub fn seek(&mut self, at: Duration) -> Result<(), String> {
        self.cursor = at
            .as_nanos()
            .saturating_mul(u128::from(SAMPLE_RATE))
            .div_ceil(1_000_000_000)
            .min(u128::from(self.end)) as u64;
        for track in &mut self.tracks {
            track.chunk = None;
            track.ended = false;
            track.decoder.seek(frame_time(self.cursor))?;
        }
        Ok(())
    }
    pub fn next(
        &mut self,
        max_frames: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<PcmChunk>, String> {
        if max_frames == 0 {
            return Err("audio block length must be positive".into());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        if self.cursor >= self.end {
            return Ok(None);
        }
        let frames = (self.end - self.cursor).min(max_frames.min(MAX_FRAMES) as u64) as usize;
        let mut samples = vec![0.0f32; frames * 2];
        for track in &mut self.tracks {
            let mut output = 0usize;
            while output < frames {
                if cancel.load(Ordering::Relaxed) {
                    return Err(CANCELLED.into());
                }
                let at = self.cursor + output as u64;
                if let Some(chunk) = &track.chunk {
                    let start = source_frame(chunk.timestamp);
                    let end = start + chunk.samples.len() as u64 / 2;
                    if end <= at {
                        track.chunk = None;
                        continue;
                    }
                    if start > at {
                        output += (start - at).min((frames - output) as u64) as usize;
                        continue;
                    }
                    let source_offset = (at - start) as usize;
                    let count = (chunk.samples.len() / 2 - source_offset).min(frames - output);
                    for offset in 0..count {
                        if track.track.contains(frame_time(at + offset as u64)) {
                            for channel in 0..2 {
                                samples[(output + offset) * 2 + channel] += chunk.samples
                                    [(source_offset + offset) * 2 + channel]
                                    * track.gain;
                            }
                        }
                    }
                    output += count;
                } else if track.ended {
                    break;
                } else {
                    track.chunk = track.decoder.next(cancel)?;
                    track.ended = track.chunk.is_none();
                }
            }
        }
        for sample in &mut samples {
            *sample = sample.clamp(-1.0, 1.0);
        }
        let chunk = PcmChunk {
            stream_index: u32::MAX,
            timestamp: frame_time(self.cursor),
            samples,
        };
        self.cursor += frames as u64;
        Ok(Some(chunk))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WaveformBucket {
    pub peak: f32,
    pub rms: f32,
}
#[derive(Clone, Debug)]
pub struct TrackWaveform {
    pub stream_index: u32,
    pub buckets: Vec<WaveformBucket>,
}
#[derive(Clone, Debug)]
pub struct WaveformSet {
    pub duration: Duration,
    pub tracks: Vec<TrackWaveform>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}
impl Fingerprint {
    fn read(path: &Path) -> Result<Self, String> {
        let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("the clip file could not be found".into());
        }
        Ok(Self {
            path: canonical_media_path(path)?,
            bytes: metadata.len(),
            modified: metadata.modified().map_err(|error| error.to_string())?,
        })
    }
}
// MF URL readers do not accept Windows verbatim path prefixes. Keep the
// canonical identity while removing only that transport prefix, preserving UTF-16.
fn canonical_media_path(path: &Path) -> Result<PathBuf, String> {
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    let mut wide: Vec<_> = canonical.as_os_str().encode_wide().collect();
    if wide.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        wide.drain(..8);
        wide.splice(..0, [92, 92]);
    } else if wide.starts_with(&[92, 92, 63, 92]) {
        wide.drain(..4);
    }
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
}

type Cache = VecDeque<(Fingerprint, Arc<WaveformSet>)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct Worker;
impl Drop for Worker {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Extracts actual peak/RMS buckets once per fingerprint. At most four files and
/// four workers are retained. Zoom only reads these cached source-time buckets.
/// Dropping the receiver cancels before the next PCM read; channel capacity is 2.
pub fn waveform(path: &Path) -> async_channel::Receiver<Result<Arc<WaveformSet>, String>> {
    let (sender, receiver) = async_channel::bounded(2);
    let fingerprint = match Fingerprint::read(path) {
        Ok(value) => value,
        Err(error) => {
            let _ = sender.try_send(Err(error));
            return receiver;
        }
    };
    if let Some(cached) = cached(&fingerprint) {
        let _ = sender.try_send(Ok(cached));
        return receiver;
    }
    if WORKERS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |workers| {
            (workers < 4).then_some(workers + 1)
        })
        .is_err()
    {
        let _ = sender.try_send(Err("too many active waveform requests".into()));
        return receiver;
    }
    let worker = Worker;
    let error_sender = sender.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("clip-waveforms".into())
        .spawn(move || {
            let _worker = worker;
            if sender.is_closed() {
                return;
            }
            let result = generate(&fingerprint, || sender.is_closed());
            if let Ok(waveform) = &result
                && !sender.is_closed()
            {
                let mut cache = CACHE
                    .get_or_init(|| Mutex::new(VecDeque::new()))
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                cache_store(&mut cache, fingerprint, waveform.clone());
            }
            let _ = sender.send_blocking(result);
        })
    {
        let _ = error_sender.try_send(Err(format!("could not start waveform extraction: {error}")));
    }
    receiver
}
fn cache_store(cache: &mut Cache, fingerprint: Fingerprint, waveform: Arc<WaveformSet>) {
    cache.retain(|(key, _)| key.path != fingerprint.path);
    cache.push_back((fingerprint, waveform));
    while cache.len() > CACHE_FILES {
        cache.pop_front();
    }
}

fn cached(fingerprint: &Fingerprint) -> Option<Arc<WaveformSet>> {
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let index = cache.iter().position(|(key, _)| key == fingerprint)?;
    let entry = cache.remove(index)?;
    let result = entry.1.clone();
    cache.push_back(entry);
    Some(result)
}
#[derive(Clone, Default)]
struct Accumulator {
    peak: f32,
    squares: f64,
    count: u64,
}
fn generate(
    fingerprint: &Fingerprint,
    cancelled: impl Fn() -> bool,
) -> Result<Arc<WaveformSet>, String> {
    let tracks = discover(&fingerprint.path)?;
    let info = trim::probe(&fingerprint.path).map_err(|error| error.to_string())?;
    let count = (info.duration.as_secs_f64() * 100.0)
        .ceil()
        .clamp(1.0, MAX_BUCKETS as f64) as usize;
    let mut values = vec![vec![Accumulator::default(); count]; tracks.len()];
    if !tracks.is_empty() {
        let indices: Vec<_> = tracks.iter().map(|track| track.stream_index).collect();
        let mut decoder = Decoder::open(&fingerprint.path, &indices)?;
        loop {
            if cancelled() {
                return Err(CANCELLED.into());
            }
            let Some(chunk) = decoder.next_cancelled(&cancelled)? else {
                break;
            };
            let track_index = indices
                .iter()
                .position(|index| *index == chunk.stream_index)
                .ok_or("unknown waveform stream")?;
            for (offset, frame) in chunk.samples.chunks_exact(2).enumerate() {
                let at = chunk.timestamp.as_nanos()
                    + offset as u128 * 1_000_000_000 / u128::from(SAMPLE_RATE);
                if at >= info.duration.as_nanos() {
                    continue;
                }
                let bucket = (at * count as u128 / info.duration.as_nanos()) as usize;
                let accumulator = &mut values[track_index][bucket];
                for &sample in frame {
                    accumulator.peak = accumulator.peak.max(sample.abs());
                    accumulator.squares += f64::from(sample) * f64::from(sample);
                    accumulator.count += 1;
                }
            }
        }
    }
    if cancelled() {
        return Err(CANCELLED.into());
    }
    if Fingerprint::read(&fingerprint.path)? != *fingerprint {
        return Err("clip changed during waveform extraction".into());
    }
    Ok(Arc::new(WaveformSet {
        duration: info.duration,
        tracks: tracks
            .into_iter()
            .zip(values)
            .map(|(track, values)| TrackWaveform {
                stream_index: track.stream_index,
                buckets: values
                    .into_iter()
                    .map(|value| WaveformBucket {
                        peak: value.peak,
                        rms: if value.count == 0 {
                            0.0
                        } else {
                            (value.squares / value.count as f64).sqrt() as f32
                        },
                    })
                    .collect(),
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trim::tests::{temp_dir, write_test_clip};

    #[test]
    fn waveform_cache_evicts_old_files_and_replaces_changed_identity() {
        let mut cache = Cache::new();
        let empty = Arc::new(WaveformSet {
            duration: Duration::from_secs(1),
            tracks: Vec::new(),
        });
        for index in 0..8 {
            cache_store(
                &mut cache,
                Fingerprint {
                    path: PathBuf::from(format!("{index}.mp4")),
                    bytes: 10,
                    modified: SystemTime::UNIX_EPOCH,
                },
                empty.clone(),
            );
        }
        assert_eq!(cache.len(), CACHE_FILES);
        assert_eq!(cache.front().unwrap().0.path, PathBuf::from("4.mp4"));
        cache_store(
            &mut cache,
            Fingerprint {
                path: PathBuf::from("6.mp4"),
                bytes: 20,
                modified: SystemTime::UNIX_EPOCH,
            },
            empty,
        );
        assert_eq!(cache.len(), CACHE_FILES);
        assert_eq!(cache.back().unwrap().0.bytes, 20);
        assert_eq!(
            cache
                .iter()
                .filter(|(key, _)| key.path == Path::new("6.mp4"))
                .count(),
            1
        );
    }

    fn write_mono_wave(path: &Path, rate: u32) {
        let frames = rate / 5;
        let bytes = frames * 2;
        let mut wave = Vec::new();
        wave.extend_from_slice(b"RIFF");
        wave.extend_from_slice(&(36 + bytes).to_le_bytes());
        wave.extend_from_slice(b"WAVEfmt ");
        wave.extend_from_slice(&16u32.to_le_bytes());
        wave.extend_from_slice(&1u16.to_le_bytes());
        wave.extend_from_slice(&1u16.to_le_bytes());
        wave.extend_from_slice(&rate.to_le_bytes());
        wave.extend_from_slice(&(rate * 2).to_le_bytes());
        wave.extend_from_slice(&2u16.to_le_bytes());
        wave.extend_from_slice(&16u16.to_le_bytes());
        wave.extend_from_slice(b"data");
        wave.extend_from_slice(&bytes.to_le_bytes());
        for frame in 0..frames {
            let value = ((frame as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin()
                * 8192.0) as i16;
            wave.extend_from_slice(&value.to_le_bytes());
        }
        std::fs::write(path, wave).unwrap();
    }

    fn write_sparse_audio(path: &Path) {
        let _init = trim::init().unwrap();
        let destination = trim::wide(path);
        // SAFETY: every media type and buffer belongs to this test thread; all
        // sample buffers are locked for their exact allocation then unlocked.
        unsafe {
            let writer = MFCreateSinkWriterFromURL(
                windows::core::PCWSTR(destination.as_ptr()),
                None::<&IMFByteStream>,
                None::<&IMFAttributes>,
            )
            .unwrap();
            let mut streams = Vec::new();
            for _ in 0..2 {
                let output = MFCreateMediaType().unwrap();
                output
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                    .unwrap();
                output.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).unwrap();
                output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).unwrap();
                output
                    .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)
                    .unwrap();
                output.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).unwrap();
                output
                    .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 16_000)
                    .unwrap();
                output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0).unwrap();
                let index = writer.AddStream(&output).unwrap();
                let input = MFCreateMediaType().unwrap();
                input
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                    .unwrap();
                input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).unwrap();
                input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).unwrap();
                input
                    .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)
                    .unwrap();
                input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).unwrap();
                input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4).unwrap();
                input
                    .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE * 4)
                    .unwrap();
                writer
                    .SetInputMediaType(index, &input, None::<&IMFAttributes>)
                    .unwrap();
                streams.push(index);
            }
            writer.BeginWriting().unwrap();
            for block in 0..100 {
                for (position, &stream) in streams.iter().enumerate() {
                    if position == 0 && (10..80).contains(&block) {
                        continue;
                    }
                    let mut bytes = Vec::with_capacity(960 * 4);
                    for frame in 0..960 {
                        let source = block * 960 + frame;
                        let value = ((source as f32 * 330.0 * std::f32::consts::TAU
                            / SAMPLE_RATE as f32)
                            .sin()
                            * 4096.0) as i16;
                        bytes.extend_from_slice(&value.to_le_bytes());
                        bytes.extend_from_slice(&value.to_le_bytes());
                    }
                    let sample = MFCreateSample().unwrap();
                    let buffer = MFCreateMemoryBuffer(bytes.len() as u32).unwrap();
                    let mut pointer = std::ptr::null_mut();
                    buffer.Lock(&mut pointer, None, None).unwrap();
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
                    buffer.Unlock().unwrap();
                    buffer.SetCurrentLength(bytes.len() as u32).unwrap();
                    sample.AddBuffer(&buffer).unwrap();
                    sample.SetSampleTime(i64::from(block) * 200_000).unwrap();
                    sample.SetSampleDuration(200_000).unwrap();
                    writer.WriteSample(stream, &sample).unwrap();
                }
            }
            writer.Finalize().unwrap();
        }
    }

    #[test]
    fn sparse_streams_have_isolated_readers_and_masks_do_not_stop_progress() {
        let dir = temp_dir("audio-sparse");
        let path = dir.join("sparse.mp4");
        write_sparse_audio(&path);
        let mut tracks = discover(&path).unwrap();
        // MP4 readers may reorder otherwise-identical audio streams. Find the
        // genuinely sparse stream by its timestamps instead of assuming order.
        let sparse_index = tracks
            .iter()
            .position(|track| {
                let mut decoder = Decoder::open(&path, &[track.stream_index]).unwrap();
                decoder.seek(Duration::from_millis(500)).unwrap();
                (0..3)
                    .filter_map(|_| decoder.next(&AtomicBool::new(false)).unwrap())
                    .any(|chunk| chunk.timestamp > Duration::from_secs(1))
            })
            .expect("fixture contains a timestamp gap");
        tracks.swap(0, sparse_index);
        let mut mix = MixDecoder::open(&path, &tracks).unwrap();
        assert_eq!(mix.tracks.len(), 2);
        assert!(
            mix.tracks
                .iter()
                .all(|track| track.decoder.streams.len() == 1)
        );
        let cancel = AtomicBool::new(false);
        mix.seek(Duration::from_millis(500)).unwrap();
        let gap = mix.next(1024, &cancel).unwrap().unwrap();
        assert!(gap.samples.iter().any(|value| value.abs() > 0.05));
        tracks[1].enabled = false;
        let mut sparse = MixDecoder::open(&path, &tracks).unwrap();
        sparse.seek(Duration::from_millis(500)).unwrap();
        let gap = sparse.next(1024, &cancel).unwrap().unwrap();
        assert!(gap.samples.iter().all(|value| value.abs() < 0.001));
        tracks[0].segments = vec![clipper_editor::Segment {
            start: Duration::from_millis(1600),
            end: Duration::from_millis(1900),
        }];
        let mut masked = MixDecoder::open(&path, &tracks).unwrap();
        let mut resumed = false;
        while let Some(chunk) = masked.next(1024, &cancel).unwrap() {
            if chunk.timestamp < Duration::from_millis(1500) {
                assert!(chunk.samples.iter().all(|value| *value == 0.0));
            }
            if chunk.timestamp >= Duration::from_millis(1700)
                && chunk.samples.iter().any(|value| value.abs() > 0.05)
            {
                resumed = true;
            }
        }
        assert!(resumed);
    }

    #[test]
    fn gain_updates_preserve_decoders_buffers_and_cursor_but_reject_mask_changes() {
        use windows::core::Interface;
        let dir = temp_dir("audio-update-gain");
        let path = dir.join("mono.wav");
        write_mono_wave(&path, 48_000);
        let mut tracks = discover(&path).unwrap();
        let mut mix = MixDecoder::open(&path, &tracks).unwrap();
        let reader = mix.tracks[0].decoder.reader.as_raw();
        let cancel = AtomicBool::new(false);
        mix.next(256, &cancel).unwrap().unwrap();
        tracks[0].gain_percent = 150;
        tracks[0].selected_segment = None;
        assert!(mix.update_gains(&tracks));
        assert_eq!(mix.tracks[0].decoder.reader.as_raw(), reader);
        assert_eq!(mix.cursor, 256);
        assert_eq!(mix.tracks[0].gain, 1.5);
        assert_eq!(
            mix.next(256, &cancel).unwrap().unwrap().timestamp,
            frame_time(256)
        );
        tracks[0].gain_percent = 0;
        assert!(!mix.update_gains(&tracks));
        assert_eq!(mix.tracks[0].gain, 1.5);
        tracks[0].gain_percent = 150;
        tracks[0].segments.clear();
        assert!(!mix.update_gains(&tracks));
        assert_eq!(mix.tracks[0].decoder.reader.as_raw(), reader);
    }

    #[test]
    fn mono_44100_resamples_to_stereo_48000_and_can_seek_after_eof() {
        let dir = temp_dir("audio-resample");
        let path = dir.join("mono.wav");
        write_mono_wave(&path, 44_100);
        let tracks = discover(&path).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].channels, 1);
        assert_eq!(tracks[0].sample_rate, 44_100);
        assert_eq!(tracks[0].label, "Audio (single stream)");
        let cancel = AtomicBool::new(false);
        let mut decoder = Decoder::open(&path, &[tracks[0].stream_index]).unwrap();
        let mut frames = 0;
        while let Some(chunk) = decoder.next(&cancel).unwrap() {
            assert!(
                chunk
                    .samples
                    .chunks_exact(2)
                    .all(|sample| (sample[0] - sample[1]).abs() < 0.0001)
            );
            frames += chunk.samples.len() / 2;
        }
        assert!(
            (9500..=9700).contains(&frames),
            "resampled frames: {frames}"
        );
        decoder.seek(Duration::ZERO).unwrap();
        assert!(decoder.next(&cancel).unwrap().is_some());
    }

    #[test]
    fn discovery_and_pcm_retain_actual_stream_indices_and_timestamps() {
        let dir = temp_dir("audio-discovery");
        let path = dir.join("audio.mp4");
        write_test_clip(&path, 2, 2);
        let tracks = discover(&path).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_ne!(tracks[0].stream_index, tracks[1].stream_index);
        let _init = trim::init().unwrap();
        let reader = trim::open_reader(&path).unwrap();
        for track in &tracks {
            // SAFETY: read-only native type from this reader at a discovered index.
            assert_eq!(
                unsafe {
                    reader
                        .GetNativeMediaType(track.stream_index, 0)
                        .unwrap()
                        .GetMajorType()
                        .unwrap()
                },
                MFMediaType_Audio
            );
        }
        assert!(
            tracks
                .iter()
                .all(|track| track.channels == 2 && track.sample_rate == 48_000)
        );
        let mut decoder = Decoder::open(&path, &[tracks[1].stream_index]).unwrap();
        decoder.seek(Duration::from_millis(500)).unwrap();
        let chunk = decoder.next(&AtomicBool::new(false)).unwrap().unwrap();
        assert_eq!(chunk.stream_index, tracks[1].stream_index);
        assert!(chunk.samples.len() <= MAX_FRAMES * 2);
        assert!(chunk.samples.len().is_multiple_of(2));
        assert!(chunk.samples.iter().all(|value| value.is_finite()));
        assert!(chunk.timestamp < Duration::from_secs(1));
        assert!(decoder.next(&AtomicBool::new(true)).is_err());
    }

    #[test]
    fn missing_corrupt_and_no_audio_have_explicit_results() {
        let dir = temp_dir("audio-invalid");
        assert!(discover(&dir.join("missing.mp4")).is_err());
        let corrupt = dir.join("corrupt.mp4");
        std::fs::write(&corrupt, b"not media").unwrap();
        assert!(discover(&corrupt).is_err());
        let path = dir.join("silent.mp4");
        write_test_clip(&path, 1, 0);
        assert!(discover(&path).unwrap().is_empty());
        assert!(Decoder::open(&path, &[1]).is_err());
        let result = waveform(&path).recv_blocking().unwrap().unwrap();
        assert!(result.tracks.is_empty());
    }

    #[test]
    fn waveform_contains_real_peak_rms_and_cache_reuses_fingerprint() {
        let dir = temp_dir("audio-waveform");
        let path = dir.join("audio.mp4");
        write_test_clip(&path, 1, 2);
        let receiver = waveform(&path);
        assert_eq!(receiver.capacity(), Some(2));
        let first = receiver.recv_blocking().unwrap().unwrap();
        assert_eq!(first.tracks.len(), 2);
        for track in &first.tracks {
            assert!(!track.buckets.is_empty() && track.buckets.len() <= MAX_BUCKETS);
            assert!(
                track
                    .buckets
                    .iter()
                    .any(|bucket| bucket.peak > 0.1 && bucket.rms > 0.05)
            );
            assert!(
                track
                    .buckets
                    .iter()
                    .all(|bucket| bucket.peak.is_finite() && bucket.rms <= bucket.peak + 0.0001)
            );
        }
        let second = waveform(&path).recv_blocking().unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        std::fs::write(&path, b"changed fingerprint").unwrap();
        assert!(waveform(&path).recv_blocking().unwrap().is_err());
    }

    #[test]
    fn mixed_pcm_uses_exact_seek_gain_masks_and_silence() {
        let dir = temp_dir("audio-mix");
        let path = dir.join("audio.mp4");
        write_test_clip(&path, 1, 2);
        let mut tracks = discover(&path).unwrap();
        tracks[1].enabled = false;
        let cancel = AtomicBool::new(false);
        let start = Duration::from_millis(123);
        let mut single = MixDecoder::open(&path, &tracks).unwrap();
        single.seek(start).unwrap();
        let one = single.next(1024, &cancel).unwrap().unwrap();
        assert!(one.timestamp.abs_diff(start) <= Duration::from_micros(21));
        tracks[0].gain_percent = 200;
        let mut double = MixDecoder::open(&path, &tracks).unwrap();
        double.seek(start).unwrap();
        let two = double.next(1024, &cancel).unwrap().unwrap();
        assert_eq!(one.samples.len(), two.samples.len());
        assert!(
            one.samples
                .iter()
                .zip(&two.samples)
                .all(|(a, b)| (a * 2.0 - b).abs() < 0.0001)
        );
        tracks[0].segments.clear();
        let mut silent = MixDecoder::open(&path, &tracks).unwrap();
        silent.seek(start).unwrap();
        let silence = silent.next(usize::MAX, &cancel).unwrap().unwrap();
        assert_eq!(silence.samples.len(), MAX_FRAMES * 2);
        assert!(silence.samples.iter().all(|&value| value == 0.0));
        assert!(silent.next(0, &cancel).is_err());
        silent.seek(Duration::from_secs(2)).unwrap();
        assert!(silent.next(100, &cancel).unwrap().is_none());
    }
}
