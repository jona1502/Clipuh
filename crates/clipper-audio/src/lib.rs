//! Audio capture engine.
//!
//! Pipeline: capture backends (one dedicated thread per stream, event-driven
//! WASAPI) -> bounded lock-free rings -> consumer (`read_frames`) ->
//! [`timeline`] alignment -> [`mix`] or separate tracks -> encoder (later).
//!
//! Routing ([`routing`]) guarantees that tracks never overlap, so mixing all
//! tracks never doubles a signal.
//!
//! `unsafe` is denied crate-wide and allowed only inside the Windows FFI
//! module and the documented spots that need it.
#![deny(unsafe_code)]

pub mod desktop;
pub mod gate;
pub mod live;
pub mod mix;
pub mod mixer;
pub mod ring;
pub mod routing;
pub mod timeline;
pub mod types;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;

pub use types::*;
