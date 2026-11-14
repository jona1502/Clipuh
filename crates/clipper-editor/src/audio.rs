//! Source audio identities and non-destructive per-track edits.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTrack {
    pub stream_index: u32,
    pub label: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub enabled: bool,
    pub muted: bool,
    pub solo: bool,
    /// Linear amplitude percent, restricted to 0..=200 by edit operations.
    pub gain_percent: u16,
    pub segments: Vec<Segment>,
    pub selected_segment: Option<usize>,
}
impl AudioTrack {
    pub fn new(
        stream_index: u32,
        label: String,
        channels: u16,
        sample_rate: u32,
        duration: Duration,
    ) -> Self {
        let segments = if duration >= MIN_SEGMENT {
            vec![Segment {
                start: Duration::ZERO,
                end: duration,
            }]
        } else {
            Vec::new()
        };
        let selected_segment = (!segments.is_empty()).then_some(0);
        Self {
            stream_index,
            label,
            channels,
            sample_rate,
            enabled: true,
            muted: false,
            solo: false,
            gain_percent: 100,
            segments,
            selected_segment,
        }
    }
    pub fn contains(&self, at: Duration) -> bool {
        self.segments.iter().any(|s| at >= s.start && at < s.end)
    }
}

/// Solo applies to enabled, unmuted tracks. Mute always wins. Final mixing
/// saturates the summed signal once, so preview and export share one policy.
pub fn effective_gain(tracks: &[AudioTrack], stream_index: u32) -> f32 {
    let solo = tracks.iter().any(|t| t.enabled && !t.muted && t.solo);
    tracks
        .iter()
        .find(|t| t.stream_index == stream_index)
        .filter(|t| t.enabled && !t.muted && (!solo || t.solo))
        .map_or(0., |t| f32::from(t.gain_percent.min(200)) / 100.)
}

