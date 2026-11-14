//! Bounded timestamp-addressed mixing on the recorder thread, never a capture callback.

use crate::types::{SAMPLE_RATE, TICKS_PER_SECOND};

const BLOCK: usize = 480;
const CAPACITY: usize = SAMPLE_RATE as usize;

struct Track {
    samples: Vec<[f32; 2]>,
    positions: Vec<i64>,
    /// Gain applied right now; moves towards `target` by `step` per sample.
    gain: f32,
    target: f32,
    step: f32,
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
    /// One block per track, already gained and limited.
    isolated: Vec<f32>,
}

/// Output limiter: identity up to [`KNEE`], then a smooth curve that
/// approaches (never reaches) full scale, so gain above 100 % cannot wrap or
/// clip hard. Slope is continuous at the knee.
const KNEE: f32 = 0.9;

fn soft_limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= KNEE {
        return x;
    }
    let t = (a - KNEE) / (1.0 - KNEE);
    (KNEE + (1.0 - KNEE) * t / (1.0 + t)).copysign(x)
}

impl TimelineMixer {
    pub fn new(origin: i64, gains: Vec<f32>, separate: bool) -> Self {
        let count = gains.len();
        Self {
            origin,
            position: 0,
            tracks: gains
                .into_iter()
                .map(|gain| Track {
                    samples: vec![[0.0; 2]; CAPACITY],
                    positions: vec![-1; CAPACITY],
                    gain,
                    target: gain,
                    step: 0.0,
                    received: 0,
                    next_position: None,
                })
                .collect(),
            separate,
            late: 0,
            skipped_until: 0,
            mixed: vec![0.0; BLOCK * 2],
            isolated: vec![0.0; BLOCK * 2 * count],
        }
    }

    /// Ramps track `index` linearly to `gain` over one 480-frame block (no
    /// click). Out-of-range indexes and non-finite gains are ignored.
    pub fn set_gain(&mut self, index: usize, gain: f32) {
        if let Some(track) = self.tracks.get_mut(index)
            && gain.is_finite()
        {
            track.target = gain.max(0.0);
            track.step = (track.target - track.gain) / BLOCK as f32;
        }
    }

    /// Whether track `index` has delivered at least one sample.
    pub fn has_received(&self, index: usize) -> bool {
        self.tracks
            .get(index)
            .is_some_and(|track| track.received > 0)
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
            let clean = |value: f32| if value.is_finite() { value } else { 0.0 };
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
            for (index, track) in self.tracks.iter_mut().enumerate() {
                let out = &mut self.isolated[index * BLOCK * 2..][..count * 2];
                for offset in 0..count {
                    if track.gain != track.target {
                        track.gain += track.step;
                        if (track.step > 0.0 && track.gain >= track.target)
                            || (track.step < 0.0 && track.gain <= track.target)
                        {
                            track.gain = track.target;
                        }
                    }
                    let slot = (self.position + offset as i64) as usize % CAPACITY;
                    let present = track.positions[slot] == self.position + offset as i64;
                    for channel in 0..2 {
                        let value = if present {
                            track.samples[slot][channel] * track.gain
                        } else {
                            0.0
                        };
                        self.mixed[offset * 2 + channel] += value;
                        out[offset * 2 + channel] = soft_limit(value);
                    }
                }
            }
            for value in &mut self.mixed[..count * 2] {
                *value = soft_limit(*value);
            }
            let time = (i128::from(self.position) * i128::from(TICKS_PER_SECOND)
                / i128::from(SAMPLE_RATE)) as i64;
            write(0, time, &self.mixed[..count * 2])?;
            if self.separate {
                for index in 0..self.tracks.len() {
                    write(
                        index + 1,
                        time,
                        &self.isolated[index * BLOCK * 2..][..count * 2],
                    )?;
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

    fn render(mixer: &mut TimelineMixer, end: i64) -> Vec<(usize, Vec<f32>)> {
        let mut out = Vec::new();
        mixer
            .render_until(end, |track, _, samples| {
                out.push((track, samples.to_vec()));
                Ok::<_, ()>(())
            })
            .unwrap();
        out
    }

    #[test]
    fn gain_ramps_over_one_block_without_a_step() {
        let mut mixer = TimelineMixer::new(0, vec![1.0], false);
        mixer.push(0, 0, 1, &[0.5; 1440]);
        mixer.set_gain(0, 0.0);
        let blocks = render(&mut mixer, 300_000);
        let all: Vec<f32> = blocks
            .iter()
            .flat_map(|(_, b)| b.iter().step_by(2).copied())
            .collect();
        assert!(all[0] < 0.5 && all[0] > 0.49);
        assert!(all.windows(2).all(|w| w[1] <= w[0] && w[0] - w[1] < 0.002));
        assert_eq!(all[479], 0.0);
        assert_eq!(all[1000], 0.0);
    }

    #[test]
    fn zero_gain_is_silence_and_unity_is_unchanged() {
        let mut mixer = TimelineMixer::new(0, vec![0.0, 1.0], true);
        mixer.push(0, 0, 1, &[0.8; 480]);
        mixer.push(1, 0, 1, &[0.25; 480]);
        let blocks = render(&mut mixer, 100_000);
        assert!(blocks[1].1.iter().all(|v| *v == 0.0));
        assert!(blocks[2].1.iter().all(|v| *v == 0.25));
        assert!(blocks[0].1.iter().all(|v| *v == 0.25));
    }

    #[test]
    fn double_gain_on_full_scale_stays_within_range() {
        let mut mixer = TimelineMixer::new(0, vec![2.0, 2.0], true);
        mixer.push(0, 0, 1, &[1.0; 480]);
        mixer.push(1, 0, 1, &[-1.0; 480]);
        let blocks = render(&mut mixer, 100_000);
        for (_, block) in blocks {
            assert!(block.iter().all(|v| (-1.0..=1.0).contains(v)));
        }
        let mut loud = TimelineMixer::new(0, vec![2.0, 2.0], false);
        loud.push(0, 0, 1, &[1.0; 480]);
        loud.push(1, 0, 1, &[1.0; 480]);
        let blocks = render(&mut loud, 100_000);
        assert!(blocks[0].1.iter().all(|v| *v > 0.99 && *v <= 1.0));
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
