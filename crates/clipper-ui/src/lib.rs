#![cfg(windows)]
#![forbid(unsafe_code)]

mod components;
mod feedback;
mod library_worker;
mod meter;
mod notification_center;
mod notifications;
mod recent_clips;
mod state;
mod storage_status;
mod theme;
mod views;

pub use theme::apply as apply_theme;
pub use views::{ClipperWindow, bind_keys};
