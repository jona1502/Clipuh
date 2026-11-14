//! Non-destructive clip trimming: stream-copies a time range of an MP4 into a
//! new MP4 with Media Foundation (source reader of compressed samples into a
//! sink writer using the same native types), so nothing is re-encoded.
#![allow(unsafe_code)]

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clipper_encoder::backends::media_foundation::MediaFoundation;
use clipper_platform::windows::ComApartment;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, PCWSTR};

const MIN_LEN: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq)]
pub struct MediaInfo {
    pub duration: Duration,
    pub width: u32,
    pub height: u32,
    pub audio_tracks: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrimOutcome {
    pub path: PathBuf,
    /// Actual start in the source (keyframe at or before the request).
    pub start: Duration,
    pub duration: Duration,
}

#[derive(Debug)]
pub enum TrimError {
    SourceMissing,
    InvalidRange,
    /// Destination equals source or already exists.
    DestinationExists,
    Cancelled,
    Media(String),
    Io(std::io::Error),
}

impl std::fmt::Display for TrimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceMissing => f.write_str("the clip file could not be found"),
            Self::InvalidRange => f.write_str("the selected range is not valid"),
            Self::DestinationExists => f.write_str("a file with that name already exists"),
            Self::Cancelled => f.write_str("the export was cancelled"),
            Self::Media(detail) => write!(f, "the clip could not be processed: {detail}"),
            Self::Io(error) => write!(f, "file error: {error}"),
        }
    }
}

impl std::error::Error for TrimError {}

impl From<windows::core::Error> for TrimError {
    fn from(error: windows::core::Error) -> Self {
        Self::Media(error.to_string())
    }
}

impl From<std::io::Error> for TrimError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn ticks(d: Duration) -> i64 {
    (d.as_nanos() / 100) as i64
}

fn duration_of(ticks: i64) -> Duration {
    Duration::from_nanos(ticks.max(0) as u64 * 100)
}

/// COM + Media Foundation for the current thread (both are ref-counted).
pub(crate) fn init() -> Result<(ComApartment, MediaFoundation), TrimError> {
    let com = ComApartment::init_multithreaded().map_err(TrimError::from)?;
    let mf = MediaFoundation::start().map_err(|e| TrimError::Media(e.to_string()))?;
    Ok((com, mf))
}

pub(crate) fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

pub(crate) fn open_reader(path: &Path) -> Result<IMFSourceReader, TrimError> {
    let wide = wide(path);
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    Ok(unsafe { MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), None) }?)
}

