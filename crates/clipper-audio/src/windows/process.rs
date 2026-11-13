//! WASAPI process loopback (include / exclude a process tree).
//! CONTRACT STUB — owned by the process audio agent.

use super::stream::StreamSource;
use crate::types::{AudioCapabilities, ProcessTarget};

/// Probes process loopback support on this Windows build.
pub fn capabilities() -> AudioCapabilities {
    todo!("process agent")
}

/// Root process id of `target` (for executables: the instance whose parent
/// is not the same executable), if running.
pub fn find_process(_target: &ProcessTarget) -> Option<u32> {
    todo!("process agent")
}

/// Captures only the target's process tree; survives restarts of
/// `ProcessTarget::Executable` targets.
pub(crate) fn include_source(_target: ProcessTarget) -> Box<dyn StreamSource> {
    todo!("process agent")
}

/// Captures everything except the target's process tree. While the target is
/// not running this is exactly the full mix, so it uses endpoint loopback and
/// switches to exclude mode once the target appears.
pub(crate) fn exclude_source(_target: ProcessTarget) -> Box<dyn StreamSource> {
    todo!("process agent")
}
