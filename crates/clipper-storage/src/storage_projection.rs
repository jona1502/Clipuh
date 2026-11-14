use std::error::Error;
use std::fmt;

use clipper_core::recording::AAC_TRACK_BITRATE_BPS;
use clipper_core::storage_estimate::{
    EstimateError, HybridEstimate, estimate_hybrid, estimate_size,
};
use clipper_encoder::codecs::{H264Tuning, RateControl};

use crate::config::{AppConfig, VideoCodec};

/// Storage estimate derived from the active encoder, audio tracks, and replay settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageProjection {
    pub video_bps: u64,
    pub audio_bps: u64,
    pub audio_tracks: u32,
    pub variable_bitrate: bool,
    /// Estimated file sizes for 30, 60, 120, and 300 second recordings.
    pub sizes: [u64; 4],
    pub replay_duration_seconds: u64,
    pub replay: HybridEstimate,
}

/// Failure while validating a config or estimating its storage use.
#[derive(Debug)]
pub enum StorageProjectionError {
    InvalidConfig(String),
    Estimate(EstimateError),
}

impl fmt::Display for StorageProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(formatter, "invalid configuration: {message}"),
            Self::Estimate(error) => write!(formatter, "could not estimate storage: {error}"),
        }
    }
}

impl Error for StorageProjectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidConfig(_) => None,
            Self::Estimate(error) => Some(error),
        }
    }
}

impl AppConfig {
    /// Projects recording storage use without resolving paths or accessing the filesystem.
    pub fn storage_projection(&self) -> Result<StorageProjection, StorageProjectionError> {
        self.validate()
            .map_err(|error| StorageProjectionError::InvalidConfig(error.to_string()))?;

        // Match exhaustively so adding a future codec requires defining its
        // actual rate source instead of silently reusing H.264 assumptions.
        let tuning = match self.video.codec {
            VideoCodec::H264 => H264Tuning::new(
                self.recording.quality.into(),
                self.video.bitrate_mbps,
                self.video.fps,
            ),
        };
        let video_bps = u64::from(tuning.bitrate_bps);
        let audio_tracks = self.audio.recording_options().encoded_track_count();
        let audio_bps = u64::from(audio_tracks) * u64::from(AAC_TRACK_BITRATE_BPS);
        // Current MP4 estimates exclude container overhead until it is measured.
        const CONTAINER_OVERHEAD_BASIS_POINTS: u16 = 0;

        let mut sizes = [0; 4];
        for (size, seconds) in sizes.iter_mut().zip([30, 60, 120, 300]) {
            *size = estimate_size(
                video_bps,
                audio_bps,
                seconds,
                CONTAINER_OVERHEAD_BASIS_POINTS,
            )
            .map_err(StorageProjectionError::Estimate)?
            .total_bytes;
        }

        let replay_duration_seconds = u64::from(self.replay.duration_seconds);
        let replay_total_bytes = estimate_size(
            video_bps,
            audio_bps,
            replay_duration_seconds,
            CONTAINER_OVERHEAD_BASIS_POINTS,
        )
        .map_err(StorageProjectionError::Estimate)?
        .total_bytes;
        let ram_budget_bytes = u64::from(self.replay.memory_limit_megabytes) * 1_048_576;

        Ok(StorageProjection {
            video_bps,
            audio_bps,
            audio_tracks,
            variable_bitrate: tuning.rate_control == RateControl::PeakConstrainedVbr,
            sizes,
            replay_duration_seconds,
            replay: estimate_hybrid(replay_total_bytes, ram_budget_bytes),
        })
    }
}
