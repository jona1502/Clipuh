//! Endpoint loopback (default render device) and microphone sources.
//! CONTRACT STUB — owned by the WASAPI agent.

use super::stream::{OpenedStream, StreamSource};
use crate::types::{AudioError, MicrophoneChoice};

/// Loopback of the default render device; follows default-device changes.
pub(crate) fn loopback_source() -> Box<dyn StreamSource> {
    todo!("WASAPI agent")
}

/// Opens loopback of the current default render device once. Used by the
/// process EXCLUDE source while its target is not running.
pub(crate) fn open_default_loopback(_channels: u16) -> Result<OpenedStream, AudioError> {
    todo!("WASAPI agent")
}

/// Microphone capture; `Default` follows default-device changes.
pub(crate) fn microphone_source(_choice: MicrophoneChoice) -> Box<dyn StreamSource> {
    todo!("WASAPI agent")
}

/// Channel count to request for a microphone: its mix format channels,
/// clamped to 1..=2 (2 if unknown).
pub(crate) fn microphone_channels(_choice: &MicrophoneChoice) -> u16 {
    todo!("WASAPI agent")
}
