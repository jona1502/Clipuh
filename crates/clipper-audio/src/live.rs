//! Live audio source changes of a running session (instant replay).
//!
//! The running capture keeps one track per planned stream. When the user
//! adds, removes, enables, disables or retargets a source, the new
//! [`RoutingPlan`] is compared with what runs ([`routing::diff`]) and only
//! the difference is applied: unchanged tracks keep capturing without a gap,
//! changed ones are stopped and started anew, and the mixer gains or retires
//! the matching sources. Video capture, the encoder and the replay buffer are
//! never touched.
//!
//! The encoder's audio topology (the mix plus, with separate tracks, one
//! isolated track per source planned at session start) cannot change while
//! the session runs: an MP4/replay packet stream cannot gain tracks midway.
//! Each isolated output therefore stays owned by the source it was created
//! for. A source added later feeds the mix only (its own track starts with
//! the next session) unless it owned an output at start; a removed source
//! leaves its output silent.
//!
//! Platform independent: the capture backend is a trait, so the logic is
//! tested with a fake backend and the real [`TimelineMixer`].

use crate::mixer::TimelineMixer;
use crate::routing::{self, PlannedTrack, RoutingPlan};
use crate::types::AudioError;

/// Starts and stops capture tracks. Implemented by the WASAPI engine.
pub trait TrackBackend {
    /// Starts `planned` and returns its index (unique within the capture,
    /// never reused). Must not block on the OS stream opening.
    fn add_track(&mut self, planned: &PlannedTrack) -> Result<usize, AudioError>;
    /// Stops track `index` without waiting; `false` if unknown.
    fn remove_track(&mut self, index: usize) -> bool;
}

/// A running capture track and the plan it was started for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveTrack {
    /// Capture and mixer source index.
    pub index: usize,
    pub planned: PlannedTrack,
}

/// What one [`LiveRouting::apply`] changed.
#[derive(Debug, Default)]
pub struct Change {
    pub added: Vec<LiveTrack>,
    pub removed: Vec<LiveTrack>,
    /// Sources that could not be started: `(source id, reason)`.
    pub failed: Vec<(u32, String)>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.failed.is_empty()
    }
}

/// Running tracks of a session and the session's fixed isolated outputs.
#[derive(Clone, Debug, Default)]
pub struct LiveRouting {
    tracks: Vec<LiveTrack>,
    /// Source id owning each isolated output, fixed at session start.
    outputs: Vec<u32>,
}

impl LiveRouting {
    /// The tracks a session started with. With `separate` each owns the
    /// isolated output of its position (encoder stream `position + 1`).
    pub fn new(tracks: Vec<LiveTrack>, separate: bool) -> Self {
        let outputs = if separate {
            tracks.iter().map(|t| t.planned.source).collect()
        } else {
            Vec::new()
        };
        Self { tracks, outputs }
    }

    pub fn tracks(&self) -> &[LiveTrack] {
        &self.tracks
    }

    /// Isolated output owned by `source`, if any.
    pub fn output_of(&self, source: u32) -> Option<usize> {
        self.outputs.iter().position(|s| *s == source)
    }

    /// Isolated outputs of the session (fixed).
    pub fn outputs(&self) -> usize {
        self.outputs.len()
    }

