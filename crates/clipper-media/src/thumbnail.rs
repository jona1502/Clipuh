//! One-frame poster thumbnails via a Media Foundation source reader.
#![allow(unsafe_code)]

use std::path::Path;
use std::time::Duration;

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::PCWSTR;

use crate::trim::{init, wide};

pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// Tightly packed BGRA, alpha 255.
    pub bgra: Vec<u8>,
}

/// Decodes one frame (~10% in, clamped to 0.5..=1 s) and downscales it to at
/// most `max_width` pixels wide. Blocking: run off the UI thread.
pub fn thumbnail(path: &Path, max_width: u32) -> Result<Thumbnail, String> {
    let _init = init().map_err(|e| e.to_string())?;
    let reader = open(path)?;
    let duration = duration(&reader)?;
    let at = (duration / 10)
        .clamp(Duration::from_millis(500), Duration::from_secs(1))
        .min(duration.saturating_sub(Duration::from_nanos(1)));
    frame(&reader, at, max_width, || false).map(|(_, frame)| frame)
}

/// Extracts real timeline frames on one dedicated worker and decoder. The queue
/// holds at most two frames, and a request produces at most twelve 160 px frames.
/// Dropping the receiver cancels extraction before the next frame is decoded.
pub fn strip(
    path: &Path,
    count: usize,
    max_width: u32,
) -> async_channel::Receiver<Result<(Duration, Thumbnail), String>> {
    let (sender, receiver) = async_channel::bounded(2);
    let path = path.to_owned();
    let count = count.clamp(1, 12);
    let max_width = max_width.clamp(1, 160);
    let error_sender = sender.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("clip-thumbnails".into())
        .spawn(move || {
            let run = || -> Result<(), String> {
                let _init = init().map_err(|e| e.to_string())?;
                let reader = open(&path)?;
                let duration = duration(&reader)?;
                for index in 0..count {
                    if sender.is_closed() {
                        return Ok(());
                    }
                    let at = duration.mul_f64((index as f64 + 0.5) / count as f64);
                    let thumbnail = frame(&reader, at, max_width, || sender.is_closed())?;
                    if sender.send_blocking(Ok(thumbnail)).is_err() {
                        return Ok(());
                    }
                }
                Ok(())
            };
            if let Err(error) = run() {
                let _ = sender.send_blocking(Err(error));
            }
        })
    {
        let _ = error_sender.try_send(Err(format!(
            "Could not start thumbnail extraction: {error}"
        )));
    }
    receiver
}

fn open(path: &Path) -> Result<IMFSourceReader, String> {
    if !path.is_file() {
        return Err("clip not found".into());
    }
    let wide = wide(path);
    // SAFETY: plain FFI; `wide` is NUL-terminated and outlives the call.
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1).map_err(|e| e.to_string())?;
        let attrs: IMFAttributes = attrs.ok_or("no attributes")?;
        // Lets the reader convert decoder output (NV12) to RGB32.
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(|e| e.to_string())?;
        let reader = MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attrs)
            .map_err(|e| e.to_string())?;
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
            .map_err(|e| e.to_string())?;
        reader
            .SetStreamSelection(video, true)
            .map_err(|e| e.to_string())?;
        let rgb = MFCreateMediaType().map_err(|e| e.to_string())?;
        rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| e.to_string())?;
        reader
            .SetCurrentMediaType(video, None, &rgb)
            .map_err(|e| format!("RGB32 output unsupported: {e}"))?;
        Ok(reader)
    }
}

fn duration(reader: &IMFSourceReader) -> Result<Duration, String> {
    // SAFETY: the reader is live and the returned PROPVARIANT owns its data.
    let value: PROPVARIANT = unsafe {
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
    }
    .map_err(|e| e.to_string())?;
    let ticks = u64::try_from(&value).map_err(|e| e.to_string())?;
    ticks
        .checked_mul(100)
        .map(Duration::from_nanos)
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| "invalid clip duration".into())
}

