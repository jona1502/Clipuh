//! Smooth, accurate playhead presentation.
//!
//! The player stays the single authoritative clock: every decoded-frame and
//! position event re-anchors this model. Between those events (which arrive
//! at the video frame rate, with jitter, and stall during seeks) the
//! displayed position advances with a monotonic clock, so the playhead can be
//! drawn on every display refresh instead of jumping once per video frame.
//!
//! Small disagreements with the player are corrected gradually (no visible
//! jumps, never backwards while playing); large ones (seek, stall, segment
//! jump) snap immediately. While paused the authoritative position is shown
//! unchanged.

use std::cell::Cell;
use std::time::{Duration, Instant};

/// Disagreement above which the playhead snaps to the player position.
const SNAP: Duration = Duration::from_millis(150);
/// Share of a small error removed per authoritative update. At ~60 updates
/// per second a 10 ms error is gone after about a quarter second.
const CORRECTION: f64 = 0.15;
/// Longest extrapolation past the last authoritative update. Beyond it the
/// playback has stalled (decoder hiccup, seek) and the playhead waits.
const MAX_EXTRAPOLATION: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
pub(super) struct PlayheadClock {
    /// Media position at `at`, while playing.
    anchor: Option<(Duration, Instant)>,
    /// Instant of the last authoritative update, while playing.
    synced_at: Option<Instant>,
    /// Last displayed position (keeps the playhead monotonic while playing).
    shown: Cell<Duration>,
}

impl PlayheadClock {
    /// Not playing (paused, ended, seeking while paused): show `position`.
    pub(super) fn stop(&mut self, position: Duration) {
        self.anchor = None;
        self.synced_at = None;
        self.shown.set(position);
    }

    /// A discontinuity while playing (seek, segment jump): restart at
    /// `position`, even backwards.
    pub(super) fn jump(&mut self, position: Duration, now: Instant) {
        self.anchor = Some((position, now));
        self.synced_at = Some(now);
        self.shown.set(position);
    }

    /// Authoritative player position while playing.
    pub(super) fn sync(&mut self, position: Duration, now: Instant) {
        // Predict with the previous update's extrapolation limit.
        let predicted = self.predict(now);
        self.synced_at = Some(now);
        let Some(predicted) = predicted else {
            self.jump(position, now);
            return;
        };
        if position.abs_diff(predicted) > SNAP {
            self.jump(position, now);
            return;
        }
        let error = position.as_secs_f64() - predicted.as_secs_f64();
        let corrected = (predicted.as_secs_f64() + error * CORRECTION).max(0.);
        self.anchor = Some((Duration::from_secs_f64(corrected), now));
    }

    fn predict(&self, now: Instant) -> Option<Duration> {
        let (position, at) = self.anchor?;
        let synced = self.synced_at.unwrap_or(at);
        // Extrapolate only a bounded time past the last player update.
        let horizon = synced + MAX_EXTRAPOLATION;
        let until = now.min(horizon).max(at);
        Some(position + (until - at))
    }

    /// Position to draw now. While playing: interpolated, monotonic and never
    /// past `limit` (end of the playing segment). Otherwise the last value
    /// set by [`Self::stop`].
    pub(super) fn display(&self, now: Instant, limit: Duration) -> Duration {
        let shown = self.shown.get();
        let Some(predicted) = self.predict(now) else {
            return shown;
        };
        let value = predicted.max(shown).min(limit.max(shown));
        self.shown.set(value);
        value
    }

