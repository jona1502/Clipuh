//! Clip planning: keyframe selection, trimming and timestamp rebasing.
//!
//! The planner works on the packets of a snapshot (all tracks, any order) and
//! produces a [`ClipPlan`]: the exact packets to remux plus the edit
//! information the muxer needs. Nothing is re-encoded, so a clip always
//! starts on a video keyframe (IDR). With a 2 s GOP the clip therefore starts
//! up to one GOP (2 s) earlier than `end_pts - requested`; it is never
//! shorter than requested unless the buffer holds less footage. When no
//! keyframe at or before the target exists the oldest keyframe is used and
//! the clip is shorter than requested.
//!
//! Timeline rules:
//! - Presentation time 0 of the clip is the start keyframe's PTS.
//! - Video samples keep decode order. The media timeline of a track starts at
//!   its first sample (video: first DTS, audio: first PTS); a keyframe whose
//!   DTS is earlier than its PTS (B-frame reordering) is handled by
//!   composition offsets plus an edit list (`media_time` below), so no
//!   negative timestamps are ever written.
//! - Audio is trimmed to AAC frame granularity: every packet overlapping the
//!   video range `[start, video_end)` is kept. The first packet may begin
//!   before the video (`media_time` skips into it) or after it (`lead` is an
//!   empty edit placing it). Audio tracks without any sample in range are
//!   omitted from the plan.

use std::sync::Arc;

use crate::error::ReplayError;
use crate::gop::Gop;
use crate::packet::{CodecConfig, EncodedPacket, StreamLayout, Ticks};

/// One track of a planned clip.
#[derive(Clone, Debug)]
pub struct TrackPlan {
    /// Index into the stream layout (0 = video).
    pub track: u8,
    /// Samples in decode order (video) or presentation order (audio).
    pub samples: Vec<EncodedPacket>,
    /// Ticks into the track's media timeline (measured from [`Self::base`])
    /// at which presentation of the clip starts (edit list `media_time`).
    pub media_time: Ticks,
    /// Ticks of empty edit before the first sample is presented.
    pub lead: Ticks,
    /// Presentation span of the track in the clip, `lead` included.
    pub edit_duration: Ticks,
}

impl TrackPlan {
    pub fn is_video(&self) -> bool {
        self.track == StreamLayout::VIDEO_TRACK
    }

    /// Decode time of a sample: DTS for video, PTS for audio.
    pub fn decode_time(&self, sample: &EncodedPacket) -> Ticks {
        if self.is_video() {
            sample.dts
        } else {
            sample.pts
        }
    }

    /// Start of the track's media timeline.
    pub fn base(&self) -> Ticks {
        self.samples
            .first()
            .map_or(0, |sample| self.decode_time(sample))
    }
}

#[derive(Clone, Debug)]
pub struct ClipPlan {
    /// Session PTS of the start keyframe (presentation time 0 of the clip).
    pub start_pts: Ticks,
    /// Length of the video in the clip.
    pub duration: Ticks,
    pub video: TrackPlan,
    /// Audio tracks that have samples in range, in layout order.
    pub audio: Vec<TrackPlan>,
}

impl ClipPlan {
    pub fn tracks(&self) -> impl Iterator<Item = &TrackPlan> {
        std::iter::once(&self.video).chain(self.audio.iter())
    }

    pub fn payload_bytes(&self) -> u64 {
        self.tracks()
            .flat_map(|track| track.samples.iter())
            .map(|sample| sample.data.len() as u64)
            .sum()
    }

    pub fn sample_count(&self) -> u64 {
        self.tracks().map(|track| track.samples.len() as u64).sum()
    }

    /// Upper bound of the finished file size (payload + sample tables).
    pub fn estimated_file_bytes(&self) -> u64 {
        self.payload_bytes() + self.sample_count() * 24 + 8192
    }
}

fn invalid(detail: impl Into<String>) -> ReplayError {
    ReplayError::Invalid(detail.into())
}