    /// Moves the running capture to `next`. `gain_of(source)` is the linear
    /// gain of a new mixer source; `admit` may refuse a track before it is
    /// started (e.g. its device is gone). Removals happen first, so a
    /// replaced stream of one source never runs twice at once (no doubled
    /// audio; the affected source alone has a short gap while the new stream
    /// opens). Stopped tracks keep their already captured tail; new tracks
    /// contribute from their first packet on, on the session's QPC timeline.
    pub fn apply(
        &mut self,
        next: &RoutingPlan,
        backend: &mut dyn TrackBackend,
        mixer: &mut TimelineMixer,
        gain_of: impl Fn(u32) -> f32,
        admit: impl Fn(&PlannedTrack) -> Result<(), String>,
    ) -> Change {
        let running: Vec<PlannedTrack> = self.tracks.iter().map(|t| t.planned.clone()).collect();
        let diff = routing::diff(&running, next);
        let mut change = Change::default();
        for &position in diff.remove.iter().rev() {
            let track = self.tracks.remove(position);
            backend.remove_track(track.index);
            mixer.retire_source(track.index);
            change.removed.push(track);
        }
        change.removed.reverse();
        for planned in diff.add {
            if let Err(reason) = admit(&planned) {
                change.failed.push((planned.source, reason));
                continue;
            }
            match backend.add_track(&planned) {
                Ok(index) => {
                    mixer.add_source(
                        index,
                        gain_of(planned.source),
                        self.output_of(planned.source),
                    );
                    let track = LiveTrack { index, planned };
                    self.tracks.push(track.clone());
                    change.added.push(track);
                }
                Err(error) => change.failed.push((planned.source, error.to_string())),
            }
        }
        // Plan order, so statuses and meters list sources predictably.
        self.tracks.sort_by_key(|t| {
            next.tracks
                .iter()
                .position(|p| *p == t.planned)
                .unwrap_or(usize::MAX)
        });
        change
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::plan;
    use crate::types::{
        AudioCapabilities, AudioSelection, MicrophoneChoice, ProcessTarget, SourceRequest,
        SourceRequestKind, TICKS_PER_SECOND,
    };

    const CAPS: AudioCapabilities = AudioCapabilities {
        process_loopback: true,
    };

    /// Fake capture engine: records starts/stops, hands out unique indexes,
    /// can refuse specific sources and refuses everything once stopped.
    #[derive(Default)]
    struct Fake {
        next: usize,
        running: Vec<(usize, PlannedTrack)>,
        started: Vec<usize>,
        stopped: Vec<usize>,
        refuse: Vec<u32>,
        shut_down: bool,
    }

    impl TrackBackend for Fake {
        fn add_track(&mut self, planned: &PlannedTrack) -> Result<usize, AudioError> {
            if self.shut_down {
                return Err(AudioError::SourceUnavailable("capture stopped".into()));
            }
            if self.refuse.contains(&planned.source) {
                return Err(AudioError::Os("activation failed".into()));
            }
            let index = self.next;
            self.next += 1;
            self.running.push((index, planned.clone()));
            self.started.push(index);
            Ok(index)
        }

        fn remove_track(&mut self, index: usize) -> bool {
            let before = self.running.len();
            self.running.retain(|(i, _)| *i != index);
            self.stopped.push(index);
            before != self.running.len()
        }
    }

    fn desktop(id: u32) -> SourceRequest {
        SourceRequest {
            id,
            kind: SourceRequestKind::Desktop { device: None },
        }
    }

    fn app(id: u32, exe: &str) -> SourceRequest {
        SourceRequest {
            id,
            kind: SourceRequestKind::Process(ProcessTarget::Executable(exe.into())),
        }
    }

    fn mic(id: u32, choice: MicrophoneChoice) -> SourceRequest {
        SourceRequest {
            id,
            kind: SourceRequestKind::Microphone(choice),
        }
    }

    fn routed(sources: Vec<SourceRequest>) -> RoutingPlan {
        plan(&AudioSelection { sources }, CAPS)
    }

    /// Starts a session like the recorder does: every planned track through
    /// the backend, one mixer source per track.
    fn session(
        initial: &RoutingPlan,
        separate: bool,
        fake: &mut Fake,
    ) -> (LiveRouting, TimelineMixer) {
        let tracks: Vec<LiveTrack> = initial
            .tracks
            .iter()
            .map(|p| LiveTrack {
                index: fake.add_track(p).unwrap(),
                planned: p.clone(),
            })
            .collect();
        let gains = vec![1.0; tracks.len()];
        let mixer = TimelineMixer::new(0, gains, separate);
        (LiveRouting::new(tracks, separate), mixer)
    }

    fn apply(
        live: &mut LiveRouting,
        next: &RoutingPlan,
        fake: &mut Fake,
        mixer: &mut TimelineMixer,
    ) -> Change {
        live.apply(next, fake, mixer, |_| 1.0, |_| Ok(()))
    }

    fn sources(live: &LiveRouting) -> Vec<u32> {
        live.tracks().iter().map(|t| t.planned.source).collect()
    }

    const MS: i64 = TICKS_PER_SECOND / 1000;

    /// Renders `[from, to)` ms and returns (stream, time, first sample).
    fn render(mixer: &mut TimelineMixer, to_ms: i64) -> Vec<(usize, i64, f32)> {
        let mut out = Vec::new();
        mixer
            .render_until(to_ms * MS, |stream, time, samples| {
                out.push((stream, time, samples[0]));
                Ok::<_, ()>(())
            })
            .unwrap();
        out
    }

    /// TEST E (Bug 2 regression): adding a source while the session runs
    /// starts exactly that capture and registers its mixer source; nothing
    /// else restarts.
    #[test]
    fn add_source_during_session_starts_only_that_track() {
        let mut fake = Fake::default();
        let (mut live, mut mixer) = session(&routed(vec![desktop(1)]), false, &mut fake);
        let change = apply(
            &mut live,
            &routed(vec![desktop(1), mic(2, MicrophoneChoice::Default)]),
            &mut fake,
            &mut mixer,
        );
        assert_eq!(change.added.len(), 1);
        assert!(change.removed.is_empty());
        assert_eq!(fake.stopped, Vec::<usize>::new());
        assert_eq!(fake.started, [0, 1]);
        assert!(mixer.has_source(0) && mixer.has_source(1));
        assert_eq!(sources(&live), [1, 2]);
    }

    /// TEST F: removing a source stops only its capture and retires its
    /// mixer source after its tail.
    #[test]
    fn remove_source_during_session_stops_only_that_track() {
        let mut fake = Fake::default();
        let (mut live, mut mixer) = session(
            &routed(vec![desktop(1), mic(2, MicrophoneChoice::Default)]),
            false,
            &mut fake,
        );
        mixer.push(1, 0, 1, &[0.5; 480]);
        let change = apply(&mut live, &routed(vec![desktop(1)]), &mut fake, &mut mixer);
        assert_eq!(change.removed.len(), 1);
        assert_eq!(fake.stopped, [1]);
        assert_eq!(sources(&live), [1]);
        // The tail captured before the removal is still rendered...
        let out = render(&mut mixer, 10);
        assert_eq!(out[0], (0, 0, 0.5));
        // ...then the source is gone, the desktop stays.
        assert!(!mixer.has_source(1));
        assert!(mixer.has_source(0));
    }

    /// TEST G: disable then re-enable creates exactly one new worker and one
    /// mixer source; no duplicates.
    #[test]
    fn disable_and_re_enable_never_duplicates() {
        let mut fake = Fake::default();
        let both = routed(vec![desktop(1), mic(2, MicrophoneChoice::Default)]);
        let (mut live, mut mixer) = session(&both, false, &mut fake);
        apply(&mut live, &routed(vec![desktop(1)]), &mut fake, &mut mixer);
        apply(&mut live, &both, &mut fake, &mut mixer);
        apply(&mut live, &both, &mut fake, &mut mixer);
        assert_eq!(fake.running.len(), 2);
        assert_eq!(fake.started, [0, 1, 2]);
        assert_eq!(sources(&live), [1, 2]);
        render(&mut mixer, 50);
        assert!(!mixer.has_source(1));
        assert!(mixer.has_source(2));
    }

    /// TEST H: a changed process (or device) selection replaces that track.
    #[test]
    fn changed_process_selection_replaces_the_track() {
        let mut fake = Fake::default();
        let (mut live, mut mixer) = session(&routed(vec![app(3, "Spotify.exe")]), false, &mut fake);
        let change = apply(
            &mut live,
            &routed(vec![app(3, "Discord.exe")]),
            &mut fake,
            &mut mixer,
        );
        assert_eq!(fake.stopped, [0]);
        assert_eq!(change.added.len(), 1);
        assert_eq!(
            fake.running[0].1.stream,
            crate::routing::StreamPlan::ProcessInclude(ProcessTarget::Executable(
                "Discord.exe".into()
            ))
        );
    }

    /// TEST I: adding a second separated app reshapes only the desktop
    /// remainder (exclusion -> composed); the first app keeps running.
    #[test]
    fn changed_exclusion_reconfigures_only_the_desktop() {
        let mut fake = Fake::default();
        let (mut live, mut mixer) = session(
            &routed(vec![desktop(1), app(2, "Spotify.exe")]),
            false,
            &mut fake,
        );
        let change = apply(
            &mut live,
            &routed(vec![
                desktop(1),
                app(2, "Spotify.exe"),
                app(3, "Discord.exe"),
            ]),
            &mut fake,
            &mut mixer,
        );
        // Old desktop (index 0) stopped; Spotify (index 1) untouched.
        assert_eq!(fake.stopped, [0]);
        assert_eq!(
            change
                .added
                .iter()
                .map(|t| t.planned.source)
                .collect::<Vec<_>>(),
            [1, 3]
        );
        assert!(matches!(
            change.added[0].planned.stream,
            crate::routing::StreamPlan::DesktopComposite(_)
        ));
        assert_eq!(sources(&live), [1, 2, 3]);
    }

    /// TEST J / V: a source that fails to start is reported with its reason;
    /// the desktop and other sources keep running and no mixer source is
    /// registered for the failed one.
    #[test]
    fn activation_failure_is_reported_and_isolated() {
        let mut fake = Fake {
            refuse: vec![2, 3],
            ..Fake::default()
        };
        let (mut live, mut mixer) = session(&routed(vec![desktop(1)]), false, &mut fake);
        let change = apply(
            &mut live,
            &routed(vec![
                desktop(1),
                app(2, "Spotify.exe"),
                app(3, "Discord.exe"),
            ]),
            &mut fake,
            &mut mixer,
        );
        assert_eq!(
            change.failed.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            [2, 3]
        );
        assert!(change.failed[0].1.contains("activation failed"));
        // The desktop was reshaped (composed) and is running.
        assert_eq!(sources(&live), [1]);
        assert!(mixer.has_source(live.tracks()[0].index));
        let refused_by_admit = live.apply(
            &routed(vec![
                desktop(1),
                mic(4, MicrophoneChoice::Device("gone".into())),
            ]),
            &mut fake,
            &mut mixer,
            |_| 1.0,
            |p| {
                if p.source == 4 {
                    Err("Selected Capture audio device is unavailable.".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(refused_by_admit.failed.len(), 1);
        assert_eq!(fake.running.len(), 1);
    }

    /// TEST K: rapid add/remove/add; the latest configuration wins and no
    /// worker from an older one survives.
    #[test]
    fn rapid_updates_converge_on_the_latest() {
        let mut fake = Fake::default();
        let base = routed(vec![desktop(1)]);
        let with_mic = routed(vec![desktop(1), mic(2, MicrophoneChoice::Default)]);
        let (mut live, mut mixer) = session(&base, false, &mut fake);
        for _ in 0..10 {
            apply(&mut live, &with_mic, &mut fake, &mut mixer);
            apply(&mut live, &base, &mut fake, &mut mixer);
        }
        apply(&mut live, &with_mic, &mut fake, &mut mixer);
        assert_eq!(fake.running.len(), 2);
        assert_eq!(sources(&live), [1, 2]);
        let indexes: Vec<usize> = live.tracks().iter().map(|t| t.index).collect();
        assert_eq!(
            indexes,
            fake.running.iter().map(|(i, _)| *i).collect::<Vec<_>>()
        );
        render(&mut mixer, 50);
        // Only the two live sources remain in the mixer.
        let alive = (0..fake.next).filter(|i| mixer.has_source(*i)).count();
        assert_eq!(alive, 2);
    }

    /// TEST L / U: once the capture is shut down nothing new starts.
    #[test]
    fn nothing_starts_after_shutdown() {
        let mut fake = Fake::default();
        let (mut live, mut mixer) = session(&routed(vec![desktop(1)]), false, &mut fake);
        fake.shut_down = true;
        let change = apply(
            &mut live,
            &routed(vec![desktop(1), mic(2, MicrophoneChoice::Default)]),
            &mut fake,
            &mut mixer,
        );
        assert!(change.added.is_empty());
        assert_eq!(change.failed.len(), 1);
        assert!(!mixer.has_source(1));
        assert_eq!(fake.running.len(), 1);
    }

    /// TEST O: many simultaneous sources keep distinct indexes and ids.
    #[test]
    fn many_sources_have_distinct_indexes() {
        let mut fake = Fake::default();
        let (mut live, mut mixer) = session(&routed(vec![desktop(1)]), false, &mut fake);
        apply(
            &mut live,
            &routed(vec![
                desktop(1),
                app(2, "a.exe"),
                app(3, "b.exe"),
                app(4, "c.exe"),
                mic(5, MicrophoneChoice::Default),
            ]),
            &mut fake,
            &mut mixer,
        );
        let mut indexes: Vec<usize> = live.tracks().iter().map(|t| t.index).collect();
        indexes.sort_unstable();
        indexes.dedup();
        assert_eq!(indexes.len(), 5);
        let mut ids = sources(&live);
        ids.sort_unstable();
        assert_eq!(ids, [1, 2, 3, 4, 5]);
    }

    /// TEST P / Q / R / T: a source added mid-session joins at its own QPC
    /// position. Earlier output is not rewritten, the mix timeline keeps
    /// advancing without a reset or a shift, and existing sources keep their
    /// exact samples.
    #[test]
    fn added_source_joins_the_timeline_without_shifting_it() {
        let mut fake = Fake::default();
        let (mut live, mut mixer) = session(&routed(vec![desktop(1)]), false, &mut fake);
        // 0..10 ms: desktop only.
        mixer.push(0, 0, 1, &[0.25; 480]);
        let before = render(&mut mixer, 10);
        assert_eq!(before, [(0, 0, 0.25)]);
        // Add a microphone; its first packet is captured at 20 ms.
        apply(
            &mut live,
            &routed(vec![desktop(1), mic(2, MicrophoneChoice::Default)]),
            &mut fake,
            &mut mixer,
        );
        let mic_index = live.tracks()[1].index;
        mixer.push(0, 10 * MS, 1, &[0.25; 960]);
        mixer.push(mic_index, 20 * MS, 1, &[0.5; 480]);
        let after = render(&mut mixer, 30);
        // Contiguous times after the change: 10 ms, 20 ms. No reset.
        assert_eq!(
            after.iter().map(|(_, t, _)| *t).collect::<Vec<_>>(),
            [10 * MS, 20 * MS]
        );
        // 10..20 ms: desktop alone (no fabricated microphone history).
        assert_eq!(after[0].2, 0.25);
        // 20..30 ms: desktop + microphone.
        assert_eq!(after[1].2, 0.75);
        assert_eq!(mixer.late_frames(), 0);
    }

    /// TEST S: gain of existing sources survives an unrelated change, and
    /// the new source gets its configured gain.
    #[test]
    fn gains_survive_live_changes() {
        let mut fake = Fake::default();
        let (mut live, mut mixer) = session(&routed(vec![desktop(1)]), false, &mut fake);
        mixer.set_gain(0, 0.5);
        render(&mut mixer, 10);
        live.apply(
            &routed(vec![desktop(1), mic(2, MicrophoneChoice::Default)]),
            &mut fake,
            &mut mixer,
            |source| if source == 2 { 0.0 } else { 1.0 },
            |_| Ok(()),
        );
        mixer.push(0, 10 * MS, 1, &[0.5; 480]);
        mixer.push(1, 10 * MS, 1, &[0.9; 480]);
        let out = render(&mut mixer, 20);
        // Desktop at 50 %, muted microphone contributes nothing.
        assert_eq!(out[0].2, 0.25);
    }

    /// Separate tracks: outputs are fixed at session start. A source that
    /// owned an output gets it back when re-enabled; a new source feeds the
    /// mix only; a removed source leaves its output silent.
    #[test]
    fn isolated_outputs_stay_owned_by_their_sources() {
        let mut fake = Fake::default();
        let start = routed(vec![desktop(1), mic(2, MicrophoneChoice::Default)]);
        let (mut live, mut mixer) = session(&start, true, &mut fake);
        assert_eq!(live.outputs(), 2);
        apply(&mut live, &routed(vec![desktop(1)]), &mut fake, &mut mixer);
        apply(
            &mut live,
            &routed(vec![desktop(1), app(3, "a.exe")]),
            &mut fake,
            &mut mixer,
        );
        assert_eq!(live.output_of(3), None);
        let app_index = live.tracks()[1].index;
        // The desktop was reshaped to exclude a.exe; find its new index.
        let desktop_index = live.tracks()[0].index;
        render(&mut mixer, 0);
        mixer.push(desktop_index, 0, 1, &[0.25; 480]);
        mixer.push(app_index, 0, 1, &[0.5; 480]);
        let out = render(&mut mixer, 10);
        // Mix = both, output 1 = desktop, output 2 (old mic) = silence.
        assert_eq!(out, [(0, 0, 0.75), (1, 0, 0.25), (2, 0, 0.0)]);
        // Re-enabling the microphone reclaims output 2.
        apply(
            &mut live,
            &routed(vec![
                desktop(1),
                app(3, "a.exe"),
                mic(2, MicrophoneChoice::Default),
            ]),
            &mut fake,
            &mut mixer,
        );
        assert_eq!(live.output_of(2), Some(1));
    }

    /// TEST C/D at the mixer level (Bug 1): with Spotify and Discord
    /// separated, the composed desktop track (several inputs) keeps
    /// rendering game audio next to the two app tracks.
    #[test]
    fn composed_desktop_keeps_rendering_next_to_two_apps() {
        let mut fake = Fake::default();
        let (live, mut mixer) = session(
            &routed(vec![
                desktop(1),
                app(2, "Spotify.exe"),
                app(3, "Discord.exe"),
            ]),
            true,
            &mut fake,
        );
        assert_eq!(sources(&live), [1, 2, 3]);
        // Desktop = game (input 10) + browser (input 11), both included.
        mixer.push_input(0, 10, 0, 1, &[0.1; 480]);
        mixer.push_input(0, 11, 0, 1, &[0.2; 480]);
        mixer.push(1, 0, 1, &[0.3; 480]);
        mixer.push(2, 0, 1, &[0.05; 480]);
        let out = render(&mut mixer, 10);
        let value = |stream: usize| out.iter().find(|(s, _, _)| *s == stream).unwrap().2;
        assert!((value(1) - 0.3).abs() < 1e-6, "desktop {}", value(1));
        assert!((value(2) - 0.3).abs() < 1e-6);
        assert!((value(3) - 0.05).abs() < 1e-6);
        assert!((value(0) - 0.65).abs() < 1e-6);
        assert_eq!(mixer.input_count(0), 2);
        // A part whose process exits is released once rendered.
        mixer.retire_input(0, 11);
        render(&mut mixer, 20);
        assert_eq!(mixer.input_count(0), 1);
    }
}
