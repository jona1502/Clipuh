//! Level metering, channel conversion and mixing of aligned tracks.
//! Allocation-free; safe to call on audio threads. CONTRACT STUB.

use crate::types::AudioLevels;

/// Peak and RMS over interleaved samples (all channels together).
pub fn measure(_samples: &[f32]) -> AudioLevels {
    todo!("sync agent")
}

/// Adds `input` (interleaved, `in_channels`) into stereo `out` with `gain`.
/// Mono is copied to both channels; >2 channels are folded L/R. Processes
/// `min(out.len() / 2, input.len() / in_channels)` frames and returns it.
pub fn mix_into_stereo(_out: &mut [f32], _input: &[f32], _in_channels: u16, _gain: f32) -> usize {
    todo!("sync agent")
}

/// Hard-limits a mixed buffer to [-1, 1] after summing tracks.
pub fn clamp(_buffer: &mut [f32]) {
    todo!("sync agent")
}
