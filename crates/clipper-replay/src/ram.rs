//! RAM tier: GOP assembly and budget accounting.
//!
//! OWNER: RAM Buffer agent (internal to `hybrid`).
//!
//! Packets arrive in arrival order (video and audio interleaved). Everything
//! before the first video keyframe is discarded. A video keyframe seals the
//! open GOP and starts a new one; every other packet joins the open GOP.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::error::ReplayError;
use crate::gop::Gop;
use crate::packet::{EncodedPacket, Ticks};

/// Result of [`RamTier::push`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// Packet discarded (before the first keyframe).
    Discarded,
    /// Packet joined the open GOP.
    Appended,
    /// A keyframe sealed the previous GOP and opened a new one.
    Sealed,
    /// First keyframe: opened the first GOP, nothing sealed.
    Opened,
}

#[derive(Debug)]
pub struct RamTier {
    track_count: usize,
    open: Option<Gop>,
    /// Sealed GOPs still resident in RAM, oldest first, contiguous sequences.
    sealed: VecDeque<Arc<Gop>>,
    sealed_bytes: usize,
    next_sequence: u64,
    last_video_dts: Option<Ticks>,
    newest_end: Option<Ticks>,
    /// Sealed GOPs with `sequence < handed_upto` were handed to the spill
    /// thread and stay in RAM until it reports back.
    handed_upto: u64,
    handed_bytes: usize,
}

impl RamTier {
    pub fn new(track_count: usize) -> Self {
        Self {
            track_count,
            open: None,
            sealed: VecDeque::new(),
            sealed_bytes: 0,
            next_sequence: 0,
            last_video_dts: None,
            newest_end: None,
            handed_upto: 0,
            handed_bytes: 0,
        }
    }

    /// Validates and files one packet. On error nothing changed.
    pub fn push(&mut self, packet: EncodedPacket) -> Result<PushOutcome, ReplayError> {
        if usize::from(packet.track) >= self.track_count {
            return Err(ReplayError::Invalid(format!(
                "packet for unknown track {} (layout has {})",
                packet.track, self.track_count
            )));
        }
        if packet.is_video()
            && let Some(last) = self.last_video_dts
            && packet.dts <= last
        {
            return Err(ReplayError::Invalid(format!(
                "video dts {} does not increase (previous {})",
                packet.dts, last
            )));
        }
        if self.open.is_none() && !(packet.is_video() && packet.keyframe) {
            return Ok(PushOutcome::Discarded);
        }
        if packet.is_video() {
            self.last_video_dts = Some(packet.dts);
        }
        let end = packet.end();
        self.newest_end = Some(self.newest_end.map_or(end, |n| n.max(end)));

        if packet.is_video() && packet.keyframe {
            let sequence = self.next_sequence;
            self.next_sequence += 1;
            let previous = self.open.replace(Gop::new(sequence, packet));
            return Ok(match previous {
                Some(gop) => {
                    self.sealed_bytes += gop.charged_bytes;
                    self.sealed.push_back(Arc::new(gop));
                    PushOutcome::Sealed
                }
                None => PushOutcome::Opened,
            });
        }
        if let Some(open) = self.open.as_mut() {
            open.push(packet);
        }
        Ok(PushOutcome::Appended)
    }

    pub fn open(&self) -> Option<&Gop> {
        self.open.as_ref()
    }

    pub fn sealed(&self) -> &VecDeque<Arc<Gop>> {
        &self.sealed
    }

    pub fn newest_end(&self) -> Option<Ticks> {
        self.newest_end
    }

    pub fn open_charged(&self) -> usize {
        self.open.as_ref().map_or(0, |gop| gop.charged_bytes)
    }

    /// Charged bytes of sealed GOPs plus the open GOP.
    pub fn total_bytes(&self) -> usize {
        self.sealed_bytes + self.open_charged()
    }

    /// Bytes of sealed GOPs already handed to the spill thread.
    pub fn handed_bytes(&self) -> usize {
        self.handed_bytes
    }

    /// Drops the open GOP (runaway size) and waits for the next keyframe.
    pub fn discard_open(&mut self) {
        self.open = None;
    }

    /// Removes the oldest sealed GOP.
    pub fn pop_oldest(&mut self) -> Option<Arc<Gop>> {
        let gop = self.sealed.pop_front()?;
        self.sealed_bytes -= gop.charged_bytes;
        if gop.sequence < self.handed_upto {
            self.handed_bytes -= gop.charged_bytes;
        }
        Some(gop)
    }

