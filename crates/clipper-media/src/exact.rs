//! Exact, non-destructive segment export using native Media Foundation.
#![allow(unsafe_code)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::Storage::FileSystem::MoveFileW;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, PCWSTR};

use crate::trim::{self, TrimError, TrimOutcome, init, wide};

const SECOND: i64 = 10_000_000;
static NEXT_EXPORT: AtomicU64 = AtomicU64::new(0);

fn ticks(value: Duration) -> Result<i64, TrimError> {
    i64::try_from(value.as_nanos() / 100).map_err(|_| TrimError::InvalidRange)
}

fn media_error(message: &str) -> TrimError {
    TrimError::Media(message.to_owned())
}

struct Track {
    source: u32,
    sink: u32,
    audio: Option<(u32, u32)>, // PCM samples per second and bytes per sample frame.
    frame_duration: i64,
    input: IMFMediaType,
}

/// Exports the ordered source ranges into a new H.264/AAC MP4. Decoding and
/// re-encoding is required: compressed stream-copy cannot make arbitrary cuts.
/// Video frames crossing a cut receive clipped presentation durations; PCM is
/// sliced at sample boundaries. Every stream uses the same segment time origin.
/// Blocking: run on an export worker, never the UI or capture thread.
pub fn export(
    source: &Path,
    destination: &Path,
    ranges: &[(Duration, Duration)],
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<TrimOutcome, TrimError> {
    if !source.is_file() {
        return Err(TrimError::SourceMissing);
    }
    let info = trim::probe(source)?;
    if ranges.is_empty()
        || ranges.iter().any(|&(start, end)| {
            start >= end || end > info.duration || end - start < Duration::from_millis(100)
        })
    {
        return Err(TrimError::InvalidRange);
    }
    let total = ranges
        .iter()
        .try_fold(Duration::ZERO, |sum, &(start, end)| {
            sum.checked_add(end - start).ok_or(TrimError::InvalidRange)
        })?;
    ticks(total)?;
    if destination.exists()
        || source == destination
        || matches!((source.canonicalize(), destination.canonicalize()), (Ok(a), Ok(b)) if a == b)
    {
        return Err(TrimError::DestinationExists);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(TrimError::Cancelled);
    }
    if destination
        .extension()
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("mp4"))
    {
        return Err(media_error("exact export supports MP4 output only"));
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    // Reserve our own file; never touch another export's temporary file.
    let partial = loop {
        let id = NEXT_EXPORT.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".clipuh-export-{}-{id}.mp4", std::process::id()));
        if candidate == destination {
            continue;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let result = encode(source, &partial, ranges, total, progress, cancel).and_then(|()| {
        let output = trim::probe(&partial)?;
        if output.width != info.width
            || output.height != info.height
            || output.audio_tracks != info.audio_tracks
            || output.duration.abs_diff(total) > Duration::from_millis(100)
        {
            return Err(media_error(
                "exact export failed duration or stream verification",
            ));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(TrimError::Cancelled);
        }
        // MoveFileW publishes the finished file without replacing an existing
        // destination, including one created during export. Both paths are in
        // the same directory; no cross-volume copy or NTFS hard link is needed.
        let from = wide(&partial);
        let to = wide(destination);
        // SAFETY: both strings are NUL-terminated and live through this call.
        unsafe { MoveFileW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr())) }.map_err(|error| {
            if destination.exists() {
                TrimError::DestinationExists
            } else {
                TrimError::Media(format!("could not publish the exported file: {error}"))
            }
        })?;
        progress(1.0);
        Ok(TrimOutcome {
            path: destination.to_owned(),
            start: ranges[0].0,
            duration: total,
        })
    });
    let _ = std::fs::remove_file(partial);
    result
}

fn type_with(major: &GUID, subtype: &GUID) -> Result<IMFMediaType, TrimError> {
    // SAFETY: the media type is freshly allocated and all values are scalar.
    unsafe {
        let media = MFCreateMediaType()?;
        media.SetGUID(&MF_MT_MAJOR_TYPE, major)?;
        media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        Ok(media)
    }
}

