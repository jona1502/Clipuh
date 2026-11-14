//! Phase 1 boundary for the future editor subsystem. No runtime backend is implemented.
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use super::*;
    fn s(v: u64) -> Duration {
        Duration::from_secs(v)
    }
    #[test]
    fn split_validates_boundaries_and_tracks_ranges() {
        let mut edit = EditSession::new(s(10));
        assert!(edit.split(Duration::ZERO).is_err());
        assert!(edit.split(Duration::from_millis(99)).is_err());
        assert!(edit.split(s(10)).is_err());
        edit.split(s(4)).unwrap();
        assert_eq!(edit.ranges(), vec![(s(0), s(4)), (s(4), s(10))]);
        assert_eq!(edit.duration(), s(10));
        assert_eq!(edit.selected(), Some(1));
    }
    #[test]
    fn trim_enforces_nonnegative_source_bounds() {
        let mut edit = EditSession::new(s(10));
        for (start, end) in [(s(3), s(2)), (s(2), s(2)), (s(0), s(11))] {
            assert!(edit.trim(start, end).is_err());
        }
        assert!(edit.trim(s(1), Duration::from_millis(1099)).is_err());
        edit.trim(s(2), s(7)).unwrap();
        assert_eq!(edit.duration(), s(5));
        assert_eq!(edit.ranges(), vec![(s(2), s(7))]);
    }
    #[test]
    fn deletion_and_history_restore_selection_and_invalidate_redo() {
        let mut edit = EditSession::new(s(10));
        edit.split(s(4)).unwrap();
        assert!(edit.delete_selected());
        assert_eq!(edit.ranges(), vec![(s(0), s(4))]);
        assert!(edit.undo());
        assert_eq!(edit.selected(), Some(1));
        assert!(edit.redo());
        assert!(edit.undo());
        edit.trim(s(5), s(9)).unwrap();
        assert!(!edit.can_redo());
        assert!(edit.select(0));
        assert!(edit.delete_selected());
        assert!(edit.delete_selected());
        assert!(edit.segments().is_empty());
        assert_eq!(edit.selected(), None);
        assert!(edit.split(s(1)).is_err());
        assert!(edit.trim(s(0), s(1)).is_err());
        assert!(!edit.delete_selected());
        assert!(edit.undo());
    }
    #[test]
    fn conversion_clamps_invalid_geometry() {
        assert_eq!(time_to_pixel(s(5), s(10), 200.0), 100.0);
        assert_eq!(pixel_to_time(100.0, s(10), 200.0), s(5));
        assert_eq!(pixel_to_time(-20.0, s(10), 200.0), s(0));
        assert_eq!(pixel_to_time(300.0, s(10), 200.0), s(10));
        assert_eq!(pixel_to_time(f32::NAN, s(10), 200.0), s(0));
        assert_eq!(time_to_pixel(s(5), s(0), 200.0), 0.0);
        assert_eq!(pixel_to_time(2.0, s(10), 0.0), s(0));
    }
    #[test]
    fn timestamps_roundtrip_and_reject_invalid_input() {
        let duration = Duration::from_millis(3_723_045);
        assert_eq!(format_timestamp(duration), "01:02:03.045");
        assert_eq!(parse_timestamp("01:02:03.045").unwrap(), duration);
        assert_eq!(
            parse_timestamp("02:03.045").unwrap(),
            Duration::from_millis(123_045)
        );
        assert_eq!(parse_timestamp("3.5").unwrap(), Duration::from_millis(3500));
        for input in [
            "", "-1", "NaN", "inf", "1:60", "1:1:60", "1:2:3:4", "1.0001",
        ] {
            assert!(parse_timestamp(input).is_err(), "{input}");
        }
    }
    #[test]
    fn history_is_bounded_and_noop_does_not_consume_history() {
        let mut edit = EditSession::new(s(1000));
        assert!(!edit.can_undo());
        edit.trim(s(0), s(1000)).unwrap();
        assert!(!edit.can_undo());
        for i in 1..=200 {
            edit.trim(s(i), s(1000)).unwrap();
        }
        let mut count = 0;
        while edit.undo() {
            count += 1;
        }
        assert_eq!(count, HISTORY_LIMIT);
    }
}