    /// Removes the oldest sealed GOP only when it has this sequence.
    pub fn pop_oldest_if(&mut self, sequence: u64) -> Option<Arc<Gop>> {
        if self.sealed.front()?.sequence == sequence {
            self.pop_oldest()
        } else {
            None
        }
    }

    /// Oldest not yet handed GOPs worth about `excess` bytes, batched: at
    /// least `min_gops` and up to `target_bytes` unless `excess` needs more.
    pub fn peek_spill_batch(
        &self,
        excess: usize,
        min_gops: usize,
        target_bytes: usize,
    ) -> Vec<Arc<Gop>> {
        let Some(front) = self.sealed.front() else {
            return Vec::new();
        };
        let skip = self.handed_upto.saturating_sub(front.sequence) as usize;
        let mut batch = Vec::new();
        let mut bytes = 0usize;
        for gop in self.sealed.iter().skip(skip) {
            let wanted = bytes < excess || (batch.len() < min_gops && bytes < target_bytes);
            if !wanted {
                break;
            }
            bytes += gop.charged_bytes;
            batch.push(Arc::clone(gop));
        }
        batch
    }

    /// Records that `batch` (from [`Self::peek_spill_batch`]) was handed off.
    pub fn mark_handed(&mut self, batch: &[Arc<Gop>]) {
        if let Some(last) = batch.last() {
            self.handed_upto = self.handed_upto.max(last.sequence + 1);
            self.handed_bytes += batch.iter().map(|gop| gop.charged_bytes).sum::<usize>();
        }
    }

