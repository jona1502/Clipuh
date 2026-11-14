//! Public capture engine: plans routing, owns rings and one capture thread
//! per track, and hands packets to the consumer without blocking.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, sync_channel};

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use super::stream::{LevelCell, StreamSource, StreamThread};
use super::{endpoint, process};
use crate::ring::{AudioConsumer, RingStats};
use crate::routing::{self, RoutingPlan, RoutingWarning, StreamPlan};
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
    warnings: Vec<(u32, RoutingWarning)>,
    consumers: Vec<AudioConsumer>,
    levels: Vec<Arc<LevelCell>>,
    stats: Vec<Arc<RingStats>>,
    threads: Option<Vec<StreamThread>>,
    events: Receiver<AudioEvent>,
    started_at: i64,
}

/// Refuses a hand-made plan that needs process loopback on a Windows build
/// without it, instead of starting a thread that can only fail later. Plans
/// from [`routing::plan`] never trigger this (they drop such sources with a
/// warning).
fn check_supported(
    plan: &RoutingPlan,
    caps: crate::types::AudioCapabilities,
) -> Result<(), AudioError> {
    let needs_process = plan.tracks.iter().any(|t| {
        matches!(
            t.stream,
            StreamPlan::ProcessInclude(_) | StreamPlan::ProcessExclude(_)
        )
    });
    if needs_process && !caps.process_loopback {
        return Err(AudioError::Unsupported(
            RoutingWarning::ProcessLoopbackUnsupported
                .message()
                .to_owned(),
        ));
    }
    Ok(())
}

/// Current QPC time in 100 ns units.
pub fn qpc_now_ticks() -> i64 {
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
    /// Plans routing for `selection` and starts it (no consumer wake-up).
    pub fn start(selection: &AudioSelection, config: AudioConfig) -> Result<Self, AudioError> {
        let plan = routing::plan(selection, process::capabilities());
        Self::start_plan(&plan, config, None)
    }

    /// Starts exactly `plan`, waking the consumer only when packets/events
    /// arrive. The callback must never block or allocate. A process source
    /// whose app is not running does not fail the start: its thread reports
    /// `SourceLost` and keeps retrying.
    pub fn start_plan(
        plan: &RoutingPlan,
        config: AudioConfig,
        wake: Option<std::sync::mpsc::SyncSender<()>>,
    ) -> Result<Self, AudioError> {
        if plan.tracks.is_empty() {
            return Err(AudioError::NothingToCapture);
        }
        check_supported(plan, process::capabilities())?;
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
                StreamPlan::EndpointLoopback(device) => {
                    (2, endpoint::selected_loopback_source(device.as_deref()))
                }
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
            match StreamThread::spawn_with_wake(
                index,
                channels,
                source,
                producer,
                Arc::clone(&cell),
                events_tx.clone(),
                wake.clone(),
            ) {
                Ok(thread) => threads.push(thread),
                Err(error) => {
                    for thread in &threads {
                        thread.signal_stop();
                    }
                    for thread in threads {
                        thread.stop();
                    }
                    return Err(error);
                }
            }
            tracks.push(TrackInfo {
                index,
                source: planned.source,
                content: planned.content.clone(),
                channels,
                sample_rate: SAMPLE_RATE,
            });
            consumers.push(consumer);
            levels.push(cell);
        }

        Ok(Self {
            tracks,
            warnings: plan.warnings.clone(),
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

    pub fn warnings(&self) -> &[(u32, RoutingWarning)] {
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

    /// Highest peak of `track` since the previous call (lock-free); 0 when
    /// no audio arrived meanwhile. One reader per track (the meter).
    pub fn take_peak(&self, track: usize) -> f32 {
        self.levels.get(track).map_or(0.0, |cell| cell.take_peak())
    }

    pub fn events(&self) -> &Receiver<AudioEvent> {
        &self.events
    }

    fn stop_threads(&mut self) {
        if let Some(threads) = self.threads.take() {
            // Signal all first so the threads wind down in parallel.
            for thread in &threads {
                thread.signal_stop();
            }
            for thread in threads {
                thread.stop();
            }
        }
    }

    /// Stops producers, then drains their remaining packets before finalization.
    pub fn stop_and_drain(&mut self, f: impl FnMut(AudioFrameRef<'_>)) -> CaptureSummary {
        self.stop_threads();
        self.read_frames(f);
        self.summary()
    }

    /// Latched even if the bounded event channel drops the failure notification.
    pub fn had_source_failure(&self) -> bool {
        self.levels.iter().any(|levels| levels.source_failed())
    }

    /// Whether `track` ever lost its source or failed (latched, independent of
    /// the lossy event channel). `false` for an unknown track.
    pub fn track_failed(&self, track: usize) -> bool {
        self.levels
            .get(track)
            .is_some_and(|levels| levels.source_failed())
    }

    /// Persistent counters do not depend on delivery of lossy/throttled events.
    pub fn dropped_frames(&self) -> u64 {
        self.stats
            .iter()
            .map(|stats| stats.dropped_frames.load(Ordering::Relaxed))
            .sum()
    }

    fn summary(&self) -> CaptureSummary {
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

    /// Stops all threads and returns counters. Unread packets are discarded.
    pub fn stop(mut self) -> CaptureSummary {
        self.stop_threads();
        self.summary()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AudioCapabilities, ProcessTarget, TrackContent};

    fn process_plan() -> RoutingPlan {
        RoutingPlan {
            tracks: vec![routing::PlannedTrack {
                source: 1,
                content: TrackContent::Application("a.exe".into()),
                stream: StreamPlan::ProcessInclude(ProcessTarget::Executable("a.exe".into())),
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn process_plan_is_refused_without_process_loopback() {
        let legacy = AudioCapabilities {
            process_loopback: false,
        };
        let modern = AudioCapabilities {
            process_loopback: true,
        };
        assert!(matches!(
            check_supported(&process_plan(), legacy),
            Err(AudioError::Unsupported(_))
        ));
        assert!(check_supported(&process_plan(), modern).is_ok());
        let endpoint = RoutingPlan {
            tracks: vec![routing::PlannedTrack {
                source: 1,
                content: TrackContent::DesktopFull,
                stream: StreamPlan::EndpointLoopback(None),
            }],
            warnings: Vec::new(),
        };
        assert!(check_supported(&endpoint, legacy).is_ok());
    }

    #[test]
    fn unavailable_endpoint_failure_stays_latched_without_consuming_notifications() {
        let plan = RoutingPlan {
            tracks: vec![routing::PlannedTrack {
                source: 1,
                content: crate::types::TrackContent::DesktopFull,
                stream: StreamPlan::EndpointLoopback(Some(
                    "clipper-nonexistent-test-endpoint".to_owned(),
                )),
            }],
            warnings: Vec::new(),
        };
        let capture = AudioCapture::start_plan(
            &plan,
            AudioConfig {
                event_capacity: 1,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !capture.had_source_failure() {
            assert!(
                std::time::Instant::now() < deadline,
                "source failure was not latched"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        while capture.events().try_recv().is_ok() {}
        assert!(capture.had_source_failure());
        capture.stop();
    }
}
