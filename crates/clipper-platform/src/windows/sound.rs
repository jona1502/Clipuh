//! Notification sound playback (WinMM `PlaySoundW` from memory).
//!
//! `PlaySoundW` with `SND_ASYNC | SND_MEMORY` returns immediately and plays
//! on the system's sound thread: no thread, decoder or device of our own,
//! nothing running while idle. A new sound replaces the one still playing,
//! so rapid saves never stack up overlapping sounds; starts closer together
//! than [`MIN_INTERVAL`] are skipped. The sound goes to the default output
//! device only and is never mixed into recorded tracks by clipuh itself
//! (a desktop-loopback capture can still hear it, like any system sound).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT, SND_PURGE};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::WindowsAndMessaging::MB_ICONHAND;
use windows::core::PCWSTR;

/// Shortest time between two sound starts.
pub const MIN_INTERVAL: Duration = Duration::from_millis(250);

/// Skips starts that come faster than `interval` (pure; unit tested).
#[derive(Debug)]
pub struct RateLimit {
    interval: Duration,
    last: Option<Instant>,
}

impl RateLimit {
    pub const fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// Whether a sound may start at `now` (and records it if so).
    pub fn admit(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < self.interval)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

struct Player {
    limit: RateLimit,
    /// The WAV being played: `SND_MEMORY | SND_ASYNC` reads it until the
    /// sound ends, so it stays alive until the next sound replaces it.
    playing: Option<Arc<[u8]>>,
}

static PLAYER: Mutex<Player> = Mutex::new(Player {
    limit: RateLimit::new(MIN_INTERVAL),
    playing: None,
});

#[derive(Debug, PartialEq, Eq)]
pub enum PlayError {
    /// Too soon after the previous sound; nothing was played.
    RateLimited,
    /// No output device or the data is not a playable WAV.
    Unavailable,
}

/// Plays a complete RIFF/WAVE file from memory, asynchronously.
pub fn play_wav(wav: Arc<[u8]>) -> Result<(), PlayError> {
    if !looks_like_wav(&wav) {
        return Err(PlayError::Unavailable);
    }
    let mut player = PLAYER.lock().unwrap_or_else(PoisonError::into_inner);
    if !player.limit.admit(Instant::now()) {
        return Err(PlayError::RateLimited);
    }
    // SAFETY: with SND_MEMORY the "name" is a pointer to the WAV image.
    // `wav` is stored in `player.playing` below and kept until the next call,
    // which first replaces the playing sound (PlaySoundW stops the previous
    // asynchronous sound before it returns), so WinMM never reads freed data.
    let started = unsafe {
        PlaySoundW(
            PCWSTR(wav.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    }
    .as_bool();
    player.playing = Some(wav);
    if started {
        Ok(())
    } else {
        Err(PlayError::Unavailable)
    }
}

/// Stops a playing notification sound (shutdown) and releases its data.
pub fn stop() {
    let mut player = PLAYER.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: SND_PURGE with a null name stops this process' sounds.
    let _ = unsafe { PlaySoundW(PCWSTR::null(), None, SND_PURGE) };
    player.playing = None;
}

/// The system error sound (failed saves); returns immediately.
pub fn play_error() {
    // SAFETY: no pointers.
    let _ = unsafe { MessageBeep(MB_ICONHAND) };
}

fn looks_like_wav(data: &[u8]) -> bool {
    data.len() > 44 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_skips_starts_closer_than_the_interval() {
        let mut limit = RateLimit::new(Duration::from_millis(250));
        let t0 = Instant::now();
        assert!(limit.admit(t0));
        assert!(!limit.admit(t0 + Duration::from_millis(100)));
        assert!(!limit.admit(t0 + Duration::from_millis(249)));
        assert!(limit.admit(t0 + Duration::from_millis(250)));
        assert!(!limit.admit(t0 + Duration::from_millis(300)));
        assert!(limit.admit(t0 + Duration::from_secs(5)));
    }

    #[test]
    fn non_wav_data_is_refused_without_calling_winmm() {
        assert_eq!(
            play_wav(Arc::from(&b"not a wave file at all, just bytes......."[..])),
            Err(PlayError::Unavailable)
        );
    }

    #[test]
    #[ignore = "plays a sound on the default output device"]
    fn plays_a_short_tone() {
        let wav = tone_wav();
        assert_eq!(play_wav(Arc::from(wav)), Ok(()));
        std::thread::sleep(Duration::from_millis(400));
        stop();
    }

    fn tone_wav() -> Vec<u8> {
        let rate = 48_000u32;
        let frames: Vec<i16> = (0..rate / 5)
            .map(|i| ((i as f32 * 880. * std::f32::consts::TAU / rate as f32).sin() * 6000.) as i16)
            .collect();
        let data_len = (frames.len() * 2) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for sample in frames {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }
}
