//! Phase 1 boundary for the future platform subsystem. No runtime backend is implemented.
#![forbid(unsafe_code)]

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;
