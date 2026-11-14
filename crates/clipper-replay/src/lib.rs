//! Instant replay engine: bounded hybrid RAM/SSD buffer of already encoded
//! packets and lossless clip export (remux, never re-encode).
//!
//! Platform independent and UI free. The recorder feeds [`EncodedPacket`]s
//! from the hardware encoder into a [`hybrid::ReplayBuffer`]; `SaveReplay`
//! takes a cheap [`hybrid::ReplaySnapshot`] (reference counted, no payload
//! copies) and [`export::Exporter`] writes it to MP4 on a background thread.
//!
//! Module ownership (see `docs/replay-architecture.md`):
//! - [`packet`], [`gop`], [`error`]: shared contract.
//! - [`ram`], [`hybrid`]: RAM tier, spill coordination and snapshots.
//! - [`disk`]: rotating SSD segments, cleanup, free-space checks.
//! - [`h264`], [`plan`], [`mp4`], [`export`]: clip assembly and muxing.
#![deny(unsafe_code)]

pub mod disk;
pub mod error;
pub mod export;
pub mod gop;
pub mod h264;
pub mod hybrid;
pub mod mp4;
pub mod packet;
pub mod plan;
pub mod ram;
pub mod track_names;

pub use error::ReplayError;
pub use packet::{CodecConfig, EncodedPacket, StreamLayout, TICKS_PER_SECOND, Ticks, TrackInfo};