/// Builds a clip plan, see the module documentation.
pub fn plan_clip(
    layout: &StreamLayout,
    gops: &[Arc<Gop>],
    requested: Ticks,
    end_pts: Ticks,
) -> Result<ClipPlan, ReplayError> {
    validate_layout(layout)?;
    if requested < 0 {
        return Err(invalid("negative requested duration"));
    }
    let audio_count = layout.audio_tracks();
    let mut video: Vec<EncodedPacket> = Vec::new();
    let mut audio: Vec<Vec<EncodedPacket>> = vec![Vec::new(); audio_count];
    for gop in gops {
        for packet in &gop.packets {
            if packet.duration <= 0 {
                return Err(invalid("packet with non-positive duration"));
            }
            // A B-frame encoder's derived dts lead-in may be slightly negative
            // at the session start; the muxer rebases each track to its first
            // sample, so only negative presentation times are invalid.
            if packet.pts < 0 {
                return Err(invalid("packet with negative timestamp"));
            }
            let track = usize::from(packet.track);
            if track == 0 {
                video.push(packet.clone());
            } else if let Some(list) = audio.get_mut(track - 1) {
                list.push(packet.clone());
            } else {
                return Err(invalid(format!("packet for unknown track {track}")));
            }
        }
    }

    video.sort_by_key(|packet| (packet.dts, packet.pts));
    video.dedup_by_key(|packet| packet.dts);

    let target = end_pts.saturating_sub(requested);
    let keyframes = || {
        video
            .iter()
            .filter(|packet| packet.keyframe && packet.pts < end_pts)
    };
    let start = keyframes()
        .filter(|packet| packet.pts <= target)
        .max_by_key(|packet| packet.pts)
        .or_else(|| keyframes().min_by_key(|packet| packet.pts))
        .ok_or(ReplayError::NotReady)?;
    let (start_pts, start_dts) = (start.pts, start.dts);
    let first = video
        .iter()
        .position(|packet| packet.dts == start_dts)
        .ok_or(ReplayError::NotReady)?;

    // Cut at the first frame (decode order) that reaches past the end: later
    // B-frames may reference it, so keeping them would produce garbage.
    let mut clip_video: Vec<EncodedPacket> = Vec::new();
    for packet in &video[first..] {
        if packet.pts >= end_pts {
            break;
        }
        if packet.pts < start_pts {
            continue;
        }
        if packet.pts < packet.dts {
            return Err(invalid("video packet with pts < dts"));
        }
        clip_video.push(packet.clone());
    }
    let video_end = clip_video
        .iter()
        .map(EncodedPacket::end)
        .max()
        .ok_or(ReplayError::NotReady)?;
    let duration = video_end - start_pts;
    let first_dts = clip_video[0].dts;
    let video_plan = TrackPlan {
        track: StreamLayout::VIDEO_TRACK,
        samples: clip_video,
        media_time: start_pts - first_dts,
        lead: 0,
        edit_duration: duration,
    };

    let mut audio_plans = Vec::new();
    for (index, mut list) in audio.into_iter().enumerate() {
        list.sort_by_key(|packet| packet.pts);
        list.dedup_by_key(|packet| packet.pts);
        list.retain(|packet| packet.end() > start_pts && packet.pts < video_end);
        let (Some(first), Some(last)) = (list.first(), list.last()) else {
            continue;
        };
        let lead = (first.pts - start_pts).max(0);
        let media_time = (start_pts - first.pts).max(0);
        let available = last.end() - first.pts - media_time;
        let span = available.min(duration - lead);
        if span <= 0 {
            continue;
        }
        let track = u8::try_from(index + 1).map_err(|_| invalid("too many tracks"))?;
        audio_plans.push(TrackPlan {
            track,
            samples: list,
            media_time,
            lead,
            edit_duration: lead + span,
        });
    }

    Ok(ClipPlan {
        start_pts,
        duration,
        video: video_plan,
        audio: audio_plans,
    })
}

