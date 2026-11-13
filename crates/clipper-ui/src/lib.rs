#![cfg(windows)]
#![forbid(unsafe_code)]

mod components;
mod notifications;
mod state;
mod theme;
mod views;

pub use views::{ClipperWindow, bind_keys};
