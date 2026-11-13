//! Inspect native MP4 streams and decode audio to measure signal and timestamps.
//! Usage: cargo run -p clipper-encoder --example probe_audio -- <clip.mp4>
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clipper_encoder::backends::media_foundation::MediaFoundation;
    use clipper_platform::windows::ComApartment;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::core::PCWSTR;
    let _com = ComApartment::init_multithreaded()?;
    let _mf = MediaFoundation::start()?;
    let path = std::env::args_os().nth(1).ok_or("pass an MP4 path")?;
    let wide: Vec<u16> = path.encode_wide().chain(Some(0)).collect();
    let mut audio_count = 0;
    // SAFETY: live interfaces and a NUL-terminated path. Decoded Float buffers
    // are read only while locked and every successful Lock is unlocked.
    unsafe {
        for index in 0..32 {
            let reader =
                MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), None::<&IMFAttributes>)?;
            let Ok(native) = reader.GetNativeMediaType(index, 0) else {
                break;
            };
            let subtype = native.GetGUID(&MF_MT_SUBTYPE)?;
            if native.GetGUID(&MF_MT_MAJOR_TYPE)? != MFMediaType_Audio {
                println!("stream={index} video subtype={subtype:?}");
                continue;
            }
            audio_count += 1;
            let rate = native.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
            let channels = native.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(index, true)?;
            let decoded = MFCreateMediaType()?;
            decoded.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            decoded.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
            reader.SetCurrentMediaType(index, None, &decoded)?;
            let (mut count, mut peak, mut sum, mut last, mut end) =
                (0_u64, 0.0_f32, 0.0_f64, -1_i64, 0_i64);
            loop {
                let (mut flags, mut time, mut sample) = (0, 0, None);
                reader.ReadSample(
                    index,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )?;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
                if let Some(sample) = sample {
                    if time < last {
                        return Err("non-monotonic audio timestamps".into());
                    }
                    last = time;
                    end = time + sample.GetSampleDuration()?;
                    let buffer = sample.ConvertToContiguousBuffer()?;
                    let (mut data, mut len) = (std::ptr::null_mut(), 0);
                    buffer.Lock(&mut data, None, Some(&mut len))?;
                    for value in std::slice::from_raw_parts(data.cast::<f32>(), len as usize / 4) {
                        count += 1;
                        peak = peak.max(value.abs());
                        sum += f64::from(*value).powi(2);
                    }
                    buffer.Unlock()?;
                }
            }
            println!(
                "stream={index} AAC={} rate={rate} channels={channels} decoded_samples={count} peak={peak:.5} rms={:.5} end_seconds={:.5}",
                subtype == MFAudioFormat_AAC,
                (sum / count.max(1) as f64).sqrt(),
                end as f64 / 10_000_000.0
            );
        }
    }
    if audio_count == 0 {
        return Err("MP4 contains no audio streams".into());
    }
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Media Foundation probing requires Windows.");
    std::process::exit(1);
}
