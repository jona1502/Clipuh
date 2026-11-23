//! Bounded timestamp-addressed mixing on the recorder thread, never a capture callback.
//!
//! A mixer *source* (one per user audio source) owns gain, noise gate, peak
//! meter and at most one isolated output. It is fed by one or more *inputs*
//! (capture streams): normally exactly one; a desktop remainder built from
//! several per-process streams has one input per process. Inputs of a source
//! are summed (never subtracted) before its gate and gain.
//!
//! Sources can be added and retired while the timeline runs (live audio
//! source changes during instant replay). The number of isolated outputs is
//! fixed at construction because it is the encoder's track topology; a
//! source added later either reuses a reserved output or feeds the mix only.

use crate::gate::NoiseGate;
use crate::types::{SAMPLE_RATE, TICKS_PER_SECOND};
use clipper_core::recording::NoiseGateSettings;

const BLOCK: usize = 480;
const CAPACITY: usize = SAMPLE_RATE as usize;

/// A packet whose QPC position is within this many frames (2 ms) of where the
/// previous packet of its source ended continues that source seamlessly.
/// Beyond it the packet is placed at its QPC position instead (a re-anchor):
/// the gap is rendered as silence or the overlap overwrites, so device clock
/// drift against QPC never accumulates past this bound.
pub const CONTINUITY_FRAMES: i64 = 96;

/// Re-anchors of all sources so far (see [`CONTINUITY_FRAMES`]). Device clock
/// drift shows as a steady rate of small re-anchors, a discontinuity as a
/// large one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResyncStats {
    /// Packets placed at their QPC position instead of continuing their source.
    pub count: u64,
    /// Largest |QPC position - continued position| of a re-anchor, in frames.
    pub max_frames: u64,
}

impl ResyncStats {
    fn merge(self, other: Self) -> Self {
        Self {
            count: self.count + other.count,
            max_frames: self.max_frames.max(other.max_frames),
        }
    }
}

/// One capture stream feeding a source: one second of stereo samples.
struct Input {
    key: u64,
    samples: Vec<[f32; 2]>,
    positions: Vec<i64>,
    next_position: Option<i64>,
    /// The stream ended: dropped once everything it delivered is rendered.
    retired: bool,
}

impl Input {
    fn new(key: u64) -> Self {
        Self {
            key,
            samples: vec![[0.0; 2]; CAPACITY],
            positions: vec![-1; CAPACITY],
            next_position: None,
            retired: false,
        }
    }

    /// Nothing left to render at or after `position`.
    fn drained(&self, position: i64) -> bool {
        self.next_position.is_none_or(|next| next <= position)
    }
}

struct Track {
    inputs: Vec<Input>,
    /// Isolated output (encoder stream `output + 1`), if any.
    output: Option<usize>,
    /// Gain applied right now; moves towards `target` by `step` per sample.
    gain: f32,
    target: f32,
    step: f32,
    received: u64,
    resync: ResyncStats,
    gate: NoiseGate,
    peak: f32,
    /// Removed from the session: dropped once its inputs are rendered.
    retired: bool,
}

impl Track {
    fn new(gain: f32, output: Option<usize>) -> Self {
        let gain = sanitize_gain(gain);
        Self {
            inputs: Vec::new(),
            output,
            gain,
            target: gain,
            step: 0.0,
            received: 0,
            resync: ResyncStats::default(),
            gate: NoiseGate::default(),
            peak: 0.0,
            retired: false,
        }
    }
}

fn sanitize_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, MAX_GAIN)
    } else {
        1.0
    }
}

/// One second of stereo samples per input. Old/oversized packets are dropped,
/// never allowed to allocate more memory. QPC positions continuously correct
/// sample-clock drift by placing samples on the common 48 kHz grid.
pub struct TimelineMixer {
    origin: i64,
    position: i64,
    /// Indexed by source index; `None` for removed (or never used) indexes.
    /// Indexes are never reused by the recorder, so a late packet of a
    /// removed source cannot land in a newer one.
    tracks: Vec<Option<Track>>,
    /// Isolated outputs (fixed for the session: encoder topology).
    outputs: usize,
    late: u64,
    /// Samples before this position are ignored silently (see `skip_to`).
    skipped_until: i64,
    mixed: Vec<f32>,
    /// One block per isolated output, already gained and limited.
    isolated: Vec<f32>,
    /// Re-anchors of sources that were removed, kept for diagnostics.
    removed_resync: ResyncStats,
}

