//! Hardware video encoding. Windows uses Media Foundation hardware MFTs
//! (NVENC, AMF, Quick Sync) and never falls back to software encoding.
//!
//! `unsafe` is denied crate-wide and allowed only inside the Windows FFI
//! backend, where each call documents its invariants.
#![deny(unsafe_code)]

pub mod backends;
pub mod codecs;
