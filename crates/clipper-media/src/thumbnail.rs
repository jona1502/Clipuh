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
    if !path.is_file() {
        return Err("clip not found".into());
    }
    let wide = wide(path);
    // SAFETY: plain FFI; `wide` is NUL-terminated and outlives the call.
    let reader = unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1).map_err(|e| e.to_string())?;
        let attrs: IMFAttributes = attrs.ok_or("no attributes")?;
        // Lets the reader convert decoder output (NV12) to RGB32.
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(|e| e.to_string())?;
        MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attrs).map_err(|e| e.to_string())?
    };
    let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // SAFETY: plain FFI on a reader and locals owned by this function.
    unsafe {
        let rgb = MFCreateMediaType().map_err(|e| e.to_string())?;
        rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| e.to_string())?;
        reader
            .SetCurrentMediaType(video, None, &rgb)
            .map_err(|e| format!("RGB32 output unsupported: {e}"))?;

        let duration = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()
            .and_then(|v: PROPVARIANT| u64::try_from(&v).ok())
            .map_or(Duration::ZERO, |t| Duration::from_nanos(t * 100));
        let at = (duration / 10).clamp(Duration::from_millis(500), Duration::from_secs(1));
        if duration > at {
            let target = PROPVARIANT::from((at.as_nanos() / 100) as i64);
            let _ = reader.SetCurrentPosition(&windows::core::GUID::zeroed(), &target);
        }

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
        if w == 0 || h == 0 {
            return Err("video has no frame size".into());
        }

        let mut sample = None;
        for _ in 0..64 {
            let mut flags = 0u32;
            reader
                .ReadSample(video, 0, None, Some(&mut flags), None, Some(&mut sample))
                .map_err(|e| e.to_string())?;
            if sample.is_some() {
                break;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        let sample = sample.ok_or("no video frame")?;
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
        result
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
    let ow = w.min(max_width.max(1));
    let oh = (h * ow / w).max(1);
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
