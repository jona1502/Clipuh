//! Audio endpoint enumeration. CONTRACT STUB — owned by the WASAPI agent.

use crate::types::{AudioDevice, AudioError, DeviceFlow};

/// Active render or capture endpoints with their shared-mode mix format.
pub fn list_devices(_flow: DeviceFlow) -> Result<Vec<AudioDevice>, AudioError> {
    todo!("WASAPI agent")
}