fn validate_layout(layout: &StreamLayout) -> Result<(), ReplayError> {
    match layout.tracks.first().map(|track| &track.codec) {
        Some(CodecConfig::H264 { .. }) => {}
        _ => return Err(invalid("track 0 must be H.264 video")),
    }
    if layout.tracks.len() > 255 {
        return Err(invalid("too many tracks"));
    }
    for track in &layout.tracks[1..] {
        if !matches!(track.codec, CodecConfig::Aac { .. }) {
            return Err(invalid("tracks after 0 must be AAC audio"));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::packet::TrackInfo;

    pub const AUDIO_DUR: Ticks = 213_333;

    pub fn ms(value: i64) -> Ticks {
        value * 10_000
    }

    pub fn layout(audio_tracks: usize) -> StreamLayout {
        let mut tracks = vec![TrackInfo {
            codec: CodecConfig::H264 {
                width: 1280,
                height: 720,
                sps: vec![0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40],
                pps: vec![0x68, 0xee, 0x3c, 0x80],
            },
            label: "Video".into(),
        }];
        for index in 0..audio_tracks {
            tracks.push(TrackInfo {
                codec: CodecConfig::Aac {
                    sample_rate: 48_000,
                    channels: 2,
                    audio_specific_config: vec![0x11, 0x90],
                },
                label: format!("Audio {index}"),
            });
        }
        StreamLayout { tracks }
    }

    /// AVCC payload `[len=3, nal, pts_hi, pts_lo]`, identifiable by pts (ms).
    pub fn video_pkt(pts_ms: i64, dts_ms: i64, key: bool) -> EncodedPacket {
        let nal = if key { 0x65 } else { 0x41 };
        EncodedPacket {
            track: 0,
            pts: ms(pts_ms),
            dts: ms(dts_ms),
            duration: ms(100),
            keyframe: key,
            data: Arc::from(vec![0, 0, 0, 3, nal, (pts_ms >> 8) as u8, pts_ms as u8]),
        }
    }

    pub fn audio_pkt(track: u8, index: i64) -> EncodedPacket {
        let pts = index * AUDIO_DUR;
        EncodedPacket {
            track,
            pts,
            dts: pts,
            duration: AUDIO_DUR,
            keyframe: true,
            data: Arc::from(vec![0x21, track, (index >> 8) as u8, index as u8]),
        }
    }

    pub fn gop(sequence: u64, packets: Vec<EncodedPacket>) -> Arc<Gop> {
        let mut iter = packets.into_iter();
        let mut gop = Gop::new(sequence, iter.next().expect("keyframe"));
        for packet in iter {
            gop.push(packet);
        }
        Arc::new(gop)
    }

    /// 6 s, 10 fps, keyframes at 0/2000/4000 ms, audio tracks 1..=n covering
    /// the whole range, each audio packet in the GOP its pts falls in.
    pub fn simple_gops(audio_tracks: u8) -> Vec<Arc<Gop>> {
        let mut gops = Vec::new();
        for g in 0..3i64 {
            let mut packets = Vec::new();
            for f in 0..20 {
                let pts = g * 2000 + f * 100;
                packets.push(video_pkt(pts, pts, f == 0));
            }
            for track in 1..=audio_tracks {
                for n in 0.. {
                    let pts = n * AUDIO_DUR;
                    if pts >= ms(6000) {
                        break;
                    }
                    if pts / ms(2000) == g {
                        packets.push(audio_pkt(track, n));
                    }
                }
            }
            gops.push(gop(g as u64, packets));
        }
        gops
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn exact_request_picks_keyframe_at_target() {
        let plan = plan_clip(&layout(1), &simple_gops(1), ms(2000), ms(6000)).unwrap();
        assert_eq!(plan.start_pts, ms(4000));
        assert_eq!(plan.duration, ms(2000));
        assert_eq!(plan.video.samples.len(), 20);
        assert!(plan.video.samples[0].keyframe);
        assert_eq!(plan.video.media_time, 0);
    }

    #[test]
    fn in_between_request_picks_latest_keyframe_before_target() {
        let plan = plan_clip(&layout(1), &simple_gops(1), ms(3000), ms(6000)).unwrap();
        assert_eq!(plan.start_pts, ms(2000));
        assert_eq!(plan.duration, ms(4000));
    }

    #[test]
    fn oversized_request_uses_oldest_keyframe() {
        let plan = plan_clip(&layout(1), &simple_gops(1), ms(60_000), ms(6000)).unwrap();
        assert_eq!(plan.start_pts, 0);
        assert_eq!(plan.video.samples.len(), 60);
    }

    #[test]
    fn frames_past_end_are_dropped() {
        let plan = plan_clip(&layout(1), &simple_gops(1), ms(2000), ms(5500)).unwrap();
        // target 3500 -> keyframe 2000, frames with pts < 5500
        assert_eq!(plan.start_pts, ms(2000));
        assert_eq!(plan.video.samples.len(), 35);
        assert_eq!(plan.duration, ms(3500));
    }

    #[test]
    fn audio_is_trimmed_to_video_range_per_track() {
        let plan = plan_clip(&layout(2), &simple_gops(2), ms(2000), ms(6000)).unwrap();
        assert_eq!(plan.audio.len(), 2);
        let start = plan.start_pts;
        let end = start + plan.duration;
        for audio in &plan.audio {
            let first = audio.samples.first().unwrap();
            let last = audio.samples.last().unwrap();
            assert!(first.end() > start && first.pts <= start);
            assert!(last.pts < end);
            assert!(last.end() >= end - AUDIO_DUR);
            assert!(audio.media_time >= 0 && audio.media_time < AUDIO_DUR);
            assert_eq!(audio.lead, 0);
            assert!(audio.edit_duration <= plan.duration);
            assert!(audio.edit_duration > plan.duration - AUDIO_DUR);
        }
        assert_eq!(plan.audio[0].track, 1);
        assert_eq!(plan.audio[1].track, 2);
    }

    #[test]
    fn audio_starting_after_video_gets_a_lead() {
        let mut gops = simple_gops(1);
        let mut packets = gops[2].packets.clone();
        packets.retain(|p| p.track == 0 || p.pts >= ms(4500));
        let mut prev = gops[1].packets.clone();
        prev.retain(|p| p.track == 0 || p.pts < ms(3700));
        gops[1] = gop(1, prev);
        gops[2] = gop(2, packets);
        let plan = plan_clip(&layout(1), &gops, ms(2000), ms(6000)).unwrap();
        let audio = &plan.audio[0];
        assert!(audio.lead > 0 && audio.lead <= ms(520));
        assert_eq!(audio.media_time, 0);
        assert_eq!(audio.lead, audio.samples[0].pts - plan.start_pts);
    }

    #[test]
    fn track_without_audio_in_range_is_omitted() {
        let plan = plan_clip(&layout(1), &simple_gops(0), ms(2000), ms(6000)).unwrap();
        assert!(plan.audio.is_empty());
    }

    #[test]
    fn duplicates_and_disorder_are_normalized() {
        let mut gops = simple_gops(1);
        let mut rest = gops[2].packets.clone();
        rest.extend(gops[2].packets.clone());
        rest.reverse();
        let mut g = Gop::new(2, video_pkt(4000, 4000, true));
        for p in rest {
            g.push(p);
        }
        gops[2] = Arc::new(g);
        let plan = plan_clip(&layout(1), &gops, ms(2000), ms(6000)).unwrap();
        assert_eq!(plan.video.samples.len(), 20);
        let dts: Vec<_> = plan.video.samples.iter().map(|p| p.dts).collect();
        assert!(dts.windows(2).all(|w| w[0] < w[1]));
        let pts: Vec<_> = plan.audio[0].samples.iter().map(|p| p.pts).collect();
        assert!(pts.windows(2).all(|w| w[0] < w[1]));
    }

    pub(super) fn b_frame_gops() -> Vec<Arc<Gop>> {
        // decode order (dts, pts) per GOP: I P B B P B B
        let order = [
            (0, 200),
            (100, 500),
            (200, 300),
            (300, 400),
            (400, 800),
            (500, 600),
            (600, 700),
        ];
        (0..2)
            .map(|g| {
                let base = g * 2000;
                let packets = order
                    .iter()
                    .enumerate()
                    .map(|(i, (d, p))| video_pkt(base + p, base + d, i == 0))
                    .collect();
                gop(g as u64, packets)
            })
            .collect()
    }

    #[test]
    fn b_frames_use_media_time_and_stay_non_negative() {
        // last frame pts 2800 + 100 -> end 2900
        let plan = plan_clip(&layout(0), &b_frame_gops(), ms(500), ms(2900)).unwrap();
        assert_eq!(plan.start_pts, ms(2200));
        assert_eq!(plan.video.samples[0].dts, ms(2000));
        assert_eq!(plan.video.media_time, ms(200));
        assert_eq!(plan.duration, ms(700));
        let min_pts = plan.video.samples.iter().map(|p| p.pts).min().unwrap();
        assert_eq!(min_pts - plan.start_pts, 0);
    }

    #[test]
    fn negative_dts_lead_in_at_session_start_is_exportable() {
        // Encoder-derived B-frame dts at the very first frames: dts < 0 <= pts.
        let gops: Vec<Arc<Gop>> = b_frame_gops()
            .iter()
            .map(|g| {
                let mut g = (**g).clone();
                for packet in &mut g.packets {
                    packet.dts -= ms(200);
                }
                Arc::new(g)
            })
            .collect();
        assert!(gops[0].packets[0].dts < 0);
        let plan = plan_clip(&layout(0), &gops, ms(5000), ms(2900)).unwrap();
        assert_eq!(plan.start_pts, ms(200));
        assert!(plan.video.media_time > 0);
        let mut out = Vec::new();
        let bytes =
            crate::mp4::write_mp4(&mut out, std::path::Path::new("x.mp4"), &layout(0), &plan)
                .unwrap();
        assert_eq!(bytes as usize, out.len());
    }

    #[test]
    fn not_ready_without_keyframe() {
        let mut gop = Gop::new(0, video_pkt(0, 0, true));
        gop.packets[0] = video_pkt(0, 0, false);
        let result = plan_clip(&layout(1), &[Arc::new(gop)], ms(1000), ms(1000));
        assert!(matches!(result, Err(ReplayError::NotReady)));
        let empty = plan_clip(&layout(1), &[], ms(1000), ms(1000));
        assert!(matches!(empty, Err(ReplayError::NotReady)));
    }

    #[test]
    fn malformed_input_is_invalid_not_a_panic() {
        let mut bad = video_pkt(0, 0, true);
        bad.track = 9;
        assert!(matches!(
            plan_clip(&layout(1), &[gop(0, vec![bad])], ms(1000), ms(1000)),
            Err(ReplayError::Invalid(_))
        ));
        let mut zero = video_pkt(0, 0, true);
        zero.duration = 0;
        assert!(matches!(
            plan_clip(&layout(1), &[gop(0, vec![zero])], ms(1), ms(1)),
            Err(ReplayError::Invalid(_))
        ));
        let backwards = video_pkt(0, 100, true);
        assert!(matches!(
            plan_clip(&layout(0), &[gop(0, vec![backwards])], ms(1), ms(1000)),
            Err(ReplayError::Invalid(_))
        ));
        let empty_layout = StreamLayout { tracks: vec![] };
        assert!(plan_clip(&empty_layout, &[], 0, 0).is_err());
    }
}