mod audio;
pub mod timeline;
pub use audio::{
    AudioTrack, COMBINED_LABEL, LEGACY_COMBINED_LABEL, arrange_audio_tracks, display_label,
    effective_gain, is_combined,
};

use std::collections::VecDeque;
use std::time::Duration;

pub const MIN_SEGMENT: Duration = Duration::from_millis(100);
pub const HISTORY_LIMIT: usize = 100;

/// A half-open range in the unchanged source media's time domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub start: Duration,
    pub end: Duration,
}

impl Segment {
    pub fn duration(self) -> Duration {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    segments: Vec<Segment>,
    selected: Option<usize>,
    audio_tracks: Vec<AudioTrack>,
    selected_audio: Option<usize>,
}

/// Video edits reference the source without modifying it. Track-specific edits
/// can share these source ranges without sharing their own edit history.
#[derive(Debug)]
pub struct EditSession {
    source_duration: Duration,
    state: Snapshot,
    undo: VecDeque<Snapshot>,
    redo: VecDeque<Snapshot>,
    audio_gesture: Option<Snapshot>,
}

impl EditSession {
    pub fn new(duration: Duration) -> Self {
        let segments = if duration >= MIN_SEGMENT {
            vec![Segment {
                start: Duration::ZERO,
                end: duration,
            }]
        } else {
            Vec::new()
        };
        let selected = (!segments.is_empty()).then_some(0);
        Self {
            source_duration: duration,
            state: Snapshot {
                segments,
                selected,
                audio_tracks: Vec::new(),
                selected_audio: None,
            },
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            audio_gesture: None,
        }
    }

    pub fn segments(&self) -> &[Segment] {
        &self.state.segments
    }
    pub fn selected(&self) -> Option<usize> {
        self.state.selected
    }
    pub fn selected_segment(&self) -> Option<Segment> {
        self.state
            .selected
            .and_then(|index| self.state.segments.get(index).copied())
    }
    pub fn select(&mut self, index: usize) -> bool {
        if index >= self.state.segments.len() {
            return false;
        }
        self.state.selected = Some(index);
        true
    }
    pub fn source_duration(&self) -> Duration {
        self.source_duration
    }
    pub fn duration(&self) -> Duration {
        self.state
            .segments
            .iter()
            .fold(Duration::ZERO, |total, segment| {
                total.saturating_add(segment.duration())
            })
    }
    pub fn ranges(&self) -> Vec<(Duration, Duration)> {
        self.state
            .segments
            .iter()
            .map(|segment| (segment.start, segment.end))
            .collect()
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn checkpoint(&mut self) {
        if self.audio_gesture.is_some() {
            return;
        }
        if self.undo.len() == HISTORY_LIMIT {
            self.undo.pop_front();
        }
        self.undo.push_back(self.state.clone());
        self.redo.clear();
    }

    pub fn trim(&mut self, start: Duration, end: Duration) -> Result<(), &'static str> {
        let index = self.state.selected.ok_or("Select a segment first.")?;
        if end > self.source_duration || end.saturating_sub(start) < MIN_SEGMENT {
            return Err("Select a range of at least 100 ms within the source.");
        }
        // Source ranges remain ordered and disjoint, so trim handles cannot
        // silently overlap neighboring segments or duplicate media in exports.
        if (index > 0 && start < self.state.segments[index - 1].end)
            || self
                .state
                .segments
                .get(index + 1)
                .is_some_and(|next| end > next.start)
        {
            return Err("Trim boundaries cannot overlap another segment.");
        }
        let segment = Segment { start, end };
        if self.state.segments[index] != segment {
            self.checkpoint();
            self.state.segments[index] = segment;
        }
        Ok(())
    }

    pub fn split(&mut self, at: Duration) -> Result<(), &'static str> {
        let index = self.state.selected.ok_or("Select a segment first.")?;
        let segment = self.state.segments[index];
        if at.saturating_sub(segment.start) < MIN_SEGMENT
            || segment.end.saturating_sub(at) < MIN_SEGMENT
        {
            return Err("Each side of the split must be at least 100 ms.");
        }
        self.checkpoint();
        self.state.segments[index].end = at;
        self.state.segments.insert(
            index + 1,
            Segment {
                start: at,
                end: segment.end,
            },
        );
        self.state.selected = Some(index + 1);
        Ok(())
    }

