//! Audio meter math for the mixer: dBFS scale, time-based ballistics,
//! segment colour zones and gain labels. Pure functions; the view only draws.
//!
//! Levels are the engine's post-gain sample peaks (linear, 1.0 = 0 dBFS),
//! reported ~30 times a second as the highest peak since the last report.

use std::time::Duration;

/// Bottom of the meter scale; anything quieter shows as silence.
pub const FLOOR_DB: f32 = -60.0;
/// Segment zones (dBFS at the segment's centre): green below −12, yellow up
/// to −3, red from −3 (near clipping). Same thresholds for every source.
pub const YELLOW_FROM_DB: f32 = -12.0;
pub const RED_FROM_DB: f32 = -3.0;
/// Peaks at or above this are treated as clipping (0 dBFS after rounding).
pub const CLIP_PEAK: f32 = 0.999;

/// Rising levels follow within this time constant (fast attack).
const ATTACK: Duration = Duration::from_millis(12);
/// Falling levels decay at this rate (slow release), in dB per second.
const RELEASE_DB_PER_SEC: f32 = 30.0;
/// The peak marker holds this long before it starts falling.
const PEAK_HOLD: Duration = Duration::from_millis(900);

/// dBFS of a linear peak, `None` for silence (no −∞ in the UI).
pub fn dbfs(peak: f32) -> Option<f32> {
    (peak.is_finite() && peak > 0.0)
        .then(|| 20.0 * peak.log10())
        .filter(|db| *db > FLOOR_DB)
}

/// Meter position 0.0..=1.0 of a linear peak on the dB scale.
pub fn position(peak: f32) -> f32 {
    dbfs(peak).map_or(0.0, |db| ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Green,
    Yellow,
    Red,
}

/// Colour zone of segment `index` of `count` (by its centre on the dB scale).
pub fn zone(index: usize, count: usize) -> Zone {
    let centre = (index as f32 + 0.5) / count.max(1) as f32;
    let db = FLOOR_DB + centre * -FLOOR_DB;
    if db >= RED_FROM_DB {
        Zone::Red
    } else if db >= YELLOW_FROM_DB {
        Zone::Yellow
    } else {
        Zone::Green
    }
}

/// Smoothed meter state of one source. Positions are on the dB scale.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ballistics {
    /// Latest raw position reported by the engine.
    pub target: f32,
    /// Displayed bar position.
    pub level: f32,
    /// Peak-hold marker position.
    pub peak: f32,
    held_for: Duration,
    /// The engine reported a clipping peak recently (shown in red).
    pub clipped: bool,
}

impl Ballistics {
    /// A new engine reading. Clipping is never smoothed away: it marks the
    /// meter until the hold time has passed.
    pub fn set(&mut self, peak: f32) {
        self.target = position(peak);
        if peak >= CLIP_PEAK {
            self.clipped = true;
            self.held_for = Duration::ZERO;
        }
        if self.target >= self.peak {
            self.peak = self.target;
            self.held_for = Duration::ZERO;
        }
    }

    /// Advances by `dt` of wall time (independent of the frame rate).
    pub fn advance(&mut self, dt: Duration) {
        let secs = dt.as_secs_f32();
        let fall = RELEASE_DB_PER_SEC / -FLOOR_DB * secs;
        if self.target > self.level {
            let follow = 1.0 - (-secs / ATTACK.as_secs_f32()).exp();
            self.level += (self.target - self.level) * follow;
            if self.target - self.level < 0.002 {
                self.level = self.target;
            }
        } else {
            self.level = (self.level - fall).max(self.target);
        }
        self.held_for += dt;
        if self.held_for >= PEAK_HOLD {
            self.peak = (self.peak - fall).max(self.level);
            self.clipped = false;
        }
        self.peak = self.peak.max(self.level);
    }

    /// Nothing left to animate.
    pub fn settled(&self) -> bool {
        self.level == self.target && self.peak <= self.level && !self.clipped
    }
}

/// Gain of a 0..=200 % setting as shown next to the slider ("−6.0 dB",
/// "0.0 dB", "+6.0 dB"); 0 % is "Silent" rather than −∞.
pub fn gain_label(gain_percent: u16) -> String {
    if gain_percent == 0 {
        return "Silent".to_owned();
    }
    let db = 20.0 * (f32::from(gain_percent) / 100.0).log10();
    let rounded = (db * 10.0).round() / 10.0;
    if rounded > 0.0 {
        format!("+{rounded:.1} dB")
    } else if rounded < 0.0 {
        format!("\u{2212}{:.1} dB", -rounded)
    } else {
        "0.0 dB".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn dbfs_scale_and_silence() {
        assert_eq!(dbfs(1.0), Some(0.0));
        assert!((dbfs(0.5).unwrap() + 6.02).abs() < 0.01);
        for silent in [0.0, -1.0, f32::NAN, 0.0005] {
            assert_eq!(dbfs(silent), None, "{silent}");
        }
        assert_eq!(position(0.0), 0.0);
        assert_eq!(position(1.0), 1.0);
        assert_eq!(position(2.0), 1.0);
        assert!((position(0.1) - 2.0 / 3.0).abs() < 1e-4); // −20 dBFS
    }

    #[test]
    fn zones_follow_dbfs_thresholds() {
        let zones: Vec<Zone> = (0..20).map(|i| zone(i, 20)).collect();
        // 20 segments of 3 dB: centres −58.5 … −1.5 dBFS.
        assert_eq!(zones.iter().filter(|z| **z == Zone::Green).count(), 16);
        assert_eq!(zones.iter().filter(|z| **z == Zone::Yellow).count(), 3);
        assert_eq!(zones.last(), Some(&Zone::Red));
        assert!(zones.windows(2).all(|w| w[0] as u8 <= w[1] as u8));
    }

    #[test]
    fn attack_is_fast_and_release_is_slow() {
        let mut meter = Ballistics::default();
        meter.set(1.0);
        meter.advance(MS(33));
        assert!(meter.level > 0.9, "attack {}", meter.level);
        let before = meter.level;
        meter.set(0.0);
        meter.advance(MS(100));
        // 30 dB/s on a 60 dB scale: 100 ms falls 5 % of the scale.
        assert!(
            (before - meter.level - 0.05).abs() < 1e-4,
            "release {}",
            meter.level
        );
        // Frame-rate independent: two 50 ms steps equal one 100 ms step.
        let (mut a, mut b) = (meter, meter);
        a.advance(MS(100));
        b.advance(MS(50));
        b.advance(MS(50));
        assert!((a.level - b.level).abs() < 1e-5);
        for _ in 0..200 {
            meter.advance(MS(33));
        }
        assert_eq!(meter.level, 0.0);
        assert!(meter.settled());
    }

    #[test]
    fn peak_holds_and_clipping_is_never_smoothed_away() {
        let mut meter = Ballistics::default();
        meter.set(1.0);
        assert!(meter.clipped && !meter.settled());
        meter.set(0.0);
        meter.advance(MS(500));
        assert_eq!(meter.peak, 1.0);
        assert!(meter.clipped);
        meter.advance(MS(500));
        assert!(!meter.clipped && meter.peak < 1.0);
    }

    #[test]
    fn gain_labels_are_decibels_of_the_setting() {
        assert_eq!(gain_label(100), "0.0 dB");
        assert_eq!(gain_label(200), "+6.0 dB");
        assert_eq!(gain_label(50), "\u{2212}6.0 dB");
        assert_eq!(gain_label(25), "\u{2212}12.0 dB");
        assert_eq!(gain_label(0), "Silent");
    }
}