fn frame(
    reader: &IMFSourceReader,
    at: Duration,
    max_width: u32,
    cancelled: impl Fn() -> bool,
) -> Result<(Duration, Thumbnail), String> {
    let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // SAFETY: FFI uses a live reader and media type; all out-pointers are locals.
    unsafe {
        let target =
            i64::try_from(at.as_nanos() / 100).map_err(|_| "thumbnail timestamp overflow")?;
        reader
            .SetCurrentPosition(&windows::core::GUID::zeroed(), &PROPVARIANT::from(target))
            .map_err(|e| e.to_string())?;
        let current = reader
            .GetCurrentMediaType(video)
            .map_err(|e| e.to_string())?;
        let size = current
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map_err(|e| e.to_string())?;
        let (w, h) = ((size >> 32) as usize, size as u32 as usize);
        // Negative default stride means bottom-up rows.
        let stride = current
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .unwrap_or(w as u32 * 4) as i32;
        if w == 0 || h == 0 || w > 16_384 || h > 16_384 {
            return Err("invalid video frame size".into());
        }
        // Seek lands on an earlier keyframe. Decode forward to the requested
        // presentation time instead of repeating that keyframe in every cell.
        let mut selected = None;
        for _ in 0..4096 {
            if cancelled() {
                return Err("thumbnail extraction cancelled".into());
            }
            let (mut flags, mut timestamp, mut sample) = (0u32, 0i64, None);
            reader
                .ReadSample(
                    video,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .map_err(|e| e.to_string())?;
            if let Some(sample) = sample {
                let length = sample.GetSampleDuration().unwrap_or(0);
                if timestamp >= target || timestamp.saturating_add(length) > target {
                    selected = Some((timestamp, sample));
                    break;
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        let (timestamp, sample) = selected.ok_or("no video frame at requested time")?;
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| e.to_string())?;
        let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
        buffer
            .Lock(&mut ptr, None, Some(&mut len))
            .map_err(|e| e.to_string())?;
        let data = std::slice::from_raw_parts(ptr, len as usize);
        let result = downscale(data, w, h, stride, max_width as usize);
        let _ = buffer.Unlock();
        result.map(|frame| (Duration::from_nanos(timestamp.max(0) as u64 * 100), frame))
    }
}

/// Nearest-neighbour; fine for ~160 px posters.
fn downscale(
    src: &[u8],
    w: usize,
    h: usize,
    stride: i32,
    max_width: usize,
) -> Result<Thumbnail, String> {
    let row_bytes = stride.unsigned_abs() as usize;
    if row_bytes < w * 4 || src.len() < row_bytes * h {
        return Err("short frame buffer".into());
    }
    let limit = max_width.max(1);
    let mut ow = w.min(limit);
    let mut oh = (h * ow / w).max(1);
    // Bound both dimensions so a tall source cannot defeat the cache budget.
    if oh > limit {
        oh = limit;
        ow = (w * limit / h).max(1);
    }
    let mut bgra = Vec::with_capacity(ow * oh * 4);
    for y in 0..oh {
        let sy = y * h / oh;
        let row = if stride < 0 { h - 1 - sy } else { sy };
        let line = &src[row * row_bytes..];
        for x in 0..ow {
            let p = &line[(x * w / ow) * 4..][..4];
            bgra.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
    }
    Ok(Thumbnail {
        width: ow as u32,
        height: oh as u32,
        bgra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trim::tests::{temp_dir, write_test_clip};

    #[test]
    fn portrait_thumbnail_cache_has_a_pixel_budget() {
        let source = vec![0u8; 4 * 10_000 * 4];
        let image = downscale(&source, 4, 10_000, 16, 160).unwrap();
        assert!(image.width <= 160 && image.height <= 160);
        assert!(image.bgra.len() <= 160 * 160 * 4);
    }

    #[test]
    fn strip_missing_file_reports_one_error() {
        let receiver = strip(Path::new("missing-editor-thumbnail.mp4"), 4, 160);
        assert_eq!(receiver.capacity(), Some(2));
        assert!(receiver.recv_blocking().unwrap().is_err());
        assert!(receiver.recv_blocking().is_err());
    }

    #[test]
    fn strip_is_bounded_and_decodes_distinct_frames() {
        let dir = temp_dir("strip");
        let clip = dir.join("a.mp4");
        write_test_clip(&clip, 2, 0);
        let receiver = strip(&clip, usize::MAX, u32::MAX);
        let mut frames = Vec::new();
        while let Ok(frame) = receiver.recv_blocking() {
            frames.push(frame.unwrap());
        }
        assert_eq!(frames.len(), 12);
        assert!(frames.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(frames.iter().all(|(_, frame)| frame.width <= 160));
        assert_ne!(frames[0].1.bgra, frames[6].1.bgra);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn strip_normalizes_zero_requests_and_cancellation_prevents_decode() {
        let dir = temp_dir("strip-zero");
        let clip = dir.join("a.mp4");
        write_test_clip(&clip, 1, 0);
        let receiver = strip(&clip, 0, 0);
        let (_, thumbnail) = receiver.recv_blocking().unwrap().unwrap();
        assert_eq!(thumbnail.width, 1);
        assert!(receiver.recv_blocking().is_err());
        let _init = init().unwrap();
        let reader = open(&clip).unwrap();
        assert!(frame(&reader, Duration::ZERO, 160, || true).is_err());
        drop(reader);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn thumbnail_of_test_clip() {
        let dir = temp_dir("thumb");
        let clip = dir.join("a.mp4");
        write_test_clip(&clip, 2, 1);
        let t = thumbnail(&clip, 160).unwrap();
        assert_eq!((t.width, t.height), (160, 120));
        assert_eq!(t.bgra.len(), 160 * 120 * 4);
        assert!(t.bgra.chunks(4).all(|p| p[3] == 255));
        assert!(t.bgra.chunks(4).any(|p| p[0] > 30), "frame is not blank");
        let _ = std::fs::remove_dir_all(dir);
    }
}
