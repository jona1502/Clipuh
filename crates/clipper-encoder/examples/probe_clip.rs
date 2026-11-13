//! FFprobe-like clip check with Media Foundation: lists every stream, fully
//! DECODES each one (H.264 to NV12, AAC to float) and reports frame/sample
//! counts, first/last timestamps, the decoded frame rate, min/max/median
//! interval between successive timestamps, the number of intervals above
//! 1.5x the median, and the audio peak level.
//! Usage: cargo run -p clipper-encoder --example probe_clip -- <clip.mp4>
/// Statistics over the timestamps of successive decoded samples (100 ns).
#[derive(Debug, PartialEq)]
struct IntervalStats {
    min: i64,
    max: i64,
    median: i64,
    /// Intervals longer than 1.5x the median.
    long: usize,
    /// Samples per second over `last - first + average duration`.
    rate: f64,
}

fn interval_stats(times: &[i64], durations_sum: i64) -> Option<IntervalStats> {
    if times.len() < 2 {
        return None;
    }
    let mut intervals: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let (min, max) = (*intervals.iter().min()?, *intervals.iter().max()?);
    intervals.sort_unstable();
    let median = intervals[intervals.len() / 2];
    let long = intervals.iter().filter(|i| **i * 2 > median * 3).count();
    let average = durations_sum / times.len() as i64;
    let average = if average > 0 { average } else { median };
    let span = (times[times.len() - 1] - times[0] + average) as f64 / 1e7;
    Some(IntervalStats {
        min,
        max,
        median,
        long,
        rate: times.len() as f64 / span,
    })
}

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
            let (mut peak, mut errors) = (0f32, 0u32);
            let (mut times, mut durations_sum) = (Vec::<i64>::new(), 0i64);
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
                    times.push(time);
                    durations_sum += sample.GetSampleDuration().unwrap_or(0);
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
                " decoded={count} first={:.3}s last={:.3}s end={:.3}s",
                first as f64 / 1e7,
                last as f64 / 1e7,
                end as f64 / 1e7,
            );
            if let Some(stats) = interval_stats(&times, durations_sum) {
                print!(
                    " fps={:.3} interval_ms(min/median/max)={:.2}/{:.2}/{:.2} long_intervals(>1.5x median)={}",
                    stats.rate,
                    stats.min as f64 / 1e4,
                    stats.median as f64 / 1e4,
                    stats.max as f64 / 1e4,
                    stats.long
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_60_fps_has_no_long_intervals() {
        let times: Vec<i64> = (0..60).map(|i| i * 166_667).collect();
        let stats = interval_stats(&times, 60 * 166_667).unwrap();
        assert!((stats.rate - 60.0).abs() < 0.01, "{}", stats.rate);
        assert_eq!((stats.min, stats.max, stats.long), (166_667, 166_667, 0));
    }

    #[test]
    fn holes_are_counted_and_lower_the_rate() {
        // 30 frames, then a 100 ms hole, then 30 more.
        let mut times: Vec<i64> = (0..30).map(|i| i * 166_667).collect();
        times.extend((0..30).map(|i| 30 * 166_667 + 1_000_000 + i * 166_667));
        let stats = interval_stats(&times, 0).unwrap();
        assert_eq!(stats.long, 1);
        assert_eq!(stats.max, 166_667 + 1_000_000);
        assert!(stats.rate < 57.0, "{}", stats.rate);
        assert!(interval_stats(&times[..1], 0).is_none());
    }
}