    /// Forgets every hand-off after a failed spill so the GOPs can be spilled
    /// again (or evicted) later. Late successes of newer batches are ignored
    /// by `pop_oldest_if`, so the disk tier stays strictly older than RAM.
    pub fn reset_handed(&mut self) {
        if let Some(front) = self.sealed.front() {
            self.handed_upto = front.sequence;
        }
        self.handed_bytes = 0;
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::sync::Arc;

    use crate::hybrid::ReplayBuffer;
    use crate::packet::{CodecConfig, EncodedPacket, StreamLayout, Ticks, TrackInfo};

    pub const FRAME: Ticks = 166_667;
    pub const AAC: Ticks = 213_333;
    pub const GOP_FRAMES: u64 = 120;

    pub fn layout() -> StreamLayout {
        StreamLayout {
            tracks: vec![
                TrackInfo {
                    codec: CodecConfig::H264 {
                        width: 1920,
                        height: 1080,
                        sps: vec![0x67],
                        pps: vec![0x68],
                    },
                    label: "Video".into(),
                },
                TrackInfo {
                    codec: CodecConfig::Aac {
                        sample_rate: 48_000,
                        channels: 2,
                        audio_specific_config: vec![0x11, 0x90],
                    },
                    label: "Mix".into(),
                },
            ],
        }
    }

    pub fn packet(track: u8, pts: Ticks, duration: Ticks, key: bool, size: usize) -> EncodedPacket {
        EncodedPacket {
            track,
            pts,
            dts: pts,
            duration,
            keyframe: key,
            data: Arc::from(vec![0u8; size]),
        }
    }

    /// Synthetic 60 fps video (keyframe every 2 s) plus 48 kHz AAC.
    pub struct Gen {
        pub video: u64,
        pub audio: u64,
        key: Arc<[u8]>,
        delta: Arc<[u8]>,
        aac: Arc<[u8]>,
    }

    impl Gen {
        /// ~16 Mbps: 400 kB keyframe + 119 x 30 kB deltas per GOP.
        pub fn new() -> Self {
            Self::with_sizes(400_000, 30_000, 400)
        }

        pub fn with_sizes(key: usize, delta: usize, aac: usize) -> Self {
            Self {
                video: 0,
                audio: 0,
                key: Arc::from(vec![1u8; key]),
                delta: Arc::from(vec![2u8; delta]),
                aac: Arc::from(vec![3u8; aac]),
            }
        }

        fn video_packet(&mut self) -> EncodedPacket {
            let key = self.video.is_multiple_of(GOP_FRAMES);
            let pts = self.video as Ticks * FRAME;
            self.video += 1;
            EncodedPacket {
                track: 0,
                pts,
                dts: pts,
                duration: FRAME,
                keyframe: key,
                data: Arc::clone(if key { &self.key } else { &self.delta }),
            }
        }

        fn audio_packet(&mut self) -> EncodedPacket {
            let pts = self.audio as Ticks * AAC;
            self.audio += 1;
            EncodedPacket {
                track: 1,
                pts,
                dts: pts,
                duration: AAC,
                keyframe: true,
                data: Arc::clone(&self.aac),
            }
        }

        /// Pushes packets in time order until both tracks reach `until`.
        /// Calls `each` after every push.
        pub fn run_with(
            &mut self,
            buf: &mut ReplayBuffer,
            until: Ticks,
            mut each: impl FnMut(&mut ReplayBuffer),
        ) {
            loop {
                let v = self.video as Ticks * FRAME;
                let a = self.audio as Ticks * AAC;
                if v >= until && a >= until {
                    break;
                }
                let packet = if v < until && (v <= a || a >= until) {
                    self.video_packet()
                } else {
                    self.audio_packet()
                };
                buf.push(packet).unwrap();
                each(buf);
            }
        }

        pub fn run(&mut self, buf: &mut ReplayBuffer, until: Ticks) {
            self.run_with(buf, until, |_| {});
        }

        pub fn seconds(&mut self, buf: &mut ReplayBuffer, seconds: i64) {
            let until = (self.video as Ticks * FRAME).max(self.audio as Ticks * AAC)
                + seconds * crate::packet::TICKS_PER_SECOND;
            self.run(buf, until);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn drops_packets_before_first_keyframe() {
        let mut ram = RamTier::new(2);
        assert_eq!(
            ram.push(packet(1, 0, AAC, true, 10)).unwrap(),
            PushOutcome::Discarded
        );
        assert_eq!(
            ram.push(packet(0, 0, FRAME, false, 10)).unwrap(),
            PushOutcome::Discarded
        );
        assert_eq!(ram.total_bytes(), 0);
        assert!(ram.open().is_none());
        assert_eq!(
            ram.push(packet(0, FRAME, FRAME, true, 10)).unwrap(),
            PushOutcome::Opened
        );
        assert_eq!(ram.total_bytes(), 74);
    }

    #[test]
    fn keyframe_seals_and_accounts_bytes() {
        let mut ram = RamTier::new(2);
        ram.push(packet(0, 0, FRAME, true, 100)).unwrap();
        ram.push(packet(1, 0, AAC, true, 36)).unwrap();
        ram.push(packet(0, FRAME, FRAME, false, 50)).unwrap();
        assert_eq!(
            ram.push(packet(0, 2 * FRAME, FRAME, true, 100)).unwrap(),
            PushOutcome::Sealed
        );
        assert_eq!(ram.sealed().len(), 1);
        assert_eq!(ram.sealed()[0].packets.len(), 3);
        // sealed: 164 + 100 + 114, open: 164
        assert_eq!(ram.total_bytes(), 164 + 100 + 114 + 164);
    }

    #[test]
    fn rejects_bad_input_without_side_effects() {
        let mut ram = RamTier::new(2);
        ram.push(packet(0, FRAME, FRAME, true, 10)).unwrap();
        let before = ram.total_bytes();
        assert!(matches!(
            ram.push(packet(2, FRAME, AAC, true, 10)),
            Err(ReplayError::Invalid(_))
        ));
        assert!(matches!(
            ram.push(packet(0, FRAME, FRAME, false, 10)),
            Err(ReplayError::Invalid(_))
        ));
        assert!(matches!(
            ram.push(packet(0, 0, FRAME, false, 10)),
            Err(ReplayError::Invalid(_))
        ));
        assert_eq!(ram.total_bytes(), before);
    }

    #[test]
    fn spill_batch_prefers_min_gops_and_tracks_handed() {
        let mut ram = RamTier::new(2);
        for i in 0..6 {
            ram.push(packet(0, i * FRAME, FRAME, true, 936)).unwrap();
        }
        assert_eq!(ram.sealed().len(), 5);
        let batch = ram.peek_spill_batch(1, 4, 8 << 20);
        assert_eq!(batch.len(), 4);
        ram.mark_handed(&batch);
        assert_eq!(ram.handed_bytes(), 4000);
        // Only one unhanded GOP left.
        assert_eq!(ram.peek_spill_batch(1, 4, 8 << 20).len(), 1);
        ram.pop_oldest();
        assert_eq!(ram.handed_bytes(), 3000);
        assert!(ram.pop_oldest_if(5).is_none());
        assert!(ram.pop_oldest_if(1).is_some());
    }
}