    pub fn delete_selected(&mut self) -> bool {
        let Some(index) = self.state.selected else {
            return false;
        };
        self.checkpoint();
        self.state.segments.remove(index);
        self.state.selected = if self.state.segments.is_empty() {
            None
        } else {
            Some(index.min(self.state.segments.len() - 1))
        };
        true
    }
    pub fn undo(&mut self) -> bool {
        self.finish_audio_gesture();
        let Some(previous) = self.undo.pop_back() else {
            return false;
        };
        self.redo
            .push_back(std::mem::replace(&mut self.state, previous));
        true
    }
    pub fn redo(&mut self) -> bool {
        self.finish_audio_gesture();
        let Some(next) = self.redo.pop_back() else {
            return false;
        };
        self.undo
            .push_back(std::mem::replace(&mut self.state, next));
        true
    }
}

pub fn time_to_pixel(time: Duration, duration: Duration, width: f32) -> f32 {
    if duration.is_zero() || !width.is_finite() || width <= 0.0 {
        return 0.0;
    }
    (time.min(duration).as_secs_f64() / duration.as_secs_f64() * f64::from(width)) as f32
}

pub fn pixel_to_time(x: f32, duration: Duration, width: f32) -> Duration {
    if !x.is_finite() || !width.is_finite() || width <= 0.0 {
        return Duration::ZERO;
    }
    duration.mul_f64(f64::from((x / width).clamp(0.0, 1.0)))
}

/// Accepts seconds, MM:SS.mmm, or HH:MM:SS.mmm without floating-point rounding.
pub fn parse_timestamp(input: &str) -> Result<Duration, &'static str> {
    const INVALID: &str = "Enter seconds, MM:SS.mmm, or HH:MM:SS.mmm.";
    let parts: Vec<_> = input.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return Err(INVALID);
    }
    let mut seconds = 0u64;
    for (index, part) in parts.iter().enumerate() {
        let last = index == parts.len() - 1;
        let integer = if last {
            part.split('.').next().ok_or(INVALID)?
        } else {
            part
        };
        if integer.is_empty() || !integer.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(INVALID);
        }
        let value: u64 = integer.parse().map_err(|_| INVALID)?;
        if index > 0 && value >= 60 {
            return Err(INVALID);
        }
        seconds = seconds
            .checked_mul(60)
            .and_then(|total| total.checked_add(value))
            .ok_or(INVALID)?;
    }
    let last = parts.last().ok_or(INVALID)?;
    let milliseconds = if let Some((_, fraction)) = last.split_once('.') {
        if fraction.is_empty()
            || fraction.len() > 3
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(INVALID);
        }
        let value: u32 = fraction.parse().map_err(|_| INVALID)?;
        value * 10u32.pow(3 - fraction.len() as u32)
    } else {
        0
    };
    Ok(Duration::new(seconds, milliseconds * 1_000_000))
}

pub fn format_timestamp(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        duration.subsec_millis()
    )
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn adjacent_ranges_cannot_overlap_and_failed_edit_preserves_redo() {
        let mut edit = EditSession::new(Duration::from_secs(10));
        edit.split(Duration::from_secs(4)).unwrap();
        edit.trim(Duration::from_secs(5), Duration::from_secs(9))
            .unwrap();
        assert!(edit.undo());
        let ranges = edit.ranges();
        assert!(
            edit.trim(Duration::from_secs(3), Duration::from_secs(9))
                .is_err()
        );
        assert_eq!(edit.ranges(), ranges);
        assert!(edit.can_redo());
        edit.select(0);
        assert!(edit.trim(Duration::ZERO, Duration::from_secs(5)).is_err());
        assert!(!edit.select(8));
        assert_eq!(edit.selected(), Some(0));
    }

    #[test]
    fn too_short_media_stays_empty_and_exact_minimum_is_valid() {
        let empty = EditSession::new(Duration::from_millis(99));
        assert!(empty.segments().is_empty());
        assert_eq!(empty.duration(), Duration::ZERO);
        let mut edit = EditSession::new(Duration::from_millis(200));
        edit.split(MIN_SEGMENT).unwrap();
        assert_eq!(edit.segments()[0].duration(), MIN_SEGMENT);
        assert_eq!(edit.segments()[1].duration(), MIN_SEGMENT);
    }
}