/// H.264 may reveal macroblock padding only after its first decoded sample.
/// Compare visible dimensions rather than the padded storage allocation.
fn visible_size(media: &IMFMediaType) -> Result<(u32, u32), TrimError> {
    // SAFETY: read-only attributes. MFVideoArea is C-compatible POD; its exact
    // blob size is checked before the unaligned byte buffer is read.
    unsafe {
        let storage = media.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (width, height) = ((storage >> 32) as u32, storage as u32);
        for key in [MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_GEOMETRIC_APERTURE] {
            if let Ok(length) = media.GetBlobSize(&key) {
                if length as usize != std::mem::size_of::<MFVideoArea>() {
                    return Err(media_error(
                        "the decoder returned an invalid display aperture",
                    ));
                }
                let mut bytes = [0u8; std::mem::size_of::<MFVideoArea>()];
                media.GetBlob(&key, &mut bytes, None)?;
                let area = std::ptr::read_unaligned(bytes.as_ptr().cast::<MFVideoArea>());
                let x = i32::from(area.OffsetX.value);
                let y = i32::from(area.OffsetY.value);
                if x < 0
                    || y < 0
                    || area.OffsetX.fract != 0
                    || area.OffsetY.fract != 0
                    || area.Area.cx <= 0
                    || area.Area.cy <= 0
                    || x as u64 + area.Area.cx as u64 > u64::from(width)
                    || y as u64 + area.Area.cy as u64 > u64::from(height)
                {
                    return Err(media_error(
                        "the decoder returned an unsupported display aperture",
                    ));
                }
                return Ok((area.Area.cx as u32, area.Area.cy as u32));
            }
        }
        Ok((width, height))
    }
}

fn validate_type_change(expected: &IMFMediaType, current: &IMFMediaType) -> Result<(), TrimError> {
    // SAFETY: read-only scalar attributes from valid negotiated media types.
    let valid = unsafe {
        let major = expected.GetMajorType()?;
        if current.GetMajorType()? != major
            || expected.GetGUID(&MF_MT_SUBTYPE)? != current.GetGUID(&MF_MT_SUBTYPE)?
        {
            false
        } else if major == MFMediaType_Video {
            visible_size(expected)? == visible_size(current)?
                && expected.GetUINT64(&MF_MT_FRAME_RATE)? == current.GetUINT64(&MF_MT_FRAME_RATE)?
                && expected
                    .GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO)
                    .unwrap_or((1u64 << 32) | 1)
                    == current
                        .GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO)
                        .unwrap_or((1u64 << 32) | 1)
                && expected
                    .GetUINT32(&MF_MT_INTERLACE_MODE)
                    .unwrap_or(MFVideoInterlace_Progressive.0 as u32)
                    == current
                        .GetUINT32(&MF_MT_INTERLACE_MODE)
                        .unwrap_or(MFVideoInterlace_Progressive.0 as u32)
        } else if major == MFMediaType_Audio {
            [MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AUDIO_NUM_CHANNELS,
                MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT].iter()
                .all(|key| matches!((expected.GetUINT32(key), current.GetUINT32(key)), (Ok(a), Ok(b)) if a == b))
        } else {
            false
        }
    };
    if !valid {
        return Err(media_error("the source media format changed during export"));
    }
    Ok(())
}

