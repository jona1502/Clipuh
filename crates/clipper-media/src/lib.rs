//! Clip playback and non-destructive trimming for the editor, built on Media
//! Foundation (hardware decode, no FFmpeg, no WebView).

#[cfg(windows)]
pub mod player;
#[cfg(windows)]
pub mod thumbnail;
#[cfg(windows)]
pub mod trim;

#[cfg(windows)]
pub mod exact;

#[cfg(windows)]
pub mod audio;
#[cfg(windows)]
mod audio_preview;

#[cfg(windows)]
pub mod notification_sound;
