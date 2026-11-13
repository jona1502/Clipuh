//! Platform-independent H.264 tuning derived from a [`QualityProfile`].
//!
//! Backends translate [`H264Tuning`] into encoder-specific properties and
//! skip properties the active hardware encoder rejects.

use clipper_core::recording::QualityProfile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateControl {
    /// Constant bitrate: predictable size and latency.
    Cbr,
    /// Peak-constrained variable bitrate: better quality per bit.
    PeakConstrainedVbr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum H264Profile {
    Main,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct H264Tuning {
    pub rate_control: RateControl,
    /// Average target bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Peak bitrate for VBR in bits per second (equals average for CBR).
    pub peak_bitrate_bps: u32,
    /// 0 = fastest, 100 = best quality (`CODECAPI_AVEncCommonQualityVsSpeed`).
    pub quality_vs_speed: u32,
    /// `CODECAPI_AVLowLatencyMode`. Off for every profile: file recording
    /// gains nothing from it, and with the AMD H.264 MFT it made the encoder
    /// hold input samples until the 4-texture pool starved (measured: 1080p60
    /// fell to ~2 fps with >50% dropped frames; 0 drops with it off).
    pub low_latency: bool,
    pub profile: H264Profile,
    /// Keyframe interval in frames. Two seconds keeps seeking and later
    /// replay-buffer trimming cheap.
    pub gop_frames: u32,
    /// B-frames add latency and are not supported by every hardware encoder.
    pub b_frames: u32,
}

impl H264Tuning {
    pub fn new(profile: QualityProfile, base_bitrate_mbps: u32, fps: u32) -> Self {
        let bitrate_bps = profile.bitrate_bps(base_bitrate_mbps);
        let gop_frames = fps.max(1).saturating_mul(2);
        match profile {
            QualityProfile::Performance => Self {
                rate_control: RateControl::Cbr,
                bitrate_bps,
                peak_bitrate_bps: bitrate_bps,
                quality_vs_speed: 0,
                low_latency: false,
                profile: H264Profile::Main,
                gop_frames,
                b_frames: 0,
            },
            QualityProfile::Balanced => Self {
                rate_control: RateControl::PeakConstrainedVbr,
                bitrate_bps,
                peak_bitrate_bps: bitrate_bps.saturating_add(bitrate_bps / 2),
                quality_vs_speed: 50,
                low_latency: false,
                profile: H264Profile::High,
                gop_frames,
                b_frames: 0,
            },
            QualityProfile::Quality => Self {
                rate_control: RateControl::PeakConstrainedVbr,
                bitrate_bps,
                peak_bitrate_bps: bitrate_bps.saturating_mul(2),
                quality_vs_speed: 100,
                low_latency: false,
                profile: H264Profile::High,
                gop_frames,
                b_frames: 2,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_is_fast_cbr_without_b_frames() {
        let tuning = H264Tuning::new(QualityProfile::Performance, 16, 60);
        assert_eq!(tuning.rate_control, RateControl::Cbr);
        assert_eq!(tuning.bitrate_bps, 16_000_000);
        assert_eq!(tuning.peak_bitrate_bps, tuning.bitrate_bps);
        assert_eq!(tuning.quality_vs_speed, 0);
        assert!(!tuning.low_latency);
        assert_eq!(tuning.b_frames, 0);
        assert_eq!(tuning.gop_frames, 120);
    }

    #[test]
    fn quality_raises_bitrate_and_quality_setting() {
        let balanced = H264Tuning::new(QualityProfile::Balanced, 16, 60);
        let quality = H264Tuning::new(QualityProfile::Quality, 16, 60);
        assert!(quality.bitrate_bps > balanced.bitrate_bps);
        assert!(quality.quality_vs_speed > balanced.quality_vs_speed);
        assert!(quality.peak_bitrate_bps >= quality.bitrate_bps);
        assert_eq!(quality.profile, H264Profile::High);
    }

    #[test]
    fn gop_is_two_seconds_at_any_fps() {
        for fps in [30, 60, 120] {
            assert_eq!(
                H264Tuning::new(QualityProfile::Balanced, 16, fps).gop_frames,
                fps * 2
            );
        }
    }
}