fn configure(reader: &IMFSourceReader, writer: &IMFSinkWriter) -> Result<Vec<Track>, TrimError> {
    let mut tracks = Vec::new();
    // SAFETY: COM objects belong to this worker. Every stream index is obtained
    // from this reader; unsupported types fail before any samples are written.
    unsafe {
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        for source in 0.. {
            let native = match reader.GetNativeMediaType(source, 0) {
                Ok(media) => media,
                Err(error) if error.code() == MF_E_INVALIDSTREAMNUMBER => break,
                Err(error) => return Err(error.into()),
            };
            let major = native.GetMajorType()?;
            let (output, audio, frame_duration) = if major == MFMediaType_Video {
                if tracks.iter().any(|track: &Track| track.audio.is_none()) {
                    return Err(media_error("exact export supports one video stream"));
                }
                let input = type_with(&MFMediaType_Video, &MFVideoFormat_NV12)?;
                reader.SetCurrentMediaType(source, None, &input)?;
                let input = reader.GetCurrentMediaType(source)?;
                let output = type_with(&MFMediaType_Video, &MFVideoFormat_H264)?;
                let size = input.GetUINT64(&MF_MT_FRAME_SIZE)?;
                let width = size >> 32;
                let height = size as u32 as u64;
                if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
                    return Err(media_error(
                        "H.264 exact export requires even frame dimensions",
                    ));
                }
                let rate = input.GetUINT64(&MF_MT_FRAME_RATE)?;
                let numerator = (rate >> 32) as i64;
                let denominator = rate as u32 as i64;
                if numerator == 0 || denominator == 0 {
                    return Err(media_error("the source frame rate is invalid"));
                }
                output.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
                output.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
                output.SetUINT64(
                    &MF_MT_PIXEL_ASPECT_RATIO,
                    input
                        .GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO)
                        .unwrap_or((1u64 << 32) | 1),
                )?;
                output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                let bitrate = (width
                    .saturating_mul(height)
                    .saturating_mul(numerator as u64)
                    / denominator as u64
                    / 5)
                .clamp(1_000_000, 60_000_000) as u32;
                output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
                (output, None, SECOND * denominator / numerator)
            } else if major == MFMediaType_Audio {
                let input = type_with(&MFMediaType_Audio, &MFAudioFormat_PCM)?;
                input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                reader.SetCurrentMediaType(source, None, &input)?;
                let input = reader.GetCurrentMediaType(source)?;
                let rate = input.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
                let channels = input.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?;
                let alignment = input.GetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT)?;
                if rate == 0 || alignment == 0 || channels == 0 {
                    return Err(media_error("the source audio format is invalid"));
                }
                let output = type_with(&MFMediaType_Audio, &MFAudioFormat_AAC)?;
                output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
                output.SetUINT32(
                    &MF_MT_AUDIO_SAMPLES_PER_SECOND,
                    if rate == 44_100 { 44_100 } else { 48_000 },
                )?;
                output.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                output.SetUINT32(
                    &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                    if channels > 2 { 40_000 } else { 24_000 },
                )?;
                output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
                output.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
                (output, Some((rate, alignment)), 0)
            } else {
                return Err(media_error(
                    "the source contains an unsupported non-audio/video stream",
                ));
            };
            let sink = writer.AddStream(&output)?;
            writer.SetInputMediaType(
                sink,
                &reader.GetCurrentMediaType(source)?,
                None::<&IMFAttributes>,
            )?;
            reader.SetStreamSelection(source, true)?;
            tracks.push(Track {
                source,
                sink,
                audio,
                frame_duration,
                input: reader.GetCurrentMediaType(source)?,
            });
        }
    }
    if !tracks.iter().any(|track| track.audio.is_none()) {
        return Err(media_error("the source has no video stream"));
    }
    Ok(tracks)
}