/// Native (compressed) type of every stream, in reader stream order.
fn native_types(reader: &IMFSourceReader) -> Result<Vec<IMFMediaType>, TrimError> {
    let mut types = Vec::new();
    for index in 0u32.. {
        // SAFETY: valid reader; an out-of-range index returns an error.
        match unsafe { reader.GetNativeMediaType(index, 0) } {
            Ok(t) => types.push(t),
            Err(e) if e.code() == MF_E_INVALIDSTREAMNUMBER => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(types)
}

fn major(t: &IMFMediaType) -> GUID {
    // SAFETY: valid media type.
    unsafe { t.GetMajorType() }.unwrap_or_default()
}

/// Reads duration, video size and audio track count without decoding.
pub fn probe(path: &Path) -> Result<MediaInfo, TrimError> {
    if !path.is_file() {
        return Err(TrimError::SourceMissing);
    }
    let _init = init()?;
    let reader = open_reader(path)?;
    // SAFETY: valid reader; the PROPVARIANT is owned and freed on drop.
    let value: PROPVARIANT = unsafe {
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
    }?;
    let duration = Duration::from_nanos(u64::try_from(&value).map_err(TrimError::from)? * 100);
    let types = native_types(&reader)?;
    let (mut width, mut height) = (0, 0);
    if let Some(video) = types.iter().find(|t| major(t) == MFMediaType_Video) {
        // SAFETY: valid media type.
        let size = unsafe { video.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
        (width, height) = ((size >> 32) as u32, size as u32);
    }
    let audio_tracks = types
        .iter()
        .filter(|t| major(t) == MFMediaType_Audio)
        .count();
    Ok(MediaInfo {
        duration,
        width,
        height,
        audio_tracks,
    })
}

/// `<stem> (trim 00m12s-00m45s).mp4` next to the source; adds ` 2`, ` 3`, ...
/// until the path doesn't exist. Never returns `source`.
pub fn export_path(source: &Path, start: Duration, end: Duration) -> PathBuf {
    let stem = source
        .file_stem()
        .map_or_else(|| "clip".into(), |s| s.to_string_lossy());
    let mmss = |d: Duration| format!("{:02}m{:02}s", d.as_secs() / 60, d.as_secs() % 60);
    let base = format!("{stem} (trim {}-{})", mmss(start), mmss(end));
    let dir = source.parent().unwrap_or(Path::new(""));
    let mut candidate = dir.join(format!("{base}.mp4"));
    let mut n = 2;
    while candidate == source || candidate.exists() {
        candidate = dir.join(format!("{base} {n}.mp4"));
        n += 1;
    }
    candidate
}

/// Copies `[start, end)` of `source` into `destination` without re-encoding.
/// Blocking: callers run it on a background thread. `progress` receives
/// 0.0..=1.0 (at most about every 1%); `cancel` is polled per sample.
///
/// Accuracy: the start snaps back to the previous keyframe (Clipper encodes a
/// keyframe every 2 s), so the export may begin up to ~2 s before the chosen
/// in-point. The end is frame-accurate without B-frames; with B-frames (the
/// Quality preset) it can end up to two frames early, because cutting at the
/// first frame decoded past the out-point never leaves a frame without its
/// reference. The real start is in the outcome.
pub fn trim(
    source: &Path,
    destination: &Path,
    start: Duration,
    end: Duration,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<TrimOutcome, TrimError> {
    if !source.is_file() {
        return Err(TrimError::SourceMissing);
    }
    let info = probe(source)?;
    // Players and the container may report the duration a few ms apart, so
    // an out-point at "the end" is clamped instead of rejected.
    let end = end.min(info.duration);
    if start >= end || end - start < MIN_LEN {
        return Err(TrimError::InvalidRange);
    }
    let same = match (source.canonicalize(), destination.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => source == destination,
    };
    if same || destination.exists() {
        return Err(TrimError::DestinationExists);
    }
    let mut partial = destination.as_os_str().to_owned();
    partial.push(".partial.mp4");
    let partial = PathBuf::from(partial);

    let result =
        copy_range(source, &partial, start, end, progress, cancel, &info).and_then(|start_at| {
            // `rename` replaces an existing file; one may have appeared meanwhile.
            if destination.exists() {
                return Err(TrimError::DestinationExists);
            }
            std::fs::rename(&partial, destination)?;
            Ok(TrimOutcome {
                path: destination.to_owned(),
                start: start_at,
                duration: end - start_at,
            })
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

/// The sample's decode timestamp, present on streams with B-frames.
fn decode_time(sample: &IMFSample) -> Option<i64> {
    // SAFETY: valid sample; a missing attribute is an error, mapped to None.
    unsafe { sample.GetUINT64(&MFSampleExtension_DecodeTimestamp) }
        .ok()
        .map(|dts| dts as i64)
}

/// Stream-copies into `partial`, verifies it, and returns the actual start.
fn copy_range(
    source: &Path,
    partial: &Path,
    start: Duration,
    end: Duration,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
    info: &MediaInfo,
) -> Result<Duration, TrimError> {
    let _init = init()?;
    let reader = open_reader(source)?;
    let types = native_types(&reader)?;
    let video = types
        .iter()
        .position(|t| major(t) == MFMediaType_Video)
        .ok_or_else(|| TrimError::Media("the clip has no video".into()))? as u32;
    let all = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
    let any = MF_SOURCE_READER_ANY_STREAM.0 as u32;
    let end_ticks = ticks(end);

    // Pass 1: video only, to find the keyframe the seek lands on.
    // SAFETY: valid reader and arguments; out-pointers reference live locals.
    let (t0, key_dts) = unsafe {
        reader.SetStreamSelection(all, false)?;
        reader.SetStreamSelection(video, true)?;
        reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(ticks(start)))?;
        loop {
            let (mut flags, mut time, mut sample) = (0u32, 0i64, None);
            reader.ReadSample(
                video,
                0,
                None,
                Some(&mut flags),
                Some(&mut time),
                Some(&mut sample),
            )?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Err(TrimError::InvalidRange);
            }
            let key: Option<IMFSample> = sample;
            if let Some(key) =
                key.filter(|s| s.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 1)
            {
                break (time, decode_time(&key));
            }
        }
    };
    if t0 >= end_ticks {
        return Err(TrimError::InvalidRange);
    }
    // With B-frames the keyframe decodes before it is shown; rebasing on its
    // decode time keeps every decode and presentation time non-negative.
    let base = key_dts.map_or(t0, |dts| dts.min(t0));

    let mut attributes = None;
    // SAFETY: out-pointer valid; scalar attributes set on the fresh object.
    let writer: IMFSinkWriter = unsafe {
        MFCreateAttributes(&mut attributes, 2)?;
        let attributes = attributes.ok_or_else(|| TrimError::Media("no attributes".into()))?;
        attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        let path = wide(partial);
        MFCreateSinkWriterFromURL(PCWSTR(path.as_ptr()), None::<&IMFByteStream>, &attributes)?
    };
    for (index, native) in types.iter().enumerate() {
        // SAFETY: valid writer and type; passthrough uses the same type in and out.
        unsafe {
            let stream = writer.AddStream(native)?;
            debug_assert_eq!(stream as usize, index);
            writer.SetInputMediaType(stream, native, None::<&IMFAttributes>)?;
        }
    }
    // SAFETY: configured writer; reader reselected and re-seeked to the keyframe.
    unsafe {
        writer.BeginWriting()?;
        reader.SetStreamSelection(all, true)?;
        reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(t0))?;
    }

    let mut done = vec![false; types.len()];
    let mut last_percent = -1i32;
    while done.iter().any(|d| !d) {
        if cancel.load(Ordering::Relaxed) {
            return Err(TrimError::Cancelled);
        }
        let (mut index, mut flags, mut time) = (0u32, 0u32, 0i64);
        let mut sample: Option<IMFSample> = None;
        // SAFETY: out-pointers reference live locals.
        unsafe {
            reader.ReadSample(
                any,
                0,
                Some(&mut index),
                Some(&mut flags),
                Some(&mut time),
                Some(&mut sample),
            )?;
        }
        let slot = index as usize;
        if slot >= done.len() || done[slot] {
            continue;
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0
            || (time >= end_ticks && sample.is_some())
        {
            done[slot] = true;
            // SAFETY: valid stream index; stops further reads of this stream.
            unsafe { reader.SetStreamSelection(index, false)? };
            continue;
        }
        // Samples before the base time are audio lead-in.
        let Some(sample) = sample.filter(|_| time >= t0) else {
            continue;
        };
        // SAFETY: valid sample and writer.
        unsafe {
            sample.SetSampleTime(time - base)?;
            if let Some(dts) = decode_time(&sample) {
                sample.SetUINT64(
                    &MFSampleExtension_DecodeTimestamp,
                    (dts - base).max(0) as u64,
                )?;
            }
            writer.WriteSample(index, &sample)?;
        }
        if index == video {
            let percent = ((time - t0) as f64 * 100.0 / (end_ticks - t0) as f64) as i32;
            if percent > last_percent {
                last_percent = percent;
                progress((percent as f32 / 100.0).min(1.0));
            }
        }
    }
    // SAFETY: all samples written.
    unsafe { writer.Finalize()? };
    drop(writer);
    progress(1.0);

    let out = probe(partial)?;
    if out.duration.is_zero() || out.width == 0 || out.audio_tracks != info.audio_tracks {
        return Err(TrimError::Media(
            "the exported file failed verification".into(),
        ));
    }
    Ok(duration_of(t0))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const FPS: u32 = 30;
    const SECOND: i64 = 10_000_000;

    fn pack(hi: u32, lo: u32) -> u64 {
        (u64::from(hi) << 32) | u64::from(lo)
    }

    fn make_sample(data: &[u8], time: i64, duration: i64) -> windows::core::Result<IMFSample> {
        // SAFETY: the buffer is locked for exactly `data.len()` bytes and unlocked.
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            Ok(sample)
        }
    }

    fn av_type(major: &GUID, subtype: &GUID) -> IMFMediaType {
        // SAFETY: plain FFI on a fresh media type.
        unsafe {
            let t = MFCreateMediaType().unwrap();
            t.SetGUID(&MF_MT_MAJOR_TYPE, major).unwrap();
            t.SetGUID(&MF_MT_SUBTYPE, subtype).unwrap();
            if *major == MFMediaType_Video {
                t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                    .unwrap();
                t.SetUINT64(&MF_MT_FRAME_SIZE, pack(320, 240)).unwrap();
                t.SetUINT64(&MF_MT_FRAME_RATE, pack(FPS, 1)).unwrap();
                t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).unwrap();
            } else {
                t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).unwrap();
                t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)
                    .unwrap();
                t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).unwrap();
            }
            t
        }
    }

    /// Encodes a synthetic 320x240 H.264 clip with `audio_tracks` AAC sine tracks.
    pub(crate) fn write_test_clip(path: &Path, seconds: u32, audio_tracks: usize) {
        let _init = init().unwrap();
        let (w, h) = (320usize, 240usize);
        let mut attributes = None;
        // SAFETY: plain FFI on locals owned by this function.
        unsafe {
            MFCreateAttributes(&mut attributes, 1).unwrap();
            let attributes: IMFAttributes = attributes.unwrap();
            attributes
                .SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)
                .unwrap();
            let path = wide(path);
            let writer = MFCreateSinkWriterFromURL(
                PCWSTR(path.as_ptr()),
                None::<&IMFByteStream>,
                &attributes,
            )
            .unwrap();
            let out = av_type(&MFMediaType_Video, &MFVideoFormat_H264);
            out.SetUINT32(&MF_MT_AVG_BITRATE, 1_000_000).unwrap();
            out.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, FPS).unwrap();
            let video = writer.AddStream(&out).unwrap();
            let nv12_type = av_type(&MFMediaType_Video, &MFVideoFormat_NV12);
            writer
                .SetInputMediaType(video, &nv12_type, None::<&IMFAttributes>)
                .unwrap();
            let mut audio = Vec::new();
            for _ in 0..audio_tracks {
                let out = av_type(&MFMediaType_Audio, &MFAudioFormat_AAC);
                out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 16_000)
                    .unwrap();
                out.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0).unwrap();
                out.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)
                    .unwrap();
                let stream = writer.AddStream(&out).unwrap();
                let input = av_type(&MFMediaType_Audio, &MFAudioFormat_PCM);
                input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4).unwrap();
                input
                    .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 192_000)
                    .unwrap();
                writer
                    .SetInputMediaType(stream, &input, None::<&IMFAttributes>)
                    .unwrap();
                audio.push(stream);
            }
            writer.BeginWriting().unwrap();

            let frame_ticks = SECOND / i64::from(FPS);
            let per_frame = (48_000 / FPS) as usize;
            for i in 0..(seconds * FPS) as usize {
                let time = i as i64 * frame_ticks;
                let mut nv12 = vec![128u8; w * h * 3 / 2];
                nv12[..w * h].fill(40);
                let x = (i * 4) % (w - 20);
                for row in 0..h {
                    nv12[row * w + x..row * w + x + 20].fill(220);
                }
                let frame = make_sample(&nv12, time, frame_ticks).unwrap();
                writer.WriteSample(video, &frame).unwrap();
                let pcm: Vec<u8> = (0..per_frame)
                    .flat_map(|n| {
                        let t = (i * per_frame + n) as f32;
                        let phase = t * 440.0 * std::f32::consts::TAU / 48_000.0;
                        let v = ((phase.sin() * 8000.0) as i16).to_le_bytes();
                        [v[0], v[1], v[0], v[1]]
                    })
                    .collect();
                for &stream in &audio {
                    let chunk = make_sample(&pcm, time, frame_ticks).unwrap();
                    writer.WriteSample(stream, &chunk).unwrap();
                }
            }
            writer.Finalize().unwrap();
        }
    }

    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clipper-trim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn probe_reports_clip_properties() {
        let dir = temp_dir("probe");
        let clip = dir.join("clip.mp4");
        write_test_clip(&clip, 4, 2);
        let info = probe(&clip).unwrap();
        assert!(
            (3.8..4.3).contains(&info.duration.as_secs_f64()),
            "{info:?}"
        );
        assert_eq!((info.width, info.height, info.audio_tracks), (320, 240, 2));
    }

    #[test]
    fn trim_copies_range_and_leaves_source_untouched() {
        let dir = temp_dir("trim");
        let clip = dir.join("clip.mp4");
        write_test_clip(&clip, 4, 2);
        let before = std::fs::read(&clip).unwrap();
        let out = export_path(&clip, secs(1.0), secs(3.0));
        let mut last = 0.0;
        let cancel = AtomicBool::new(false);
        let outcome = trim(
            &clip,
            &out,
            secs(1.0),
            secs(3.0),
            &mut |p| last = p,
            &cancel,
        )
        .unwrap();
        assert_eq!(outcome.path, out);
        assert!(outcome.start <= secs(1.0), "{outcome:?}");
        assert_eq!(last, 1.0);
        let info = probe(&out).unwrap();
        assert!(
            (2.0..3.1).contains(&info.duration.as_secs_f64()),
            "{info:?}"
        );
        assert_eq!(info.audio_tracks, 2);
        assert_eq!(std::fs::read(&clip).unwrap(), before);
        assert!(!PathBuf::from(format!("{}.partial.mp4", out.display())).exists());

        // The output decodes: one video and one audio sample.
        let _init = init().unwrap();
        let reader = open_reader(&out).unwrap();
        for (index, t) in native_types(&reader).unwrap().iter().enumerate() {
            let wanted = if major(t) == MFMediaType_Video {
                av_type(&MFMediaType_Video, &MFVideoFormat_NV12)
            } else {
                av_type(&MFMediaType_Audio, &MFAudioFormat_PCM)
            };
            // SAFETY: valid reader and media types; out-pointers are live locals.
            unsafe {
                reader
                    .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, true)
                    .unwrap();
                reader
                    .SetCurrentMediaType(index as u32, None, &wanted)
                    .unwrap();
                let mut got = false;
                for _ in 0..200 {
                    let (mut flags, mut sample): (u32, Option<IMFSample>) = (0, None);
                    reader
                        .ReadSample(
                            index as u32,
                            0,
                            None,
                            Some(&mut flags),
                            None,
                            Some(&mut sample),
                        )
                        .unwrap();
                    if sample.is_some() {
                        got = true;
                        break;
                    }
                }
                assert!(got, "stream {index} produced no sample");
            }
        }
    }

    #[test]
    fn rejects_bad_requests() {
        let dir = temp_dir("invalid");
        let clip = dir.join("clip.mp4");
        write_test_clip(&clip, 2, 1);
        let go = |src: &Path, dst: &Path, a: f64, b: f64| {
            trim(
                src,
                dst,
                secs(a),
                secs(b),
                &mut |_| {},
                &AtomicBool::new(false),
            )
        };
        let dst = dir.join("out.mp4");
        assert!(matches!(
            go(&clip, &dst, 1.5, 1.0),
            Err(TrimError::InvalidRange)
        ));
        // An out-point past the end is clamped; a range starting after it is invalid.
        assert!(matches!(
            go(&clip, &dst, 3.0, 4.0),
            Err(TrimError::InvalidRange)
        ));
        let missing = dir.join("nope.mp4");
        assert!(matches!(
            go(&missing, &dst, 0.0, 1.0),
            Err(TrimError::SourceMissing)
        ));
        assert!(matches!(
            go(&clip, &clip, 0.0, 1.0),
            Err(TrimError::DestinationExists)
        ));
        std::fs::write(&dst, b"keep").unwrap();
        assert!(matches!(
            go(&clip, &dst, 0.0, 1.0),
            Err(TrimError::DestinationExists)
        ));
        assert_eq!(std::fs::read(&dst).unwrap(), b"keep");
    }

    #[test]
    fn cancel_leaves_no_files() {
        let dir = temp_dir("cancel");
        let clip = dir.join("clip.mp4");
        write_test_clip(&clip, 2, 1);
        let dst = dir.join("out.mp4");
        let cancel = AtomicBool::new(true);
        let result = trim(&clip, &dst, secs(0.0), secs(1.0), &mut |_| {}, &cancel);
        assert!(matches!(result, Err(TrimError::Cancelled)));
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["clip.mp4"]);
    }

    #[test]
    fn export_path_avoids_source_and_existing_files() {
        let dir = temp_dir("names");
        let clip = dir.join("clip.mp4");
        let first = export_path(&clip, secs(12.0), secs(45.0));
        assert_eq!(first.file_name().unwrap(), "clip (trim 00m12s-00m45s).mp4");
        std::fs::write(&first, b"x").unwrap();
        let second = export_path(&clip, secs(12.0), secs(45.0));
        assert_eq!(
            second.file_name().unwrap(),
            "clip (trim 00m12s-00m45s) 2.mp4"
        );
        assert_ne!(second, clip);
    }
}
