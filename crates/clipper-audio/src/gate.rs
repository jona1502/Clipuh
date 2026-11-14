//! A linked-channel noise gate on the common 48 kHz timeline, before gain.

use crate::types::SAMPLE_RATE;
use clipper_core::recording::NoiseGateSettings;

/// Constant-size state; no buffering, allocation, synchronization or I/O.
pub struct NoiseGate {
    settings: NoiseGateSettings,
    opening: f32,
    closing: f32,
    attack_step: f64,
    release_step: f64,
    hold_frames: u32,
    remaining: u32,
    open: bool,
    gain: f64,
    envelope: f32,
    decay: f32,
}

impl Default for NoiseGate {
    fn default() -> Self {
        let mut gate = Self {
            settings: NoiseGateSettings::default(),
            opening: 0.0,
            closing: 0.0,
            attack_step: 0.0,
            release_step: 0.0,
            hold_frames: 0,
            remaining: 0,
            open: false,
            gain: 1.0,
            envelope: 0.0,
            decay: (-1.0 / (0.01 * SAMPLE_RATE as f32)).exp(),
        };
        gate.configure(NoiseGateSettings::default());
        gate
    }
}

impl NoiseGate {
    pub fn configure(&mut self, settings: NoiseGateSettings) {
        if settings.enabled != self.settings.enabled {
            // Bypass does not observe audio. Never resume a stale detector or
            // hold counter, while retaining output gain for a smooth toggle.
            self.envelope = 0.0;
            self.open = false;
            self.remaining = 0;
        }
        self.settings = settings.sanitized();
        self.opening = 10.0f32.powf(self.settings.threshold_db / 20.0);
        self.closing = self.opening * 10.0f32.powf(-3.0 / 20.0);
        let frames = |ms: f32| {
            (f64::from(ms) * f64::from(SAMPLE_RATE) / 1000.0)
                .round()
                .max(1.0)
        };
        self.attack_step = 1.0 / frames(self.settings.attack_ms);
        self.release_step = 1.0 / frames(self.settings.release_ms);
        self.hold_frames =
            (f64::from(self.settings.hold_ms) * f64::from(SAMPLE_RATE) / 1000.0).round() as u32;
        self.remaining = self.remaining.min(self.hold_frames);
    }

    /// Reset when the source restarts or its timeline skips. Newly enabled
    /// sessions begin closed; live enable/disable retains the ramp gain.
    pub fn reset(&mut self) {
        self.remaining = 0;
        self.open = false;
        self.envelope = 0.0;
        self.gain = if self.settings.enabled { 0.0 } else { 1.0 };
    }

    /// One linked peak-envelope observation per timeline frame, shared by all
    /// channels. Instant detector attack and 10 ms exponential decay bridge
    /// waveform zero crossings even with hold set to zero. Hold plus 3 dB
    /// hysteresis prevents near-threshold fluctuations from switching the gate.
    pub fn gain(&mut self, peak: f32) -> f32 {
        if !self.settings.enabled {
            if self.gain < 1.0 {
                self.gain = (self.gain + self.attack_step).min(1.0);
                if self.gain > 1.0 - 1e-12 {
                    self.gain = 1.0;
                }
            }
            return self.gain as f32;
        }
        self.envelope = peak.max(self.envelope * self.decay);
        // Below every supported threshold: avoid subnormal arithmetic during
        // long silences on processors without flush-to-zero enabled.
        if self.envelope < 1e-12 {
            self.envelope = 0.0;
        }
        let threshold = if self.open {
            self.closing
        } else {
            self.opening
        };
        if self.envelope >= threshold {
            self.open = true;
            self.remaining = self.hold_frames;
        } else if self.remaining > 0 {
            self.remaining -= 1;
        } else {
            self.open = false;
        }
        if self.open {
            self.gain = (self.gain + self.attack_step).min(1.0);
            if self.gain > 1.0 - 1e-12 {
                self.gain = 1.0;
            }
        } else {
            self.gain = (self.gain - self.release_step).max(0.0);
            if self.gain < 1e-12 {
                self.gain = 0.0;
            }
        }
        self.gain as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> NoiseGateSettings {
        NoiseGateSettings {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn disabled_is_bit_identical() {
        let mut gate = NoiseGate::default();
        for peak in [0.0, 0.001, 0.5, 1.0] {
            assert_eq!(gate.gain(peak), 1.0);
        }
    }

    #[test]
    fn attack_hold_release_and_hysteresis() {
        let mut gate = NoiseGate::default();
        gate.configure(enabled());
        let mut previous = 1.0;
        // Enabling on a running source closes smoothly, never in one step.
        for _ in 0..7200 {
            let gain = gate.gain(0.0);
            assert!((gain - previous).abs() <= 1.0 / 240.0 + 1e-6);
            previous = gain;
        }
        assert_eq!(previous, 0.0);
        for _ in 0..240 {
            previous = gate.gain(0.1);
        }
        assert_eq!(previous, 1.0);
        // 3 dB hysteresis keeps an open gate open below the opening threshold.
        let near = 10.0f32.powf(-47.0 / 20.0);
        for _ in 0..20000 {
            assert_eq!(gate.gain(near), 1.0);
        }
        for _ in 0..4800 {
            assert_eq!(gate.gain(0.0), 1.0);
        }
        for _ in 0..4800 {
            gate.gain(0.0);
        }
        assert!(gate.gain(0.0) < 1.0);
        for _ in 0..7200 {
            previous = gate.gain(0.0);
        }
        assert_eq!(previous, 0.0);
    }

    #[test]
    fn reset_and_toggle_do_not_retain_envelope() {
        let mut gate = NoiseGate::default();
        gate.configure(enabled());
        for _ in 0..20000 {
            gate.gain(0.1);
        }
        gate.reset();
        assert_eq!(gate.gain(0.0), 0.0);
        gate.configure(NoiseGateSettings::default());
        for _ in 0..240 {
            gate.gain(0.0);
        }
        assert_eq!(gate.gain(0.3), 1.0);
    }

    #[test]
    fn zero_hold_near_threshold_sine_does_not_chatter() {
        let mut gate = NoiseGate::default();
        gate.configure(NoiseGateSettings {
            enabled: true,
            threshold_db: -45.0,
            hold_ms: 0.0,
            attack_ms: 100.0,
            release_ms: 5.0,
        });
        gate.reset();
        let amplitude = 10.0f32.powf(-44.0 / 20.0);
        for frame in 0..48000 {
            let sample = amplitude * (std::f32::consts::TAU * 100.0 * frame as f32 / 48000.0).sin();
            let gain = gate.gain(sample.abs());
            if frame > 5000 {
                assert_eq!(gain, 1.0);
            }
        }
    }

    #[test]
    fn live_reenable_discards_stale_loud_detector_without_gain_step() {
        let mut gate = NoiseGate::default();
        gate.configure(enabled());
        for _ in 0..240 {
            gate.gain(0.5);
        }
        assert_eq!(gate.gain(0.5), 1.0);
        gate.configure(NoiseGateSettings::default());
        for _ in 0..240 {
            assert_eq!(gate.gain(0.0), 1.0);
        }
        gate.configure(enabled());
        let first = gate.gain(0.0);
        assert!(first < 1.0 && first > 0.999);
        for _ in 0..7200 {
            gate.gain(0.0);
        }
        assert_eq!(gate.gain(0.0), 0.0);
    }
}