fn encode(
    source: &Path,
    partial: &Path,
    ranges: &[(Duration, Duration)],
    total: Duration,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<(), TrimError> {
    let _init = init()?;
    // SAFETY: all COM objects and attribute out-pointers are local to this
    // worker. Wide strings remain live for the duration of their FFI calls.
    let (reader, writer) = unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 3)?;
        let attributes = attributes.ok_or_else(|| media_error("no export attributes"))?;
        attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        let source = wide(source);
        let partial = wide(partial);
        (
            MFCreateSourceReaderFromURL(PCWSTR(source.as_ptr()), &attributes)?,
            MFCreateSinkWriterFromURL(
                PCWSTR(partial.as_ptr()),
                None::<&IMFByteStream>,
                &attributes,
            )?,
        )
    };
    let tracks = configure(&reader, &writer)?;
    // SAFETY: writer is configured for every selected source stream.
    unsafe { writer.BeginWriting()? };
    let mut offset = 0i64;
    let total = ticks(total)?;
    let mut percent = -1;
    for &(start, end) in ranges {
        let (start, end) = (ticks(start)?, ticks(end)?);
        let mut done = vec![false; tracks.len()];
        let mut written = vec![false; tracks.len()];
        // SAFETY: selected tracks are valid reader indexes; seeking flushes
        // decoder state and returns preroll, which is discarded below.
        unsafe {
            for track in &tracks {
                reader.SetStreamSelection(track.source, true)?;
            }
            reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(start))?;
        }
        while done.iter().any(|value| !value) {
            if cancel.load(Ordering::Relaxed) {
                return Err(TrimError::Cancelled);
            }
            let (mut index, mut flags, mut time, mut sample) = (0, 0, 0, None);
            // SAFETY: each out-pointer refers to a live local.
            unsafe {
                reader.ReadSample(
                    MF_SOURCE_READER_ANY_STREAM.0 as u32,
                    0,
                    Some(&mut index),
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )?
            };
            let Some(slot) = tracks.iter().position(|track| track.source == index) else {
                continue;
            };
            if done[slot] {
                continue;
            }
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err(media_error("the source decoder reported an export error"));
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                // SAFETY: reader and sink indexes were negotiated together.
                // Validate visible size/layout before renegotiating decoder
                // padding, stride and aperture with the writer's processor.
                unsafe {
                    let current = reader.GetCurrentMediaType(index)?;
                    validate_type_change(&tracks[slot].input, &current)?;
                    writer.SetInputMediaType(
                        tracks[slot].sink,
                        &current,
                        None::<&IMFAttributes>,
                    )?;
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0
                || (time >= end && sample.is_some())
            {
                done[slot] = true;
                // SAFETY: valid source index; stop requesting this finished stream.
                unsafe { reader.SetStreamSelection(index, false)? };
                continue;
            }
            let Some(sample) = sample else { continue };
            let track = &tracks[slot];
            let clipped = clip_sample(&sample, track, time, start, end, offset)?;
            if let Some(sample) = clipped {
                // SAFETY: a new or decoded sample matches the configured input type.
                unsafe {
                    if !written[slot] {
                        sample.SetUINT32(&MFSampleExtension_Discontinuity, 1)?;
                    }
                    writer.WriteSample(track.sink, &sample)?;
                }
                written[slot] = true;
                let current =
                    (((offset + (time - start).max(0)) as f64 / total as f64) * 100.0) as i32;
                if current > percent {
                    percent = current;
                    progress((current as f32 / 100.0).min(0.99));
                }
            }
        }
        if written.iter().any(|value| !value) {
            return Err(media_error(
                "a selected segment contains no samples for one of its tracks",
            ));
        }
        offset += end - start;
    }
    // SAFETY: every stream has been written with monotonically increasing times.
    unsafe { writer.Finalize()? };
    Ok(())
}

