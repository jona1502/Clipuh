//! Clip playback and non-destructive trimming for the editor, built on Media
//! Foundation (hardware decode, no FFmpeg, no WebView).

#[cfg(windows)]
pub mod player;
#[cfg(windows)]
pub mod trim;
