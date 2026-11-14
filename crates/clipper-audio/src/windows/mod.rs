//! WASAPI backends (shared mode, event-driven) for Windows 10/11.
#![allow(unsafe_code)]

mod apps;
mod devices;
mod endpoint;
mod engine;
mod process;
mod stream;

pub use apps::{AudioApplication, executable_of_window, list_audio_applications};
pub use devices::list_devices;
pub use engine::{AudioCapture, CaptureSummary, qpc_now_ticks};
pub use process::{capabilities, find_process};
