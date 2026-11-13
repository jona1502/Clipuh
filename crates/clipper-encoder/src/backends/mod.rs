//! Hardware encoder backends.

#[cfg(target_os = "windows")]
pub mod media_foundation;
#[cfg(target_os = "windows")]
pub mod packet_encoder;
