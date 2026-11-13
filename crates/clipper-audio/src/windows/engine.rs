//! Public capture engine: plans routing, owns rings and one capture thread
//! per track, and hands packets to the consumer without blocking.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, sync_channel};

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use super::stream::{LevelCell, StreamSource, StreamThread};
use super::{endpoint, process};
use crate::ring::{AudioConsumer, RingStats};
use crate::routing::{self, RoutingWarning, StreamPlan};
use crate::types::{
    AudioConfig, AudioError, AudioEvent, AudioFrameRef, AudioLevels, AudioSelection, SAMPLE_RATE,
    TICKS_PER_SECOND, TrackInfo,
};

/// Final counters per track after `stop`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureSummary {
    pub frames: Vec<u64>,
    pub dropped_frames: Vec<u64>,
}

/// Running audio capture: one dedicated thread per track.
pub struct AudioCapture {
    tracks: Vec<TrackInfo>,
    warnings: Vec<RoutingWarning>,
    consumers: Vec<AudioConsumer>,
    levels: Vec<Arc<LevelCell>>,
    stats: Vec<Arc<RingStats>>,
    threads: Option<Vec<StreamThread>>,
    events: Receiver<AudioEvent>,
    started_at: i64,
}

/// Current QPC time in 100 ns units.
fn qpc_now_ticks() -> i64 {
    let mut freq = 0i64;
    let mut counter = 0i64;
    // SAFETY: both pointers reference valid, live i64 locals.
    unsafe {
        let _ = QueryPerformanceFrequency(&mut freq);
        let _ = QueryPerformanceCounter(&mut counter);
    }
    if freq <= 0 {
        return 0;
    }
    (i128::from(counter) * i128::from(TICKS_PER_SECOND) / i128::from(freq)) as i64
}

impl AudioCapture {
    /// Plans routing for `selection`, opens rings and starts every track.
    pub fn start(selection: &AudioSelection, config: AudioConfig) -> Result<Self, AudioError> {
        let plan = routing::plan(selection, process::capabilities());
        if plan.tracks.is_empty() {
            return Err(AudioError::NothingToCapture);
        }
        // Taken before the threads spawn so no packet predates the origin.
        let started_at = qpc_now_ticks();
        let (events_tx, events_rx) = sync_channel(config.event_capacity.max(1));

        let mut tracks = Vec::new();
        let mut consumers = Vec::new();
        let mut levels = Vec::new();
        let mut stats = Vec::new();
        let mut threads: Vec<StreamThread> = Vec::new();

        for (index, planned) in plan.tracks.iter().enumerate() {
            let (channels, source): (u16, Box<dyn StreamSource>) = match &planned.stream {
                StreamPlan::EndpointLoopback => (2, endpoint::loopback_source()),
                StreamPlan::Microphone(choice) => (
                    endpoint::microphone_channels(choice),
                    endpoint::microphone_source(choice.clone()),
                ),
                StreamPlan::ProcessInclude(target) => (2, process::include_source(target.clone())),
                StreamPlan::ProcessExclude(target) => (2, process::exclude_source(target.clone())),
            };
            let (producer, consumer) = crate::ring::audio_ring(channels, config.ring_ms);
            let cell = Arc::new(LevelCell::default());
            stats.push(Arc::clone(consumer.stats()));
            match StreamThread::spawn(
                index,
                channels,
                source,
                producer,
                Arc::clone(&cell),
                events_tx.clone(),
            ) {
                Ok(thread) => threads.push(thread),
                Err(error) => {
                    for thread in threads {
                        thread.stop();
                    }
                    return Err(error);
                }
            }
            tracks.push(TrackInfo {
                index,
                content: planned.content.clone(),
                channels,
                sample_rate: SAMPLE_RATE,
            });
            consumers.push(consumer);
            levels.push(cell);
        }

        Ok(Self {
            tracks,
            warnings: plan.warnings,
            consumers,
            levels,
            stats,
            threads: Some(threads),
            events: events_rx,
            started_at,
        })
    }

    pub fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    pub fn warnings(&self) -> &[RoutingWarning] {
        &self.warnings
    }

    /// Drains all available packets of all tracks, non-blocking. Returns the
    /// number of packets delivered.
    pub fn read_frames(&mut self, mut f: impl FnMut(AudioFrameRef<'_>)) -> usize {
        let mut count = 0;
        for (track, consumer) in self.consumers.iter_mut().enumerate() {
            let channels = consumer.channels();
            while consumer
                .pop_with(|header, samples| {
                    f(AudioFrameRef {
                        track,
                        timestamp: header.timestamp,
                        channels,
                        samples,
                        flags: header.flags,
                    });
                })
                .is_some()
            {
                count += 1;
            }
        }
        count
    }

    /// Latest peak/RMS of `track` (lock-free).
    pub fn levels(&self, track: usize) -> AudioLevels {
        self.levels
            .get(track)
            .map(|cell| cell.load())
            .unwrap_or_default()
    }

    pub fn events(&self) -> &Receiver<AudioEvent> {
        &self.events
    }

    fn stop_threads(&mut self) {
        if let Some(threads) = self.threads.take() {
            for thread in threads {
                thread.stop();
            }
        }
    }

    /// Stops all threads and returns counters. Unread packets are discarded.
    pub fn stop(mut self) -> CaptureSummary {
        self.stop_threads();
        CaptureSummary {
            frames: self
                .stats
                .iter()
                .map(|s| s.frames.load(Ordering::Relaxed))
                .collect(),
            dropped_frames: self
                .stats
                .iter()
                .map(|s| s.dropped_frames.load(Ordering::Relaxed))
                .collect(),
        }
    }

    /// QPC time (100 ns) taken just before the tracks started: timeline origin.
    pub fn started_at(&self) -> i64 {
        self.started_at
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop_threads();
    }
}