impl EditSession {
    /// Initial metadata is not an edit. Seed earlier video-only snapshots too,
    /// so undoing a video edit cannot make discovered streams disappear.
    pub fn set_audio_tracks(&mut self, tracks: Vec<AudioTrack>) {
        if !self.state.audio_tracks.is_empty() {
            return;
        }
        for state in self
            .undo
            .iter_mut()
            .chain(self.redo.iter_mut())
            .chain(std::iter::once(&mut self.state))
        {
            state.audio_tracks = tracks.clone();
            state.selected_audio = (!tracks.is_empty()).then_some(0);
        }
    }
    /// Preview lookahead must remain silent across removed video ranges.
    /// Keep the editable source masks unchanged for Undo and later video edits.
    pub fn preview_audio_tracks(&self) -> Vec<AudioTrack> {
        self.state
            .audio_tracks
            .iter()
            .cloned()
            .map(|mut track| {
                track.segments = track
                    .segments
                    .iter()
                    .flat_map(|audio| {
                        self.state.segments.iter().filter_map(move |video| {
                            let start = audio.start.max(video.start);
                            let end = audio.end.min(video.end);
                            (start < end).then_some(Segment { start, end })
                        })
                    })
                    .collect();
                track.selected_segment = None;
                track
            })
            .collect()
    }
    pub fn audio_tracks(&self) -> &[AudioTrack] {
        &self.state.audio_tracks
    }
    pub fn selected_audio(&self) -> Option<usize> {
        self.state.selected_audio
    }
    pub fn select_audio(&mut self, index: Option<usize>) -> Result<(), &'static str> {
        if index.is_some_and(|i| i >= self.state.audio_tracks.len()) {
            return Err("Audio track not found.");
        }
        self.state.selected_audio = index;
        Ok(())
    }
    pub fn select_audio_segment(&mut self, index: usize) -> Result<(), &'static str> {
        let track = self
            .state
            .selected_audio
            .ok_or("Select an audio track first.")?;
        let audio = &mut self.state.audio_tracks[track];
        if index >= audio.segments.len() {
            return Err("Audio segment not found.");
        }
        audio.selected_segment = Some(index);
        Ok(())
    }
    fn update_track(
        &mut self,
        index: usize,
        edit: impl FnOnce(&mut AudioTrack) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let mut candidate = self
            .state
            .audio_tracks
            .get(index)
            .cloned()
            .ok_or("Audio track not found.")?;
        edit(&mut candidate)?;
        if self.state.audio_tracks[index] != candidate {
            self.checkpoint();
            self.state.audio_tracks[index] = candidate;
        }
        Ok(())
    }
    pub fn set_track_gain(&mut self, index: usize, gain: u16) -> Result<(), &'static str> {
        if gain > 200 {
            return Err("Gain must be between 0 and 200 percent.");
        }
        self.update_track(index, |t| {
            t.gain_percent = gain;
            Ok(())
        })
    }
    pub fn set_track_mute(&mut self, index: usize, value: bool) -> Result<(), &'static str> {
        self.update_track(index, |t| {
            t.muted = value;
            Ok(())
        })
    }
    pub fn set_track_solo(&mut self, index: usize, value: bool) -> Result<(), &'static str> {
        self.update_track(index, |t| {
            t.solo = value;
            Ok(())
        })
    }
    pub fn set_track_enabled(&mut self, index: usize, value: bool) -> Result<(), &'static str> {
        self.update_track(index, |t| {
            t.enabled = value;
            Ok(())
        })
    }
    pub fn trim_audio(&mut self, start: Duration, end: Duration) -> Result<(), &'static str> {
        let duration = self.source_duration;
        let index = self
            .state
            .selected_audio
            .ok_or("Select an audio track first.")?;
        self.update_track(index, |track| {
            let i = track
                .selected_segment
                .ok_or("Select an audio segment first.")?;
            if end > duration || end.saturating_sub(start) < MIN_SEGMENT {
                return Err("Select at least 100 ms within the source.");
            }
            if (i > 0 && start < track.segments[i - 1].end)
                || track.segments.get(i + 1).is_some_and(|s| end > s.start)
            {
                return Err("Audio segments cannot overlap.");
            }
            track.segments[i] = Segment { start, end };
            Ok(())
        })
    }
    pub fn split_audio(&mut self, at: Duration) -> Result<(), &'static str> {
        let index = self
            .state
            .selected_audio
            .ok_or("Select an audio track first.")?;
        self.update_track(index, |track| {
            let i = track
                .selected_segment
                .ok_or("Select an audio segment first.")?;
            let segment = track.segments[i];
            if at.saturating_sub(segment.start) < MIN_SEGMENT
                || segment.end.saturating_sub(at) < MIN_SEGMENT
            {
                return Err("Each audio segment must be at least 100 ms.");
            }
            track.segments[i].end = at;
            track.segments.insert(
                i + 1,
                Segment {
                    start: at,
                    end: segment.end,
                },
            );
            track.selected_segment = Some(i + 1);
            Ok(())
        })
    }
    pub fn delete_audio(&mut self) -> Result<(), &'static str> {
        let index = self
            .state
            .selected_audio
            .ok_or("Select an audio track first.")?;
        self.update_track(index, |track| {
            let i = track
                .selected_segment
                .ok_or("Select an audio segment first.")?;
            track.segments.remove(i);
            track.selected_segment = if track.segments.is_empty() {
                None
            } else {
                Some(i.min(track.segments.len() - 1))
            };
            Ok(())
        })
    }
    pub fn begin_audio_gesture(&mut self) {
        if self.audio_gesture.is_none() {
            self.audio_gesture = Some(self.state.clone());
        }
    }
    pub fn finish_audio_gesture(&mut self) {
        if let Some(before) = self.audio_gesture.take()
            && before != self.state
        {
            if self.undo.len() == HISTORY_LIMIT {
                self.undo.pop_front();
            }
            self.undo.push_back(before);
            self.redo.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> EditSession {
        let mut e = EditSession::new(Duration::from_secs(10));
        e.set_audio_tracks(vec![
            AudioTrack::new(1, "Desktop".into(), 2, 48_000, Duration::from_secs(10)),
            AudioTrack::new(3, "Microphone".into(), 1, 44_100, Duration::from_secs(10)),
        ]);
        e
    }
    #[test]
    fn mute_solo_gain_enable_share_one_policy_and_history() {
        let mut e = session();
        e.set_track_gain(0, 150).unwrap();
        assert_eq!(effective_gain(e.audio_tracks(), 1), 1.5);
        e.set_track_solo(1, true).unwrap();
        assert_eq!(effective_gain(e.audio_tracks(), 1), 0.);
        e.set_track_mute(1, true).unwrap();
        assert_eq!(effective_gain(e.audio_tracks(), 1), 1.5);
        assert_eq!(effective_gain(e.audio_tracks(), 3), 0.);
        e.set_track_enabled(0, false).unwrap();
        assert_eq!(effective_gain(e.audio_tracks(), 1), 0.);
        assert!(e.undo());
        assert_eq!(effective_gain(e.audio_tracks(), 1), 1.5);
        assert!(e.redo());
        assert!(e.set_track_gain(0, 201).is_err());
        assert!(e.set_track_mute(9, true).is_err());
    }
    #[test]
    fn independent_audio_trim_split_delete_are_undoable_source_masks() {
        let mut e = session();
        e.select_audio(Some(1)).unwrap();
        e.trim_audio(Duration::from_secs(2), Duration::from_secs(8))
            .unwrap();
        e.split_audio(Duration::from_secs(5)).unwrap();
        assert_eq!(e.audio_tracks()[1].segments.len(), 2);
        assert!(!e.audio_tracks()[1].contains(Duration::from_secs(1)));
        assert!(e.split_audio(Duration::from_secs(5)).is_err());
        e.delete_audio().unwrap();
        assert!(!e.audio_tracks()[1].contains(Duration::from_secs(6)));
        assert_eq!(e.segments().len(), 1);
        assert!(e.undo());
        assert!(e.audio_tracks()[1].contains(Duration::from_secs(6)));
        e.select_audio_segment(0).unwrap();
        assert!(
            e.trim_audio(Duration::from_secs(2), Duration::from_secs(6))
                .is_err()
        );
        assert!(
            e.trim_audio(Duration::from_secs(5), Duration::from_secs(5))
                .is_err()
        );
    }
    #[test]
    fn preview_masks_intersect_video_cuts_without_changing_source_audio() {
        let mut e = session();
        e.trim(Duration::from_secs(2), Duration::from_secs(8))
            .unwrap();
        let preview = e.preview_audio_tracks();
        assert!(!preview[0].contains(Duration::from_secs(1)));
        assert!(preview[0].contains(Duration::from_secs(3)));
        assert!(!preview[0].contains(Duration::from_secs(9)));
        assert!(e.audio_tracks()[0].contains(Duration::from_secs(1)));
        e.split(Duration::from_secs(4)).unwrap();
        e.delete_selected();
        assert!(!e.preview_audio_tracks()[0].contains(Duration::from_secs(5)));
        assert!(e.undo());
        assert!(e.preview_audio_tracks()[0].contains(Duration::from_secs(5)));
    }
    #[test]
    fn slider_gesture_is_one_undo_and_discovery_survives_video_history() {
        let mut e = session();
        e.begin_audio_gesture();
        for gain in [120, 130, 150] {
            e.set_track_gain(0, gain).unwrap();
        }
        e.finish_audio_gesture();
        assert!(e.undo());
        assert_eq!(e.audio_tracks()[0].gain_percent, 100);
        assert!(!e.can_undo());
        assert!(e.redo());
        assert_eq!(e.audio_tracks()[0].gain_percent, 150);
        let mut video = EditSession::new(Duration::from_secs(10));
        video
            .trim(Duration::from_secs(2), Duration::from_secs(8))
            .unwrap();
        video.set_audio_tracks(e.audio_tracks().to_vec());
        assert!(video.undo());
        assert_eq!(video.audio_tracks().len(), 2);
    }
    #[test]
    fn mixed_single_track_and_no_audio_remain_truthful() {
        let e = EditSession::new(Duration::from_secs(1));
        assert!(e.audio_tracks().is_empty());
        assert_eq!(effective_gain(e.audio_tracks(), 0), 0.);
        let e = session();
        assert_eq!(e.audio_tracks()[1].stream_index, 3);
        assert_eq!(e.audio_tracks()[1].channels, 1);
        assert_eq!(e.audio_tracks()[1].sample_rate, 44_100);
    }
}
