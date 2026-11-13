//! OS integration shared by capture and encoder backends.
//!
//! `unsafe` is denied crate-wide and allowed only inside the Windows FFI
//! module, where each call documents its invariants.
#![deny(unsafe_code)]

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;
