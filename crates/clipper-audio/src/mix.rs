//! Level metering, channel conversion and mixing of aligned tracks.
//! Allocation-free; safe to call on audio threads.

use crate::types::AudioLevels;

/// Peak and RMS over interleaved samples (all channels together).
///
/// Non-finite samples (NaN, infinities) are ignored: they count neither
/// towards the peak nor towards the RMS mean. Empty or all-invalid input
/// yields zeros.
pub fn measure(samples: &[f32]) -> AudioLevels {
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f64;
    let mut count = 0usize;
    for &s in samples {
        if !s.is_finite() {
            continue;
        }
        let a = s.abs();
        if a > peak {
            peak = a;
        }
        sum_sq += f64::from(s) * f64::from(s);
        count += 1;
    }
    if count == 0 {
        return AudioLevels::default();
    }
    AudioLevels {
        peak,
        rms: (sum_sq / count as f64).sqrt() as f32,
    }
}

/// Adds `input` (interleaved, `in_channels`) into stereo `out` with `gain`.
/// Mono is copied to both channels; >2 channels are folded L/R. Processes
/// `min(out.len() / 2, input.len() / in_channels)` frames and returns it.
///
/// Fold for >2 channels: even-indexed channels sum into L, odd-indexed into
/// R, both scaled by `2 / in_channels` (so 6 channels: each side is the
/// mean of 3 channels).
pub fn mix_into_stereo(out: &mut [f32], input: &[f32], in_channels: u16, gain: f32) -> usize {
    let ch = usize::from(in_channels);
    if ch == 0 {
        return 0;
    }
    let frames = (out.len() / 2).min(input.len() / ch);
    let out = &mut out[..frames * 2];
    let input = &input[..frames * ch];
    match ch {
        1 => {
            for (o, &s) in out.chunks_exact_mut(2).zip(input) {
                let v = s * gain;
                o[0] += v;
                o[1] += v;
            }
        }
        2 => {
            for (o, i) in out.chunks_exact_mut(2).zip(input.chunks_exact(2)) {
                o[0] += i[0] * gain;
                o[1] += i[1] * gain;
            }
        }
        _ => {
            let scale = gain * 2.0 / ch as f32;
            for (o, frame) in out.chunks_exact_mut(2).zip(input.chunks_exact(ch)) {
                let (mut l, mut r) = (0.0f32, 0.0f32);
                for pair in frame.chunks(2) {
                    l += pair[0];
                    if let Some(&v) = pair.get(1) {
                        r += v;
                    }
                }
                o[0] += l * scale;
                o[1] += r * scale;
            }
        }
    }
    frames
}

/// Hard-limits a mixed buffer to [-1, 1] after summing tracks. NaN becomes 0.
pub fn clamp(buffer: &mut [f32]) {
    for s in buffer {
        *s = if s.is_nan() { 0.0 } else { s.clamp(-1.0, 1.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48_000.0).sin() as f32)
            .collect()
    }

    #[test]
    fn measure_empty_and_nan() {
        assert_eq!(measure(&[]), AudioLevels::default());
        assert_eq!(measure(&[f32::NAN]), AudioLevels::default());
        let l = measure(&[0.5, f32::NAN, -1.0, f32::INFINITY]);
        assert_eq!(l.peak, 1.0);
        assert!((l.rms - (1.25f32 / 2.0).sqrt()).abs() < 1e-6);
    }

    #[test]
    fn measure_full_scale_sine() {
        let l = measure(&sine(48_000));
        assert!((l.peak - 1.0).abs() < 1e-3, "{l:?}");
        assert!((l.rms - 0.707).abs() < 0.005, "{l:?}");
    }

    #[test]
    fn mixing_disjoint_tracks_sums_exactly() {
        let mut out = vec![0.0f32; 8];
        let a = [0.25, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0];
        let b = [0.0, 0.0, 0.0, 0.0, 0.125, -0.5, 0.75, 0.25];
        assert_eq!(mix_into_stereo(&mut out, &a, 2, 1.0), 4);
        assert_eq!(mix_into_stereo(&mut out, &b, 2, 1.0), 4);
        let expect: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        assert_eq!(out, expect);
    }

    #[test]
    fn mono_upmix_with_gain() {
        let mut out = vec![0.0f32; 6];
        assert_eq!(mix_into_stereo(&mut out, &[0.5, -0.25, 1.0], 1, 0.5), 3);
        assert_eq!(out, [0.25, 0.25, -0.125, -0.125, 0.5, 0.5]);
    }

    #[test]
    fn six_channel_fold() {
        // L,R,C,LFE,Ls,Rs layout treated as even->L, odd->R.
        let mut out = vec![0.0f32; 2];
        let input = [0.3, 0.6, 0.3, 0.6, 0.3, 0.6];
        assert_eq!(mix_into_stereo(&mut out, &input, 6, 1.0), 1);
        assert!((out[0] - 0.3).abs() < 1e-6 && (out[1] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn frame_count_limited_and_zero_channels() {
        let mut out = vec![0.0f32; 4];
        assert_eq!(mix_into_stereo(&mut out, &[1.0; 10], 1, 1.0), 2);
        assert_eq!(mix_into_stereo(&mut out, &[1.0; 10], 0, 1.0), 0);
        let mut out = vec![0.0f32; 20];
        assert_eq!(mix_into_stereo(&mut out, &[1.0; 5], 2, 1.0), 2);
    }

    #[test]
    fn clamp_limits_and_zeroes_nan() {
        let mut b = [2.0, -3.0, 0.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
        clamp(&mut b);
        assert_eq!(b, [1.0, -1.0, 0.5, 0.0, 1.0, -1.0]);
    }
}
