//! Bounded timestamp-addressed mixing on the recorder thread, never a capture callback.

use crate::types::{SAMPLE_RATE, TICKS_PER_SECOND};

const BLOCK: usize = 480;
const CAPACITY: usize = SAMPLE_RATE as usize;

struct Track {
    samples: Vec<[f32; 2]>,
    positions: Vec<i64>,
    gain: f32,
    received: u64,
    next_position: Option<i64>,
}

/// One second of stereo samples per source. Old/oversized packets are dropped,
/// never allowed to allocate more memory. QPC positions continuously correct
/// sample-clock drift by placing samples on the common 48 kHz grid.
pub struct TimelineMixer {
    origin: i64,
    position: i64,
    tracks: Vec<Track>,
    separate: bool,
    late: u64,
    /// Samples before this position are ignored silently (see `skip_to`).
    skipped_until: i64,
    mixed: Vec<f32>,
    isolated: Vec<f32>,
}

impl TimelineMixer {
    pub fn new(origin: i64, gains: Vec<f32>, separate: bool) -> Self {
        Self {
            origin,
            position: 0,
            tracks: gains
                .into_iter()
                .map(|gain| Track {
                    samples: vec![[0.0; 2]; CAPACITY],
                    positions: vec![-1; CAPACITY],
                    gain,
                    received: 0,
                    next_position: None,
                })
                .collect(),
            separate,
            late: 0,
            skipped_until: 0,
            mixed: vec![0.0; BLOCK * 2],
            isolated: vec![0.0; BLOCK * 2],
        }
    }

    pub fn late_frames(&self) -> u64 {
        self.late
    }

    /// Starts the timeline at `qpc` without rendering the span before it.
    /// Samples older than that point are ignored without counting as late.
    pub fn skip_to(&mut self, qpc: i64) {
        let target = ((i128::from(qpc) - i128::from(self.origin)) * i128::from(SAMPLE_RATE)
            / i128::from(TICKS_PER_SECOND))
        .clamp(0, i128::from(i64::MAX)) as i64;
        if target > self.position {
            self.position = target;
            self.skipped_until = target;
        }
    }

    pub fn all_sources_received(&self) -> bool {
        self.tracks.iter().all(|track| track.received > 0)
    }

    pub fn push(&mut self, index: usize, timestamp: i64, channels: u16, samples: &[f32]) {
        let Some(track) = self.tracks.get_mut(index) else {
            return;
        };
        let channels = usize::from(channels);
        if channels == 0 {
            return;
        }
        let first = ((i128::from(timestamp) - i128::from(self.origin)) * i128::from(SAMPLE_RATE)
            / i128::from(TICKS_PER_SECOND)) as i64;
        // Packet-clock jitter below 2 ms preserves sample continuity. Larger
        // clock offsets are corrected against QPC instead of accumulating drift.
        let first = track
            .next_position
            .filter(|next| (*next - first).abs() <= 96)
            .unwrap_or(first);
        track.next_position = Some(first.saturating_add((samples.len() / channels) as i64));
        for (offset, frame) in samples.chunks_exact(channels).enumerate() {
            let position = first.saturating_add(offset as i64);
            if position < 0 {
                continue;
            }
            if position < self.skipped_until {
                continue;
            }
            if position < self.position || position >= self.position + CAPACITY as i64 {
                self.late += 1;
                continue;
            }
            let slot = position as usize % CAPACITY;
            let clean = |value: f32| {
                if value.is_finite() {
                    value * track.gain
                } else {
                    0.0
                }
            };
            track.samples[slot] = [clean(frame[0]), clean(frame[channels.min(2) - 1])];
            track.positions[slot] = position;
            track.received += 1;
        }
    }

