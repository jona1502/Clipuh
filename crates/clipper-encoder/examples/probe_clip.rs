//! FFprobe-like clip check with Media Foundation: lists every stream, fully
//! DECODES each one (H.264 to NV12, AAC to float) and reports frame/sample
//! counts, first/last timestamps, gaps, and audio peak level.
//! Usage: cargo run -p clipper-encoder --example probe_clip -- <clip.mp4>
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clipper_encoder::backends::media_foundation::MediaFoundation;
    use clipper_platform::windows::ComApartment;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToInt64;
    use windows::core::PCWSTR;

    let _com = ComApartment::init_multithreaded()?;
    let _mf = MediaFoundation::start()?;
    let path = std::env::args_os().nth(1).ok_or("pass an MP4 path")?;
    let wide: Vec<u16> = path.encode_wide().chain(Some(0)).collect();
    let mut failed = false;
    // SAFETY: live interfaces, NUL-terminated path; decoded buffers are only
    // read while locked and every successful Lock is unlocked.
    unsafe {
        let reader = MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), None::<&IMFAttributes>)?;
        let duration = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()
            .and_then(|value| PropVariantToInt64(&value).ok());
        println!(
            "container duration={:.3}s",
            duration.unwrap_or(0) as f64 / 1e7
        );
        for index in 0..32u32 {
            let reader =
                MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), None::<&IMFAttributes>)?;
            let Ok(native) = reader.GetNativeMediaType(index, 0) else {
                break;
            };
            let video = native.GetGUID(&MF_MT_MAJOR_TYPE)? == MFMediaType_Video;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(index, true)?;
            let decoded = MFCreateMediaType()?;
            if video {
                let size = native.GetUINT64(&MF_MT_FRAME_SIZE)?;
                print!(
                    "stream={index} video h264 {}x{}",
                    size >> 32,
                    size & 0xffff_ffff
                );
                decoded.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                decoded.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            } else {
                print!(
                    "stream={index} audio aac {} Hz {} ch",
                    native.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?,
                    native.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?
                );
                decoded.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                decoded.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
            }
            reader.SetCurrentMediaType(index, None, &decoded)?;
            let (mut count, mut first, mut last, mut end) = (0u64, None, 0i64, 0i64);
            let (mut max_gap, mut peak, mut errors) = (0i64, 0f32, 0u32);
            loop {
                let mut flags = 0u32;
                let mut time = 0i64;
                let mut sample = None;
                if reader
                    .ReadSample(
                        index,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut time),
                        Some(&mut sample),
                    )
                    .is_err()
                {
                    errors += 1;
                    break;
                }
                if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                    errors += 1;
                    break;
                }
                if let Some(sample) = sample {
                    if count > 0 {
                        max_gap = max_gap.max(time - end);
                    }
                    first.get_or_insert(time);
                    last = time;
                    end = time + sample.GetSampleDuration().unwrap_or(0);
                    count += 1;
                    if !video {
                        let buffer = sample.ConvertToContiguousBuffer()?;
                        let mut data = std::ptr::null_mut();
                        let mut len = 0u32;
                        buffer.Lock(&mut data, None, Some(&mut len))?;
                        let floats =
                            std::slice::from_raw_parts(data.cast::<f32>(), len as usize / 4);
                        for value in floats {
                            peak = peak.max(value.abs());
                        }
                        buffer.Unlock()?;
                    }
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
            }
            let first = first.unwrap_or(0);
            print!(
                " decoded={count} first={:.3}s last={:.3}s end={:.3}s max_gap={:.1}ms",
                first as f64 / 1e7,
                last as f64 / 1e7,
                end as f64 / 1e7,
                max_gap as f64 / 1e4
            );
            if !video {
                print!(" peak={peak:.4}");
            }
            println!(" errors={errors}");
            failed |= errors > 0 || count == 0 || first < 0;
        }
    }
    if failed {
        return Err("decode check failed".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
