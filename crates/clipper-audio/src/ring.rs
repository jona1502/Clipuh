//! Bounded, lock-free, allocation-free packet ring between one capture
//! thread (producer) and the consumer.
//!
//! Two SPSC rings: interleaved f32 samples and packet headers. Both are
//! allocated once at start. When full, the producer drops the whole packet
//! and counts it — it never blocks the audio thread and memory never grows.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::RingBuffer;

use crate::types::{FrameFlags, SAMPLE_RATE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketHeader {
    /// QPC time of the first frame, 100 ns ticks.
    pub timestamp: i64,
    pub frames: u32,
    pub flags: FrameFlags,
}

/// Shared counters, readable from any thread without locks.
#[derive(Debug, Default)]
pub struct RingStats {
    pub packets: AtomicU64,
    pub frames: AtomicU64,
    pub dropped_frames: AtomicU64,
}

pub struct AudioProducer {
    samples: rtrb::Producer<f32>,
    headers: rtrb::Producer<PacketHeader>,
    channels: u16,
    stats: Arc<RingStats>,
}

pub struct AudioConsumer {
    samples: rtrb::Consumer<f32>,
    headers: rtrb::Consumer<PacketHeader>,
    channels: u16,
    stats: Arc<RingStats>,
    /// Used only when a packet wraps around the ring end.
    scratch: Vec<f32>,
}

/// Creates a ring holding `capacity_ms` of 48 kHz audio with `channels`.
pub fn audio_ring(channels: u16, capacity_ms: u32) -> (AudioProducer, AudioConsumer) {
    let channels = channels.max(1);
    let frames = (SAMPLE_RATE as usize * capacity_ms.max(10) as usize) / 1000;
    let sample_capacity = frames * usize::from(channels);
    // WASAPI event mode delivers ~10 ms packets; one header per 2.5 ms of
    // capacity leaves ample headroom for small packets.
    let header_capacity = (capacity_ms.max(10) as usize / 2).max(16);
    let (sample_tx, sample_rx) = RingBuffer::new(sample_capacity);
    let (header_tx, header_rx) = RingBuffer::new(header_capacity);
    let stats = Arc::new(RingStats::default());
    (
        AudioProducer {
            samples: sample_tx,
            headers: header_tx,
            channels,
            stats: Arc::clone(&stats),
        },
        AudioConsumer {
            samples: sample_rx,
            headers: header_rx,
            channels,
            stats,
            scratch: Vec::with_capacity(sample_capacity),
        },
    )
}

impl AudioProducer {
    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn stats(&self) -> &Arc<RingStats> {
        &self.stats
    }

    /// Pushes one packet. `samples == None` means silence of `frames`.
    /// Returns `false` (and counts the drop) when the ring is full.
    /// Real-time safe: no allocation, no lock, no syscall.
    pub fn push(&mut self, header: PacketHeader, samples: Option<&[f32]>) -> bool {
        let len = header.frames as usize * usize::from(self.channels);
        if self.headers.slots() < 1 || self.samples.slots() < len {
            self.stats
                .dropped_frames
                .fetch_add(u64::from(header.frames), Ordering::Relaxed);
            return false;
        }
        let written = match samples {
            Some(samples) if samples.len() == len => self
                .samples
                .push_entire_slice(samples)
                .map(|()| len)
                .unwrap_or(0),
            // Wrong length is a backend bug; store silence rather than garbage.
            _ => match self.samples.write_chunk_uninit(len) {
                Ok(chunk) => chunk.fill_from_iter(std::iter::repeat(0.0)),
                Err(_) => 0,
            },
        };
        if written != len || self.headers.push(header).is_err() {
            // Unreachable given the slot checks above; never corrupt framing.
            self.stats
                .dropped_frames
                .fetch_add(u64::from(header.frames), Ordering::Relaxed);
            return false;
        }
        self.stats.packets.fetch_add(1, Ordering::Relaxed);
        self.stats
            .frames
            .fetch_add(u64::from(header.frames), Ordering::Relaxed);
        true
    }
}

impl AudioConsumer {
    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn stats(&self) -> &Arc<RingStats> {
        &self.stats
    }

    /// `true` when the producer was dropped and everything was read.
    pub fn is_finished(&self) -> bool {
        self.headers.is_abandoned() && self.headers.slots() == 0
    }

    /// Pops one packet and hands its samples to `f` without allocating.
    /// Contiguous packets are passed straight from the ring (zero copy).
    pub fn pop_with<R>(&mut self, f: impl FnOnce(PacketHeader, &[f32]) -> R) -> Option<R> {
        let header = *self.headers.peek().ok()?;
        let len = header.frames as usize * usize::from(self.channels);
        let chunk = self.samples.read_chunk(len).ok()?;
        let (first, second) = chunk.as_slices();
        let result = if second.is_empty() {
            f(header, first)
        } else {
            self.scratch.clear();
            self.scratch.extend_from_slice(first);
            self.scratch.extend_from_slice(second);
            f(header, &self.scratch)
        };
        chunk.commit_all();
        let _ = self.headers.pop();
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(timestamp: i64, frames: u32) -> PacketHeader {
        PacketHeader {
            timestamp,
            frames,
            flags: FrameFlags::default(),
        }
    }

    #[test]
    fn packets_round_trip_in_order() {
        let (mut tx, mut rx) = audio_ring(2, 100);
        assert!(tx.push(header(1, 2), Some(&[0.1, 0.2, 0.3, 0.4])));
        assert!(tx.push(header(2, 1), None));
        let first = rx.pop_with(|h, s| (h.timestamp, s.to_vec())).unwrap();
        assert_eq!(first, (1, vec![0.1, 0.2, 0.3, 0.4]));
        let second = rx.pop_with(|h, s| (h.timestamp, s.to_vec())).unwrap();
        assert_eq!(second, (2, vec![0.0, 0.0]));
        assert!(rx.pop_with(|_, _| ()).is_none());
    }

    #[test]
    fn overflow_drops_whole_packets_and_counts_them() {
        // 10 ms mono = 480 frames capacity.
        let (mut tx, mut rx) = audio_ring(1, 10);
        let packet = vec![0.5; 300];
        assert!(tx.push(header(0, 300), Some(&packet)));
        assert!(!tx.push(header(1, 300), Some(&packet)));
        assert_eq!(tx.stats().dropped_frames.load(Ordering::Relaxed), 300);
        // Framing stays intact after an overflow.
        assert_eq!(rx.pop_with(|h, s| (h.timestamp, s.len())), Some((0, 300)));
        assert!(tx.push(header(2, 300), Some(&packet)));
        assert_eq!(rx.pop_with(|h, s| (h.timestamp, s.len())), Some((2, 300)));
    }

    #[test]
    fn wrapped_packets_are_reassembled() {
        let (mut tx, mut rx) = audio_ring(1, 10); // 480 samples
        let a = vec![1.0; 400];
        let b: Vec<f32> = (0..200).map(|i| i as f32).collect();
        assert!(tx.push(header(0, 400), Some(&a)));
        rx.pop_with(|_, _| ()).unwrap();
        assert!(tx.push(header(1, 200), Some(&b))); // wraps at 480
        let got = rx.pop_with(|_, s| s.to_vec()).unwrap();
        assert_eq!(got, b);
    }

    #[test]
    fn wrong_length_is_stored_as_silence() {
        let (mut tx, mut rx) = audio_ring(2, 100);
        assert!(tx.push(header(0, 2), Some(&[1.0])));
        assert_eq!(rx.pop_with(|_, s| s.to_vec()), Some(vec![0.0; 4]));
    }

    #[test]
    fn finished_after_producer_drop_and_drain() {
        let (mut tx, mut rx) = audio_ring(1, 10);
        assert!(tx.push(header(0, 1), Some(&[0.0])));
        drop(tx);
        assert!(!rx.is_finished());
        rx.pop_with(|_, _| ()).unwrap();
        assert!(rx.is_finished());
    }
}