/// Highest per-track gain accepted by [`TimelineMixer::set_gain`] (200 %).
/// Larger requests are clamped, so a bad caller cannot overdrive the limiter
/// into a permanently flat-topped mix.
pub const MAX_GAIN: f32 = 2.0;

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
    /// Sources `0..gains.len()`; with `separate`, source `i` owns isolated
    /// output `i` (encoder stream `i + 1`).
    pub fn new(origin: i64, gains: Vec<f32>, separate: bool) -> Self {
        let outputs = if separate { gains.len() } else { 0 };
        let mut mixer = Self::with_outputs(origin, outputs);
        for (index, gain) in gains.into_iter().enumerate() {
            mixer.add_source(index, gain, separate.then_some(index));
        }
        mixer
    }

    /// No sources yet and `outputs` isolated outputs that render silence
    /// until a source is assigned to them.
    pub fn with_outputs(origin: i64, outputs: usize) -> Self {
        Self {
            origin,
            position: 0,
            tracks: Vec::new(),
            outputs,
            late: 0,
            skipped_until: 0,
            mixed: vec![0.0; BLOCK * 2],
            isolated: vec![0.0; BLOCK * 2 * outputs],
            removed_resync: ResyncStats::default(),
        }
    }

    /// Adds source `index` (replacing a previous one with that index) with
    /// `gain` and, if `output` is a valid output index, that isolated output.
    /// It contributes from its first packet's QPC position on; nothing
    /// earlier is synthesized.
    pub fn add_source(&mut self, index: usize, gain: f32, output: Option<usize>) {
        if self.tracks.len() <= index {
            self.tracks.resize_with(index + 1, || None);
        }
        let output = output.filter(|o| *o < self.outputs);
        if let Some(old) = self.tracks[index].take() {
            self.removed_resync = self.removed_resync.merge(old.resync);
        }
        self.tracks[index] = Some(Track::new(gain, output));
    }

    /// Removes source `index` once everything already delivered for it is
    /// rendered (its tail before the removal is kept, nothing after it).
    pub fn retire_source(&mut self, index: usize) {
        if let Some(Some(track)) = self.tracks.get_mut(index) {
            track.retired = true;
        }
    }

    /// Input `key` of source `index` ended (its stream stopped). Dropped once
    /// its samples are rendered; its memory is released then.
    pub fn retire_input(&mut self, index: usize, key: u64) {
        if let Some(Some(track)) = self.tracks.get_mut(index)
            && let Some(input) = track.inputs.iter_mut().find(|i| i.key == key)
        {
            input.retired = true;
        }
    }

    /// Whether source `index` exists (not removed yet).
    pub fn has_source(&self, index: usize) -> bool {
        matches!(self.tracks.get(index), Some(Some(_)))
    }

    /// Live inputs of source `index` (diagnostics and tests).
    pub fn input_count(&self, index: usize) -> usize {
        self.track(index).map_or(0, |t| t.inputs.len())
    }

    /// Isolated outputs (encoder streams `1..=outputs`).
    pub fn outputs(&self) -> usize {
        self.outputs
    }

    fn track(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index).and_then(Option::as_ref)
    }

    fn track_mut(&mut self, index: usize) -> Option<&mut Track> {
        self.tracks.get_mut(index).and_then(Option::as_mut)
    }

    /// Ramps track `index` linearly to `gain` (clamped to `0..=`[`MAX_GAIN`])
    /// over one 480-frame block (no click). Out-of-range indexes and
    /// non-finite gains are ignored.
    pub fn set_gain(&mut self, index: usize, gain: f32) {
        if let Some(track) = self.track_mut(index)
            && gain.is_finite()
        {
            track.target = gain.clamp(0.0, MAX_GAIN);
            track.step = (track.target - track.gain) / BLOCK as f32;
        }
    }

    /// Live updates retain the current gain envelope to avoid toggle clicks.
    pub fn set_noise_gate(&mut self, index: usize, settings: NoiseGateSettings) {
        if let Some(track) = self.track_mut(index) {
            track.gate.configure(settings);
        }
    }

    pub fn reset_noise_gate(&mut self, index: usize) {
        if let Some(track) = self.track_mut(index) {
            track.gate.reset();
        }
    }

    /// Actual rendered source peak, after gate, gain and per-track limiting.
    pub fn take_peak(&mut self, index: usize) -> f32 {
        self.track_mut(index)
            .map_or(0.0, |track| std::mem::take(&mut track.peak))
    }

    /// Whether track `index` has delivered at least one sample.
    pub fn has_received(&self, index: usize) -> bool {
        self.track(index).is_some_and(|track| track.received > 0)
    }

    pub fn late_frames(&self) -> u64 {
        self.late
    }

    /// Re-anchors summed over all sources (diagnostics, no per-packet log).
    pub fn resync_stats(&self) -> ResyncStats {
        self.tracks
            .iter()
            .flatten()
            .fold(self.removed_resync, |total, track| {
                total.merge(track.resync)
            })
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
            for track in self.tracks.iter_mut().flatten() {
                track.gate.reset();
            }
        }
    }

    pub fn all_sources_received(&self) -> bool {
        self.tracks.iter().flatten().all(|track| track.received > 0)
    }

    /// Packet of the single (first) input of source `index`.
    pub fn push(&mut self, index: usize, timestamp: i64, channels: u16, samples: &[f32]) {
        self.push_input(index, 0, timestamp, channels, samples);
    }

    /// Packet of input `key` of source `index`. A new key allocates that
    /// input's ring once (recorder thread, never per sample).
    pub fn push_input(
        &mut self,
        index: usize,
        key: u64,
        timestamp: i64,
        channels: u16,
        samples: &[f32],
    ) {
        let (origin, position, skipped_until) = (self.origin, self.position, self.skipped_until);
        let Some(track) = self.tracks.get_mut(index).and_then(Option::as_mut) else {
            return;
        };
        let channels = usize::from(channels);
        if channels == 0 {
            return;
        }
        let input = match track.inputs.iter().position(|i| i.key == key) {
            Some(found) => &mut track.inputs[found],
            None => {
                track.inputs.push(Input::new(key));
                track.inputs.last_mut().expect("just pushed")
            }
        };
        let first = ((i128::from(timestamp) - i128::from(origin)) * i128::from(SAMPLE_RATE)
            / i128::from(TICKS_PER_SECOND)) as i64;
        // Packet-clock jitter below 2 ms preserves sample continuity. Larger
        // clock offsets are corrected against QPC instead of accumulating drift.
        let first = match input.next_position {
            Some(next) if (next - first).abs() <= CONTINUITY_FRAMES => next,
            Some(next) => {
                track.resync.count += 1;
                track.resync.max_frames = track.resync.max_frames.max(next.abs_diff(first));
                first
            }
            None => first,
        };
        input.next_position = Some(first.saturating_add((samples.len() / channels) as i64));
        for (offset, frame) in samples.chunks_exact(channels).enumerate() {
            let position_now = first.saturating_add(offset as i64);
            if position_now < 0 {
                continue;
            }
            if position_now < skipped_until {
                continue;
            }
            if position_now < position || position_now >= position + CAPACITY as i64 {
                self.late += 1;
                continue;
            }
            let slot = position_now as usize % CAPACITY;
            let clean = |value: f32| if value.is_finite() { value } else { 0.0 };
            input.samples[slot] = [clean(frame[0]), clean(frame[channels.min(2) - 1])];
            input.positions[slot] = position_now;
            track.received += 1;
        }
    }

    /// Drops retired inputs and sources whose delivered audio is rendered.
    fn collect_retired(&mut self) {
        let position = self.position;
        for slot in &mut self.tracks {
            let Some(track) = slot else { continue };
            track
                .inputs
                .retain(|input| !(input.retired && input.drained(position)));
            if track.retired && track.inputs.iter().all(|input| input.drained(position)) {
                self.removed_resync = self.removed_resync.merge(track.resync);
                *slot = None;
            }
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
            self.isolated.fill(0.0);
            for track in self.tracks.iter_mut().flatten() {
                for offset in 0..count {
                    if track.gain != track.target {
                        track.gain += track.step;
                        if (track.step > 0.0 && track.gain >= track.target)
                            || (track.step < 0.0 && track.gain <= track.target)
                        {
                            track.gain = track.target;
                        }
                    }
                    let position = self.position + offset as i64;
                    let slot = position as usize % CAPACITY;
                    // Inputs of one source are disjoint captures: summed.
                    let mut frame = [0.0f32; 2];
                    let mut present = false;
                    for input in &track.inputs {
                        if input.positions[slot] == position {
                            present = true;
                            frame[0] += input.samples[slot][0];
                            frame[1] += input.samples[slot][1];
                        }
                    }
                    // One detector per source, before gain/mute/solo. Both
                    // channels and the mix/isolated outputs share its envelope.
                    let gate_gain = track.gate.gain(frame[0].abs().max(frame[1].abs()));
                    for (channel, sample) in frame.iter().enumerate() {
                        let value = if present {
                            sample * gate_gain * track.gain
                        } else {
                            0.0
                        };
                        self.mixed[offset * 2 + channel] += value;
                        if let Some(output) = track.output {
                            self.isolated[output * BLOCK * 2 + offset * 2 + channel] += value;
                        }
                        track.peak = track.peak.max(soft_limit(value).abs());
                    }
                }
            }
            for value in &mut self.mixed[..count * 2] {
                *value = soft_limit(*value);
            }
            for value in &mut self.isolated {
                *value = soft_limit(*value);
            }
            let time = (i128::from(self.position) * i128::from(TICKS_PER_SECOND)
                / i128::from(SAMPLE_RATE)) as i64;
            write(0, time, &self.mixed[..count * 2])?;
            for output in 0..self.outputs {
                write(
                    output + 1,
                    time,
                    &self.isolated[output * BLOCK * 2..][..count * 2],
                )?;
            }
            self.position += count as i64;
            self.collect_retired();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_isolated_linked_and_preserves_timestamps() {
        let mut mixer = TimelineMixer::new(0, vec![1.0, 1.0], true);
        mixer.set_noise_gate(
            0,
            clipper_core::recording::NoiseGateSettings {
                enabled: true,
                ..Default::default()
            },
        );
        mixer.reset_noise_gate(0);
        let quiet = [0.001, -0.002].repeat(480);
        mixer.push(0, 0, 2, &quiet);
        mixer.push(1, 0, 2, &quiet);
        let mut blocks = Vec::new();
        mixer
            .render_until(100_000, |track, timestamp, samples| {
                blocks.push((track, timestamp, samples.to_vec()));
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(blocks.len(), 3);
        assert!(
            blocks
                .iter()
                .all(|(_, time, samples)| *time == 0 && samples.len() == 960)
        );
        assert!(blocks[1].2.iter().all(|sample| *sample == 0.0));
        assert_eq!(blocks[2].2, quiet);
        // A right-channel transient opens both channels with one shared ramp.
        let loud = [0.1, -0.4].repeat(480);
        mixer.push(0, 100_000, 2, &loud);
        let blocks = render(&mut mixer, 200_000);
        for frame in blocks[1].1.chunks_exact(2) {
            assert!((frame[0] * -4.0 - frame[1]).abs() < 1e-7);
        }
        assert_eq!(mixer.late_frames(), 0);
    }

    /// Opt-in microbenchmark: run with `cargo test -p clipper-audio --release
    /// dsp_cost -- --ignored --nocapture`. Includes existing mixing work.
    #[test]
    #[ignore = "timing benchmark; run optimized and without competing builds"]
    fn dsp_cost_four_sources_one_minute() {
        fn run(enabled: bool) -> std::time::Duration {
            let mut mixer = TimelineMixer::new(0, vec![1.0; 4], true);
            for index in 0..4 {
                mixer.set_noise_gate(
                    index,
                    NoiseGateSettings {
                        enabled,
                        ..Default::default()
                    },
                );
                mixer.reset_noise_gate(index);
            }
            let loud = [0.03f32; BLOCK * 2];
            let quiet = [0.0001f32; BLOCK * 2];
            let begin = std::time::Instant::now();
            for block in 0..6000 {
                let samples = if block % 100 < 50 { &loud } else { &quiet };
                for index in 0..4 {
                    mixer.push(index, block * 100_000, 2, std::hint::black_box(samples));
                }
                mixer
                    .render_until((block + 1) * 100_000, |_, _, output| {
                        std::hint::black_box(output);
                        Ok::<_, ()>(())
                    })
                    .unwrap();
            }
            begin.elapsed()
        }
        // Warm both instruction paths before measuring each three times.
        run(false);
        run(true);
        let bypass = (0..3).map(|_| run(false)).min().unwrap();
        let gated = (0..3).map(|_| run(true)).min().unwrap();
        println!(
            "four stereo sources, 60 s: bypass={bypass:?} gated={gated:?}; gate delta={:.2} ns/source-frame, {:.4}% of one core",
            (gated.as_secs_f64() - bypass.as_secs_f64()) * 1e9 / (4.0 * 48000.0 * 60.0),
            (gated.as_secs_f64() - bypass.as_secs_f64()) / 60.0 * 100.0
        );
    }

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
    fn gain_is_clamped_to_two_and_never_negative() {
        let mut mixer = TimelineMixer::new(0, vec![50.0, -3.0, f32::NAN], false);
        assert_eq!(mixer.tracks[0].as_ref().unwrap().gain, MAX_GAIN);
        assert_eq!(mixer.tracks[1].as_ref().unwrap().gain, 0.0);
        assert_eq!(mixer.tracks[2].as_ref().unwrap().gain, 1.0);
        mixer.set_gain(1, 10.0);
        assert_eq!(mixer.tracks[1].as_ref().unwrap().target, MAX_GAIN);
        mixer.set_gain(1, -1.0);
        assert_eq!(mixer.tracks[1].as_ref().unwrap().target, 0.0);
        mixer.set_gain(1, f32::INFINITY);
        assert_eq!(mixer.tracks[1].as_ref().unwrap().target, 0.0);
    }

    #[test]
    fn many_loud_sources_stay_inside_full_scale() {
        let mut mixer = TimelineMixer::new(0, vec![2.0; 4], true);
        for track in 0..4 {
            mixer.push(track, 0, 1, &[1.0; 480]);
        }
        for (_, block) in render(&mut mixer, 100_000) {
            assert!(block.iter().all(|v| v.abs() <= 1.0 && v.is_finite()));
        }
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
