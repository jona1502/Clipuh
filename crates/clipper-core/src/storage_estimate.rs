/// The result of estimating encoded payload and configurable storage overhead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeEstimate {
    pub payload_bytes: u64,
    pub overhead_bytes: u64,
    pub total_bytes: u64,
}

/// Failure while converting a bitrate and duration into a storage estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstimateError {
    InvalidOverheadBasisPoints(u16),
    ArithmeticOverflow,
    SizeOverflow,
}

impl std::fmt::Display for EstimateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOverheadBasisPoints(value) => write!(
                f,
                "overhead must be between 0 and 10000 basis points, got {value}"
            ),
            Self::ArithmeticOverflow => f.write_str("storage size arithmetic overflowed"),
            Self::SizeOverflow => f.write_str("estimated storage size exceeds u64 bytes"),
        }
    }
}

impl std::error::Error for EstimateError {}

/// Estimate storage from video and audio bitrates, duration, and overhead.
///
/// Bitrates are in bits per second, duration is in seconds, and overhead is in
/// basis points relative to the rounded payload size. Partial bytes and
/// positive fractional overhead bytes are rounded up.
pub fn estimate_size(
    video_bps: u64,
    audio_bps: u64,
    seconds: u64,
    overhead_basis_points: u16,
) -> Result<SizeEstimate, EstimateError> {
    if overhead_basis_points > 10_000 {
        return Err(EstimateError::InvalidOverheadBasisPoints(
            overhead_basis_points,
        ));
    }

    let total_bps = u128::from(video_bps)
        .checked_add(u128::from(audio_bps))
        .ok_or(EstimateError::ArithmeticOverflow)?;
    let total_bits = total_bps
        .checked_mul(u128::from(seconds))
        .ok_or(EstimateError::ArithmeticOverflow)?;
    let payload_bytes = div_ceil(total_bits, 8)?;
    let overhead_numerator = payload_bytes
        .checked_mul(u128::from(overhead_basis_points))
        .ok_or(EstimateError::ArithmeticOverflow)?;
    let overhead_bytes = div_ceil(overhead_numerator, 10_000)?;
    let total_bytes = payload_bytes
        .checked_add(overhead_bytes)
        .ok_or(EstimateError::ArithmeticOverflow)?;

    Ok(SizeEstimate {
        payload_bytes: u64::try_from(payload_bytes).map_err(|_| EstimateError::SizeOverflow)?,
        overhead_bytes: u64::try_from(overhead_bytes).map_err(|_| EstimateError::SizeOverflow)?,
        total_bytes: u64::try_from(total_bytes).map_err(|_| EstimateError::SizeOverflow)?,
    })
}

fn div_ceil(value: u128, divisor: u128) -> Result<u128, EstimateError> {
    let quotient = value / divisor;
    if value.is_multiple_of(divisor) {
        Ok(quotient)
    } else {
        quotient
            .checked_add(1)
            .ok_or(EstimateError::ArithmeticOverflow)
    }
}

/// Estimated placement of a total storage size across RAM and SSD budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HybridEstimate {
    pub total_bytes: u64,
    pub ram_budget_bytes: u64,
    pub estimated_ram_bytes: u64,
    pub estimated_ssd_bytes: u64,
}

/// Split a total estimate into its RAM and SSD portions.
pub fn estimate_hybrid(total_bytes: u64, ram_budget_bytes: u64) -> HybridEstimate {
    HybridEstimate {
        total_bytes,
        ram_budget_bytes,
        estimated_ram_bytes: total_bytes.min(ram_budget_bytes),
        estimated_ssd_bytes: total_bytes.saturating_sub(ram_budget_bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::{EstimateError, estimate_hybrid, estimate_size};

    #[test]
    fn storage_estimate_matches_exact_payload_sizes() {
        assert_eq!(
            estimate_size(16_000_000, 0, 60, 0).unwrap().payload_bytes,
            120_000_000
        );
        assert_eq!(
            estimate_size(16_000_000, 3 * 128_000, 60, 0)
                .unwrap()
                .payload_bytes,
            122_880_000
        );
        assert_eq!(
            estimate_size(8_000_000, 256_000, 30, 0)
                .unwrap()
                .payload_bytes,
            30_960_000
        );
        assert_eq!(
            estimate_size(20_000_000, 256_000, 300, 0)
                .unwrap()
                .payload_bytes,
            759_600_000
        );
    }

    #[test]
    fn storage_estimate_zero_rates_or_duration_has_zero_size() {
        for (video_bps, audio_bps, seconds) in [(0, 0, 100), (0, 10_000, 0)] {
            assert_eq!(
                estimate_size(video_bps, audio_bps, seconds, 10_000).unwrap(),
                super::SizeEstimate {
                    payload_bytes: 0,
                    overhead_bytes: 0,
                    total_bytes: 0,
                }
            );
        }
    }

    #[test]
    fn storage_estimate_rounds_payload_bits_up_to_a_byte() {
        assert_eq!(estimate_size(9, 0, 1, 0).unwrap().payload_bytes, 2);
    }

    #[test]
    fn storage_estimate_rounds_positive_overhead_up_to_a_byte() {
        assert_eq!(
            estimate_size(8, 0, 1, 1).unwrap(),
            super::SizeEstimate {
                payload_bytes: 1,
                overhead_bytes: 1,
                total_bytes: 2,
            }
        );
    }

    #[test]
    fn storage_estimate_accepts_overhead_bounds_and_rejects_values_above_them() {
        assert_eq!(estimate_size(8_000, 0, 1, 0).unwrap().overhead_bytes, 0);
        assert_eq!(
            estimate_size(8_000, 0, 1, 10_000).unwrap().overhead_bytes,
            1_000
        );
        assert_eq!(
            estimate_size(8_000, 0, 1, 10_001),
            Err(EstimateError::InvalidOverheadBasisPoints(10_001))
        );
    }

    #[test]
    fn storage_estimate_handles_large_valid_values_without_narrow_intermediate_overflow() {
        assert_eq!(
            estimate_size(u64::MAX, u64::MAX, 2, 0)
                .unwrap()
                .payload_bytes,
            9_223_372_036_854_775_808
        );
    }

    #[test]
    fn storage_estimate_reports_arithmetic_overflow() {
        assert_eq!(
            estimate_size(u64::MAX, u64::MAX, u64::MAX, 0),
            Err(EstimateError::ArithmeticOverflow)
        );
    }

    #[test]
    fn storage_estimate_reports_output_overflow() {
        assert_eq!(
            estimate_size(u64::MAX, u64::MAX, 3, 10_000),
            Err(EstimateError::SizeOverflow)
        );
    }

    #[test]
    fn storage_estimate_hybrid_places_fitting_size_in_ram() {
        assert_eq!(
            estimate_hybrid(100, 128),
            super::HybridEstimate {
                total_bytes: 100,
                ram_budget_bytes: 128,
                estimated_ram_bytes: 100,
                estimated_ssd_bytes: 0,
            }
        );
    }
}