fn clip_sample(
    sample: &IMFSample,
    track: &Track,
    time: i64,
    start: i64,
    end: i64,
    offset: i64,
) -> Result<Option<IMFSample>, TrimError> {
    // SAFETY: decoded sample is owned by this worker; buffer locks are paired
    // even on allocation failures, and slices stay within GetCurrentLength.
    unsafe {
        if let Some((rate, alignment)) = track.audio {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let length = buffer.GetCurrentLength()?;
            let frames = i64::from(length / alignment);
            let ceil_frames = |delta: i64| {
                (((i128::from(delta.max(0)) * i128::from(rate) + i128::from(SECOND) - 1)
                    / i128::from(SECOND))
                .min(i128::from(frames))) as i64
            };
            let first = ceil_frames(start.saturating_sub(time));
            let last = ceil_frames(end.saturating_sub(time));
            if first >= last {
                return Ok(None);
            }
            let clipped_time = (time + first * SECOND / i64::from(rate)).max(start);
            let duration = ((last - first) * SECOND / i64::from(rate)).min(end - clipped_time);
            if duration <= 0 {
                return Ok(None);
            }
            let bytes = u32::try_from((last - first) * i64::from(alignment))
                .map_err(|_| TrimError::InvalidRange)?;
            let output = MFCreateMemoryBuffer(bytes)?;
            let (mut input_ptr, mut output_ptr) = (std::ptr::null_mut(), std::ptr::null_mut());
            buffer.Lock(&mut input_ptr, None, None)?;
            if let Err(error) = output.Lock(&mut output_ptr, None, None) {
                let _ = buffer.Unlock();
                return Err(error.into());
            }
            std::ptr::copy_nonoverlapping(
                input_ptr.add(first as usize * alignment as usize),
                output_ptr,
                bytes as usize,
            );
            let input_unlock = buffer.Unlock();
            let output_unlock = output.Unlock();
            input_unlock?;
            output_unlock?;
            output.SetCurrentLength(bytes)?;
            let result = MFCreateSample()?;
            result.AddBuffer(&output)?;
            result.SetSampleTime(offset + clipped_time - start)?;
            result.SetSampleDuration(duration)?;
            Ok(Some(result))
        } else {
            let duration = sample.GetSampleDuration().unwrap_or(track.frame_duration);
            let first = time.max(start);
            let last = time.saturating_add(duration).min(end);
            if first >= last {
                return Ok(None);
            }
            sample.SetSampleTime(offset + first - start)?;
            sample.SetSampleDuration(last - first)?;
            Ok(Some(sample.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trim::{
        self,
        tests::{temp_dir, write_test_clip},
    };
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    fn decoded_reader(path: &Path) -> IMFSourceReader {
        // SAFETY: attributes and wide path are owned by this call.
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 2).unwrap();
            let attributes = attributes.unwrap();
            attributes
                .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                .unwrap();
            attributes
                .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                .unwrap();
            let path = wide(path);
            MFCreateSourceReaderFromURL(PCWSTR(path.as_ptr()), &attributes).unwrap()
        }
    }

    #[test]
    fn decoder_padding_is_accepted_but_visible_resolution_changes_are_rejected() {
        let _init = init().unwrap();
        let expected = type_with(&MFMediaType_Video, &MFVideoFormat_NV12).unwrap();
        let current = type_with(&MFMediaType_Video, &MFVideoFormat_NV12).unwrap();
        // SAFETY: fresh media types and a byte view of a live C-compatible POD.
        unsafe {
            expected
                .SetUINT64(&MF_MT_FRAME_SIZE, (1920u64 << 32) | 1080)
                .unwrap();
            expected
                .SetUINT64(&MF_MT_FRAME_RATE, (60u64 << 32) | 1)
                .unwrap();
            expected.CopyAllItems(&current).unwrap();
            current
                .SetUINT64(&MF_MT_FRAME_SIZE, (1920u64 << 32) | 1088)
                .unwrap();
            assert!(validate_type_change(&expected, &current).is_err());
            let mut area = MFVideoArea::default();
            area.Area.cx = 1920;
            area.Area.cy = 1080;
            let bytes = std::slice::from_raw_parts(
                (&area as *const MFVideoArea).cast::<u8>(),
                std::mem::size_of::<MFVideoArea>(),
            );
            current
                .SetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes)
                .unwrap();
            assert_eq!(visible_size(&current).unwrap(), (1920, 1080));
            validate_type_change(&expected, &current).unwrap();
            current
                .SetUINT64(&MF_MT_FRAME_RATE, (30u64 << 32) | 1)
                .unwrap();
            assert!(validate_type_change(&expected, &current).is_err());
            current
                .SetUINT64(&MF_MT_FRAME_RATE, (60u64 << 32) | 1)
                .unwrap();
            current
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .unwrap();
            assert!(validate_type_change(&expected, &current).is_err());
        }
    }

    #[test]
    fn audio_layout_changes_are_rejected() {
        let _init = init().unwrap();
        let expected = type_with(&MFMediaType_Audio, &MFAudioFormat_PCM).unwrap();
        let current = type_with(&MFMediaType_Audio, &MFAudioFormat_PCM).unwrap();
        // SAFETY: scalar attributes on fresh media types.
        unsafe {
            expected
                .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)
                .unwrap();
            expected.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).unwrap();
            expected
                .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
                .unwrap();
            expected.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4).unwrap();
            expected.CopyAllItems(&current).unwrap();
            validate_type_change(&expected, &current).unwrap();
            current.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1).unwrap();
            assert!(validate_type_change(&expected, &current).is_err());
        }
    }

    #[test]
    #[ignore = "set CLIPUH_EDITOR_TEST_CLIP to an existing real MP4"]
    fn real_clip_exact_segment_export() {
        let source = std::path::PathBuf::from(
            std::env::var_os("CLIPUH_EDITOR_TEST_CLIP").expect("set CLIPUH_EDITOR_TEST_CLIP"),
        );
        let info = trim::probe(&source).unwrap();
        let original = std::fs::read(&source).unwrap();
        let dir = temp_dir("exact-real-clip");
        let destination = dir.join("result.mp4");
        let quarter = info.duration / 4;
        let ranges = [
            (Duration::from_millis(150), quarter),
            (quarter * 2 + Duration::from_millis(75), quarter * 3),
        ];
        let expected: Duration = ranges.iter().map(|(start, end)| *end - *start).sum();
        let result = export(
            &source,
            &destination,
            &ranges,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result.duration, expected);
        let output = trim::probe(&destination).unwrap();
        assert_eq!(
            (output.width, output.height, output.audio_tracks),
            (info.width, info.height, info.audio_tracks)
        );
        assert!(output.duration.abs_diff(expected) < Duration::from_millis(100));
        assert_eq!(std::fs::read(&source).unwrap(), original);
    }

    #[test]
    fn exact_segments_preserve_tracks_and_source() {
        let dir = temp_dir("exact-segments");
        let source = dir.join("source.mp4");
        let destination = dir.join("result.mp4");
        write_test_clip(&source, 4, 2);
        let original = std::fs::read(&source).unwrap();
        let ms = Duration::from_millis;
        let ranges = [(ms(450), ms(1250)), (ms(2150), ms(3050))];
        let outcome = export(
            &source,
            &destination,
            &ranges,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome.start, ms(450));
        assert_eq!(outcome.duration, ms(1700));
        let info = trim::probe(&destination).unwrap();
        assert_eq!(info.audio_tracks, 2);
        assert!((info.duration.as_secs_f64() - 1.7).abs() < 0.06, "{info:?}");
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_decodes_with_synced_timestamps(&destination, 3, 1.7);
        // The fixture's bright bar moves four pixels each frame. At 450 ms
        // it is near x=52; a keyframe-snapped export would start near x=0.
        let _init = init().unwrap();
        let reader = decoded_reader(&destination);
        // SAFETY: all COM objects and out-pointers are local to this test.
        unsafe {
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .unwrap();
            reader
                .SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)
                .unwrap();
            reader
                .SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    None,
                    &type_with(&MFMediaType_Video, &MFVideoFormat_NV12).unwrap(),
                )
                .unwrap();
            let mut sample = None;
            for _ in 0..20 {
                let (mut flags, mut time) = (0, 0);
                reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut time),
                        Some(&mut sample),
                    )
                    .unwrap();
                if sample.is_some() {
                    break;
                }
            }
            let buffer = sample.unwrap().ConvertToContiguousBuffer().unwrap();
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None).unwrap();
            let row = std::slice::from_raw_parts(ptr, 320);
            let bar = row.iter().position(|value| *value > 170);
            buffer.Unlock().unwrap();
            assert!(matches!(bar, Some(48..=56)), "first frame bar at {bar:?}");
        }
    }

    fn assert_decodes_with_synced_timestamps(path: &Path, streams: u32, duration: f64) {
        let _init = init().unwrap();
        for stream in 0..streams {
            let reader = decoded_reader(path);
            // SAFETY: valid reader and types; all out-pointers are live locals.
            unsafe {
                reader
                    .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                    .unwrap();
                reader.SetStreamSelection(stream, true).unwrap();
                let media = if reader
                    .GetNativeMediaType(stream, 0)
                    .unwrap()
                    .GetMajorType()
                    .unwrap()
                    == MFMediaType_Video
                {
                    type_with(&MFMediaType_Video, &MFVideoFormat_NV12).unwrap()
                } else {
                    {
                        let media = type_with(&MFMediaType_Audio, &MFAudioFormat_PCM).unwrap();
                        media.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).unwrap();
                        media
                    }
                };
                reader
                    .SetCurrentMediaType(stream, None, &media)
                    .unwrap_or_else(|error| panic!("stream {stream}: {error}"));
                let mut last = -1;
                let mut final_end = 0;
                let mut count = 0;
                loop {
                    let (mut flags, mut time, mut sample) = (0, 0, None);
                    reader
                        .ReadSample(
                            stream,
                            0,
                            None,
                            Some(&mut flags),
                            Some(&mut time),
                            Some(&mut sample),
                        )
                        .unwrap();
                    if let Some(sample) = sample {
                        assert!(time >= last, "stream {stream}: {time} after {last}");
                        if count > 0 {
                            assert!(
                                time <= final_end + 2,
                                "gap in stream {stream}: {final_end}..{time}"
                            );
                        }
                        if count == 0 {
                            assert!(time.abs() < SECOND / 30, "stream {stream} starts at {time}");
                        }
                        last = time;
                        final_end = time + sample.GetSampleDuration().unwrap();
                        count += 1;
                    }
                    if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                        break;
                    }
                    assert!(count < 1000);
                }
                assert!(count > 0);
                assert!(
                    (final_end as f64 / SECOND as f64 - duration).abs() < 0.06,
                    "stream {stream} ends at {final_end}"
                );
            }
        }
    }

    #[test]
    fn destination_created_during_export_is_never_overwritten() {
        let dir = temp_dir("exact-race");
        let source = dir.join("source.mp4");
        let destination = dir.join("result.mp4");
        write_test_clip(&source, 2, 1);
        let mut created = false;
        let result = export(
            &source,
            &destination,
            &[(Duration::from_millis(250), Duration::from_millis(1250))],
            &mut |_| {
                if !created {
                    std::fs::write(&destination, b"keep").unwrap();
                    created = true;
                }
            },
            &AtomicBool::new(false),
        );
        assert!(
            matches!(result, Err(TrimError::DestinationExists)),
            "{result:?}"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    }

    #[test]
    fn cancellation_during_encoding_cleans_temporary_output() {
        let dir = temp_dir("exact-mid-cancel");
        let source = dir.join("source.mp4");
        let destination = dir.join("result.mp4");
        write_test_clip(&source, 2, 1);
        let cancel = AtomicBool::new(false);
        let result = export(
            &source,
            &destination,
            &[(Duration::from_millis(250), Duration::from_millis(1250))],
            &mut |_| {
                cancel.store(true, Ordering::Relaxed);
            },
            &cancel,
        );
        assert!(matches!(result, Err(TrimError::Cancelled)), "{result:?}");
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn exact_export_rejects_missing_invalid_and_existing_destinations() {
        let dir = temp_dir("exact-invalid");
        let source = dir.join("source.mp4");
        let destination = dir.join("result.mp4");
        let ms = Duration::from_millis;
        let cancel = AtomicBool::new(false);
        assert!(matches!(
            export(
                &source,
                &destination,
                &[(ms(0), ms(1000))],
                &mut |_| {},
                &cancel
            ),
            Err(trim::TrimError::SourceMissing)
        ));
        write_test_clip(&source, 2, 1);
        for ranges in [
            vec![],
            vec![(ms(500), ms(500))],
            vec![(ms(1500), ms(1000))],
            vec![(ms(0), ms(5000))],
        ] {
            assert!(matches!(
                export(&source, &destination, &ranges, &mut |_| {}, &cancel),
                Err(trim::TrimError::InvalidRange)
            ));
        }
        std::fs::write(&destination, b"keep").unwrap();
        assert!(matches!(
            export(
                &source,
                &destination,
                &[(ms(0), ms(1000))],
                &mut |_| {},
                &cancel
            ),
            Err(trim::TrimError::DestinationExists)
        ));
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep");
    }

    #[test]
    fn exact_export_cancellation_keeps_destination_absent() {
        let dir = temp_dir("exact-cancel");
        let source = dir.join("source.mp4");
        let destination = dir.join("result.mp4");
        write_test_clip(&source, 2, 1);
        assert!(matches!(
            export(
                &source,
                &destination,
                &[(Duration::ZERO, Duration::from_secs(1))],
                &mut |_| {},
                &AtomicBool::new(true)
            ),
            Err(trim::TrimError::Cancelled)
        ));
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }
}
