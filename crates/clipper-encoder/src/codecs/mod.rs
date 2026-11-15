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
    /// Optional encoder knobs; `None` leaves the encoder's own default.
    /// `CODECAPI_AVEncVideoMaxNumRefFrame`.
    pub max_ref_frames: Option<u32>,
    /// `CODECAPI_AVEncH264CABACEnable` (false = CAVLC).
    pub cabac: Option<bool>,
    /// Slices per frame given as macroblock rows per slice
    /// (`CODECAPI_AVEncSliceControlMode` = 2 plus `..SliceControlSize`).
    pub slice_mb_rows: Option<u32>,
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
                max_ref_frames: None,
                cabac: None,
                slice_mb_rows: None,
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
                max_ref_frames: None,
                cabac: None,
                slice_mb_rows: None,
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
                max_ref_frames: None,
                cabac: None,
                slice_mb_rows: None,
            },
        }
    }
}

/// PCI vendor id of AMD hardware encoders, as Media Foundation reports it in
/// `MFT_ENUM_HARDWARE_VENDOR_ID_Attribute`.
pub const AMD_VENDOR_ID: &str = "VEN_1002";

/// Highest mean bitrate the encoder is configured with (as `bitrate_bps`).
const MAX_BITRATE_BPS: u32 = 150_000_000;

impl H264Tuning {
    /// The tuning for the encoder of `vendor` (`VEN_xxxx`). Only AMD's
    /// Quality profile changes; every other combination is `self`.
    ///
    /// On AMD (VCN, measured on an RX 9070 XT; `docs/performance/
    /// replay-gpu-load.md`) Quality's QvS 100 + 2 B-frames cost 2.2x the
    /// encoder time of QvS 50 without B-frames, while bitrate costs no
    /// encoder time. QvS 50, no B-frames and 1.25x the bitrate scored higher
    /// SSIM/PSNR than the shipped Quality on real gameplay (equal on UI
    /// scenes) for 55 % less encoder time, at ~+20-30 % file size. Other
    /// vendors' encoders were not measured, so they keep the profile as is.
    pub fn for_vendor(self, profile: QualityProfile, vendor: Option<&str>) -> Self {
        let amd = vendor.is_some_and(|vendor| vendor.eq_ignore_ascii_case(AMD_VENDOR_ID));
        if !amd || profile != QualityProfile::Quality {
            return self;
        }
        let bitrate_bps = self
            .bitrate_bps
            .saturating_add(self.bitrate_bps / 4)
            .min(MAX_BITRATE_BPS);
        Self {
            bitrate_bps,
            peak_bitrate_bps: bitrate_bps.saturating_mul(2),
            quality_vs_speed: 50,
            b_frames: 0,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amd_quality_spends_bits_instead_of_encoder_time() {
        let quality = H264Tuning::new(QualityProfile::Quality, 16, 60);
        let amd = quality.for_vendor(QualityProfile::Quality, Some("VEN_1002"));
        assert_eq!(amd.quality_vs_speed, 50);
        assert_eq!(amd.b_frames, 0);
        assert_eq!(amd.bitrate_bps, 30_000_000);
        assert_eq!(amd.peak_bitrate_bps, 60_000_000);
        assert_eq!(
            (amd.rate_control, amd.profile, amd.gop_frames),
            (quality.rate_control, quality.profile, quality.gop_frames)
        );
        // Lower-case ids and the bitrate cap.
        let capped = H264Tuning::new(QualityProfile::Quality, 100, 60)
            .for_vendor(QualityProfile::Quality, Some("ven_1002"));
        assert_eq!(capped.bitrate_bps, MAX_BITRATE_BPS);
    }

    #[test]
    fn other_vendors_and_profiles_are_unchanged() {
        for profile in QualityProfile::ALL {
            let tuning = H264Tuning::new(profile, 16, 60);
            for vendor in [Some("VEN_10DE"), Some("VEN_8086"), None] {
                assert_eq!(tuning.for_vendor(profile, vendor), tuning);
            }
            if profile != QualityProfile::Quality {
                assert_eq!(tuning.for_vendor(profile, Some(AMD_VENDOR_ID)), tuning);
            }
        }
    }

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