    pub(super) fn is_running(&self) -> bool {
        self.anchor.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    const FAR: Duration = Duration::from_secs(3600);

    #[test]
    fn paused_shows_the_authoritative_position() {
        let mut clock = PlayheadClock::default();
        clock.stop(ms(1234));
        let t = Instant::now();
        assert_eq!(clock.display(t, FAR), ms(1234));
        assert_eq!(clock.display(t + ms(500), FAR), ms(1234));
        assert!(!clock.is_running());
    }

    #[test]
    fn interpolates_between_player_updates() {
        let mut clock = PlayheadClock::default();
        let t0 = Instant::now();
        clock.sync(ms(1000), t0);
        // A 180 Hz display draws several times per 60 fps video frame.
        assert_eq!(clock.display(t0 + ms(5), FAR), ms(1005));
        assert_eq!(clock.display(t0 + ms(11), FAR), ms(1011));
        assert_eq!(clock.display(t0 + ms(16), FAR), ms(1016));
    }

    #[test]
    fn small_errors_are_corrected_gradually_and_never_backwards() {
        let mut clock = PlayheadClock::default();
        let t0 = Instant::now();
        clock.sync(ms(1000), t0);
        let before = clock.display(t0 + ms(17), FAR);
        // Player reports 10 ms behind the prediction.
        clock.sync(ms(1007), t0 + ms(17));
        let after = clock.display(t0 + ms(17), FAR);
        assert!(after >= before, "moved backwards: {after:?} < {before:?}");
        // Converges onto the player clock over subsequent updates.
        let mut now = t0 + ms(17);
        let mut player = ms(1007);
        for _ in 0..60 {
            now += ms(16);
            player += ms(16);
            clock.sync(player, now);
        }
        let shown = clock.display(now, FAR);
        assert!(shown.abs_diff(player) <= ms(11), "{shown:?} vs {player:?}");
        let predicted = clock.predict(now).unwrap();
        assert!(
            predicted.abs_diff(player) < ms(1),
            "{predicted:?} vs {player:?}"
        );
    }

    #[test]
    fn large_disagreement_snaps() {
        let mut clock = PlayheadClock::default();
        let t0 = Instant::now();
        clock.sync(ms(1000), t0);
        clock.sync(ms(5000), t0 + ms(16));
        assert_eq!(clock.display(t0 + ms(16), FAR), ms(5000));
    }

    #[test]
    fn jump_moves_backwards_for_seeks_and_segment_jumps() {
        let mut clock = PlayheadClock::default();
        let t0 = Instant::now();
        clock.sync(ms(9000), t0);
        assert_eq!(clock.display(t0 + ms(10), FAR), ms(9010));
        clock.jump(ms(2000), t0 + ms(10));
        assert_eq!(clock.display(t0 + ms(10), FAR), ms(2000));
        assert_eq!(clock.display(t0 + ms(20), FAR), ms(2010));
    }

    #[test]
    fn never_runs_past_the_segment_end() {
        let mut clock = PlayheadClock::default();
        let t0 = Instant::now();
        clock.sync(ms(4990), t0);
        assert_eq!(clock.display(t0 + ms(50), ms(5000)), ms(5000));
    }

    #[test]
    fn waits_when_the_player_stalls() {
        let mut clock = PlayheadClock::default();
        let t0 = Instant::now();
        clock.sync(ms(1000), t0);
        // No update for 2 s: extrapolation stops after MAX_EXTRAPOLATION.
        assert_eq!(
            clock.display(t0 + ms(2000), FAR),
            ms(1000) + MAX_EXTRAPOLATION
        );
        // Playback resumes: the playhead holds (never backwards) until the
        // player clock passes it, then follows it again.
        clock.sync(ms(1100), t0 + ms(2000));
        assert_eq!(clock.display(t0 + ms(2000), FAR), ms(1250));
        let mut now = t0 + ms(2000);
        let mut player = ms(1100);
        for _ in 0..60 {
            now += ms(16);
            player += ms(16);
            clock.sync(player, now);
        }
        assert!(clock.display(now, FAR).abs_diff(player) < ms(5));
    }

    #[test]
    fn stop_after_playing_shows_the_pause_position() {
        let mut clock = PlayheadClock::default();
        let t0 = Instant::now();
        clock.sync(ms(1000), t0);
        let _ = clock.display(t0 + ms(30), FAR);
        clock.stop(ms(1020));
        assert_eq!(clock.display(t0 + ms(100), FAR), ms(1020));
    }
}