    /// Emit up to `qpc_end`. Caller normally keeps 80 ms of jitter headroom,
    /// then flushes to the last video timestamp when stopping. Stream zero is
    /// the playback mix; optional streams 1.. are isolated editing tracks.
    pub fn render_until<E>(
        &mut self,
        qpc_end: i64,
        mut write: impl FnMut(usize, i64, &[f32]) -> Result<(), E>,
    ) -> Result<(), E> {
        let end = ((i128::from(qpc_end) - i128::from(self.origin)) * i128::from(SAMPLE_RATE)
            / i128::from(TICKS_PER_SECOND))
        .max(0) as i64;
        // A suspend or bogus clock jump must not synthesize minutes of PCM
        // in one worker iteration. Signal loss and bound catch-up to capacity.
        let limit = self.position.saturating_add(CAPACITY as i64);
        if end > limit {
            self.late += (end - limit) as u64;
        }
        let end = end.min(limit);
        while self.position < end {
            let count = ((end - self.position) as usize).min(BLOCK);
            self.mixed[..count * 2].fill(0.0);
            for track in &self.tracks {
                for offset in 0..count {
                    let position = self.position + offset as i64;
                    let slot = position as usize % CAPACITY;
                    if track.positions[slot] == position {
                        self.mixed[offset * 2] += track.samples[slot][0];
                        self.mixed[offset * 2 + 1] += track.samples[slot][1];
                    }
                }
            }
            crate::mix::clamp(&mut self.mixed[..count * 2]);
            let time = (i128::from(self.position) * i128::from(TICKS_PER_SECOND)
                / i128::from(SAMPLE_RATE)) as i64;
            write(0, time, &self.mixed[..count * 2])?;
            if self.separate {
                for (index, track) in self.tracks.iter().enumerate() {
                    self.isolated[..count * 2].fill(0.0);
                    for offset in 0..count {
                        let position = self.position + offset as i64;
                        let slot = position as usize % CAPACITY;
                        if track.positions[slot] == position {
                            self.isolated[offset * 2..offset * 2 + 2]
                                .copy_from_slice(&track.samples[slot]);
                        }
                    }
                    crate::mix::clamp(&mut self.isolated[..count * 2]);
                    write(index + 1, time, &self.isolated[..count * 2])?;
                }
            }
            self.position += count as i64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_sources_and_retains_isolated_tracks_without_doubling_the_mix() {
        let mut mixer = TimelineMixer::new(1_000_000, vec![1.0, 0.5], true);
        mixer.push(0, 1_000_000, 2, &[0.2; 960]);
        mixer.push(1, 1_000_000, 1, &[0.4; 480]);
        let mut blocks = Vec::new();
        mixer
            .render_until(1_100_000, |track, time, samples| {
                blocks.push((track, time, samples[0]));
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(blocks, vec![(0, 0, 0.4), (1, 0, 0.2), (2, 0, 0.2)]);
    }

    #[test]
    fn timestamp_gaps_do_not_compress_time_and_late_packets_are_counted() {
        let mut mixer = TimelineMixer::new(0, vec![1.0], false);
        mixer.push(0, 100_000, 1, &[0.5; 480]);
        let mut values = Vec::new();
        mixer
            .render_until(200_000, |_, time, samples| {
                values.push((time, samples[0]));
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(values, vec![(0, 0.0), (100_000, 0.5)]);
        mixer.push(0, 0, 1, &[0.5; 480]);
        assert_eq!(mixer.late_frames(), 480);
    }

    #[test]
    fn memory_is_bounded_and_bad_future_timestamps_cannot_overwrite_current_audio() {
        let mut mixer = TimelineMixer::new(0, vec![1.0], false);
        mixer.push(0, 0, 1, &[0.25; 480]);
        mixer.push(0, 100_000_000, 1, &[0.75; 480]);
        let mut first = 0.0;
        mixer
            .render_until(100_000, |_, _, samples| {
                first = samples[0];
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(first, 0.25);
        assert_eq!(mixer.late_frames(), 480);
    }
    #[test]
    fn skip_to_starts_the_timeline_late_without_late_samples() {
        let mut mixer = TimelineMixer::new(0, vec![1.0], false);
        mixer.skip_to(10 * TICKS_PER_SECOND);
        // Older audio is ignored silently, newer audio is rendered.
        mixer.push(0, 9 * TICKS_PER_SECOND, 1, &[0.5; 480]);
        mixer.push(0, 10 * TICKS_PER_SECOND, 1, &[0.25; 480]);
        let mut first = None;
        mixer
            .render_until(10 * TICKS_PER_SECOND + 100_000, |_, time, samples| {
                first.get_or_insert((time, samples[0]));
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(first, Some((10 * TICKS_PER_SECOND, 0.25)));
        assert_eq!(mixer.late_frames(), 0);
    }

    #[test]
    fn clock_jump_cannot_emit_unbounded_silence() {
        let mut mixer = TimelineMixer::new(0, vec![1.0], false);
        let mut blocks = 0;
        mixer
            .render_until(600 * TICKS_PER_SECOND, |_, _, _| {
                blocks += 1;
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(blocks, 100);
        assert!(mixer.late_frames() > 0);
    }
}
