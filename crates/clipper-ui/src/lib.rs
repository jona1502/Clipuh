#![cfg(windows)]
#![forbid(unsafe_code)]

mod components;
mod state;
mod theme;
mod views;

pub use views::ClipperWindow;
