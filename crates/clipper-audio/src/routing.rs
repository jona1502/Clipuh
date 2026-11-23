//! Turns a user [`AudioSelection`] into non-overlapping capture streams.
//!
//! Windows offers three loopback shapes:
//! - endpoint loopback: the full render mix (contains every process),
//! - process loopback INCLUDE: one process tree,
//! - process loopback EXCLUDE: everything except ONE process tree.
//!
//! Tracks must be disjoint so separate tracks and the mixed output never
//! contain a signal twice. Hence:
//! - process sources: INCLUDE streams (need process loopback),
//! - desktop with no separately captured process: endpoint loopback, labelled
//!   "full mix",
//! - desktop with exactly one separately captured process: EXCLUDE that one,
//! - desktop with two or more: one exclusion cannot express it (the Windows
//!   process loopback parameters take ONE target process id), so the desktop
//!   remainder is composed instead: one INCLUDE stream per other process that
//!   owns an audio session, summed in the mixer (see [`crate::desktop`]).
//!   Subtracting signals is never attempted: loopback copies are not
//!   sample-aligned.
//! - microphones are independent devices, one track per distinct choice.

use crate::types::{
    AudioCapabilities, AudioSelection, MicrophoneChoice, ProcessTarget, SourceRequestKind,
    TrackContent,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamPlan {
    /// Loopback of a render device (`None` = default, follows default changes).
    EndpointLoopback(Option<String>),
    /// Only this process tree.
    ProcessInclude(ProcessTarget),
    /// Everything except this process tree. Process loopback EXCLUDE is
    /// system-wide, so the chosen output device is ignored for this stream.
    /// While the target is not running the remainder equals the full mix, so
    /// backends may capture endpoint loopback until the target appears.
    ProcessExclude(ProcessTarget),
    /// Everything except these process trees (two or more), composed from
    /// per-process INCLUDE streams of every other process with an audio
    /// session. System-wide like `ProcessExclude`. Audio that cannot be
    /// attributed to a separable process (see [`crate::desktop`]) is missing
    /// from this track and reported, never captured twice.
    DesktopComposite(Vec<ProcessTarget>),
    Microphone(MicrophoneChoice),
}

impl StreamPlan {
    /// Needs WASAPI process loopback.
    pub fn needs_process_loopback(&self) -> bool {
        matches!(
            self,
            Self::ProcessInclude(_) | Self::ProcessExclude(_) | Self::DesktopComposite(_)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedTrack {
    /// Mixer source id this track serves.
    pub source: u32,
    pub content: TrackContent,
    pub stream: StreamPlan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoutingWarning {
    /// Process loopback needs Windows 10 build 20348+ / Windows 11.
    ProcessLoopbackUnsupported,
    /// Same source selected more than once; recorded only once.
    Duplicate,
}

impl RoutingWarning {
    pub fn message(&self) -> &'static str {
        match self {
            Self::ProcessLoopbackUnsupported => {
                "Per-app audio needs Windows 11 (or Windows 10 build 20348+)."
            }
            Self::Duplicate => "This source is already recorded by another entry.",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingPlan {
    pub tracks: Vec<PlannedTrack>,
    /// `(source id, warning)`.
    pub warnings: Vec<(u32, RoutingWarning)>,
}

fn app_name(target: &ProcessTarget) -> String {
    match target {
        ProcessTarget::Executable(name) => name.clone(),
        ProcessTarget::Pid(pid) => format!("pid {pid}"),
    }
}

pub fn plan(selection: &AudioSelection, caps: AudioCapabilities) -> RoutingPlan {
    let mut out = RoutingPlan::default();

    // Pass 1: which process sources survive (they decide the desktop shape).
    let mut includes: Vec<&ProcessTarget> = Vec::new();
    let mut dropped: Vec<Option<RoutingWarning>> = vec![None; selection.sources.len()];
    for (i, source) in selection.sources.iter().enumerate() {
        if let SourceRequestKind::Process(target) = &source.kind {
            if !caps.process_loopback {
                dropped[i] = Some(RoutingWarning::ProcessLoopbackUnsupported);
            } else if includes.iter().any(|t| t.same_as(target)) {
                dropped[i] = Some(RoutingWarning::Duplicate);
            } else {
                includes.push(target);
            }
        }
    }

    // Pass 2: emit in selection order.
    let mut mics: Vec<&MicrophoneChoice> = Vec::new();
    let mut desktops: Vec<&Option<String>> = Vec::new();
    for (i, source) in selection.sources.iter().enumerate() {
        let id = source.id;
        if let Some(warning) = dropped[i].take() {
            out.warnings.push((id, warning));
            continue;
        }
        match &source.kind {
            SourceRequestKind::Process(target) => out.tracks.push(PlannedTrack {
                source: id,
                content: TrackContent::Application(app_name(target)),
                stream: StreamPlan::ProcessInclude(target.clone()),
            }),
            SourceRequestKind::Microphone(choice) => {
                if mics.contains(&choice) {
                    out.warnings.push((id, RoutingWarning::Duplicate));
                } else {
                    mics.push(choice);
                    out.tracks.push(PlannedTrack {
                        source: id,
                        content: TrackContent::Microphone,
                        stream: StreamPlan::Microphone(choice.clone()),
                    });
                }
            }
            SourceRequestKind::Desktop { device } => {
                // With includes present all desktops share one remainder, so
                // only the first counts; otherwise duplicates are same-device.
                if desktops.contains(&device) || (!desktops.is_empty() && !includes.is_empty()) {
                    out.warnings.push((id, RoutingWarning::Duplicate));
                    continue;
                }
                desktops.push(device);
                match includes.as_slice() {
                    [] => out.tracks.push(PlannedTrack {
                        source: id,
                        content: TrackContent::DesktopFull,
                        stream: StreamPlan::EndpointLoopback(device.clone()),
                    }),
                    [target] => out.tracks.push(PlannedTrack {
                        source: id,
                        content: TrackContent::DesktopExcluding(app_name(target)),
                        stream: StreamPlan::ProcessExclude((*target).clone()),
                    }),
                    many => out.tracks.push(PlannedTrack {
                        source: id,
                        content: TrackContent::DesktopExcludingMany(
                            many.iter().map(|t| app_name(t)).collect(),
                        ),
                        stream: StreamPlan::DesktopComposite(
                            many.iter().map(|t| (*t).clone()).collect(),
                        ),
                    }),
                }
            }
        }
    }
    out
}

/// How a running set of tracks becomes a new plan (live source changes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingDiff {
    /// Positions in `running` to stop.
    pub remove: Vec<usize>,
    /// Tracks of `next` to start, in plan order.
    pub add: Vec<PlannedTrack>,
}

/// Minimal change from `running` to `next`. A track survives only when its
/// source, content and stream are all unchanged, so it keeps capturing
/// without a gap; anything else (another device, another application, a
/// desktop whose exclusions changed) is stopped and started anew. Every
/// running track is matched at most once, so a duplicate can never survive.
pub fn diff(running: &[PlannedTrack], next: &RoutingPlan) -> RoutingDiff {
    let mut kept = vec![false; running.len()];
    let mut out = RoutingDiff::default();
    for track in &next.tracks {
        match running
            .iter()
            .enumerate()
            .find(|(i, r)| !kept[*i] && *r == track)
        {
            Some((i, _)) => kept[i] = true,
            None => out.add.push(track.clone()),
        }
    }
    out.remove = (0..running.len()).filter(|i| !kept[*i]).collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SourceRequest;

    const SUPPORTED: AudioCapabilities = AudioCapabilities {
        process_loopback: true,
    };
    const LEGACY: AudioCapabilities = AudioCapabilities {
        process_loopback: false,
    };

    fn exe(name: &str) -> ProcessTarget {
        ProcessTarget::Executable(name.into())
    }

    fn app(id: u32, name: &str) -> SourceRequest {
        SourceRequest {
            id,
            kind: SourceRequestKind::Process(exe(name)),
        }
    }

    fn desktop(id: u32, device: Option<&str>) -> SourceRequest {
        SourceRequest {
            id,
            kind: SourceRequestKind::Desktop {
                device: device.map(str::to_owned),
            },
        }
    }

    fn mic(id: u32, choice: MicrophoneChoice) -> SourceRequest {
        SourceRequest {
            id,
            kind: SourceRequestKind::Microphone(choice),
        }
    }

    fn sel(sources: Vec<SourceRequest>) -> AudioSelection {
        AudioSelection { sources }
    }

    fn contents(plan: &RoutingPlan) -> Vec<TrackContent> {
        plan.tracks.iter().map(|t| t.content.clone()).collect()
    }

    /// No two tracks may capture the same process: an INCLUDE target must not
    /// also be audible in a desktop track, and at most one desktop remainder
    /// exists when processes are included.
    fn assert_disjoint(plan: &RoutingPlan) {
        let includes: Vec<&ProcessTarget> = plan
            .tracks
            .iter()
            .filter_map(|t| match &t.stream {
                StreamPlan::ProcessInclude(target) => Some(target),
                _ => None,
            })
            .collect();
        let mut remainders = 0;
        for track in &plan.tracks {
            match &track.stream {
                StreamPlan::EndpointLoopback(_) => {
                    remainders += 1;
                    assert!(includes.is_empty(), "full mix overlaps {includes:?}")
                }
                StreamPlan::ProcessExclude(excluded) => {
                    remainders += 1;
                    assert!(
                        includes.iter().all(|i| *i == excluded),
                        "remainder excluding {excluded:?} overlaps {includes:?}"
                    )
                }
                StreamPlan::DesktopComposite(excluded) => {
                    remainders += 1;
                    assert!(excluded.len() >= 2, "composite for {excluded:?}");
                    assert!(
                        includes.iter().all(|i| excluded.contains(i)),
                        "composite excluding {excluded:?} overlaps {includes:?}"
                    )
                }
                _ => {}
            }
        }
        assert!(includes.is_empty() || remainders <= 1);
    }

    #[test]
    fn n_apps_keep_order_and_ids() {
        let plan = plan(
            &sel(vec![app(7, "a.exe"), app(3, "b.exe"), app(9, "c.exe")]),
            SUPPORTED,
        );
        assert_eq!(
            contents(&plan),
            [
                TrackContent::Application("a.exe".into()),
                TrackContent::Application("b.exe".into()),
                TrackContent::Application("c.exe".into())
            ]
        );
        assert_eq!(
            plan.tracks.iter().map(|t| t.source).collect::<Vec<_>>(),
            [7, 3, 9]
        );
        assert!(plan.warnings.is_empty());
        assert_disjoint(&plan);
    }

    #[test]
    fn multiple_distinct_mics_are_separate_tracks() {
        let plan = plan(
            &sel(vec![
                mic(1, MicrophoneChoice::Default),
                mic(2, MicrophoneChoice::Device("usb".into())),
            ]),
            SUPPORTED,
        );
        assert_eq!(plan.tracks.len(), 2);
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn duplicate_mic_warns_with_id() {
        let plan = plan(
            &sel(vec![
                mic(1, MicrophoneChoice::Default),
                mic(2, MicrophoneChoice::Default),
            ]),
            SUPPORTED,
        );
        assert_eq!(plan.tracks.len(), 1);
        assert_eq!(plan.warnings, [(2, RoutingWarning::Duplicate)]);
    }

    #[test]
    fn desktop_alone_is_an_honestly_labelled_full_mix() {
        let plan = plan(&sel(vec![desktop(4, Some("dev"))]), SUPPORTED);
        assert_eq!(
            plan.tracks,
            [PlannedTrack {
                source: 4,
                content: TrackContent::DesktopFull,
                stream: StreamPlan::EndpointLoopback(Some("dev".into())),
            }]
        );
        assert_eq!(TrackContent::DesktopFull.label(), "Desktop (full mix)");
    }

    #[test]
    fn desktop_and_one_app_use_exclusion() {
        let plan = plan(
            &sel(vec![desktop(1, None), app(2, "Discord.exe")]),
            SUPPORTED,
        );
        assert_eq!(
            contents(&plan),
            [
                TrackContent::DesktopExcluding("Discord.exe".into()),
                TrackContent::Application("Discord.exe".into())
            ]
        );
        assert_eq!(
            plan.tracks[0].stream,
            StreamPlan::ProcessExclude(exe("Discord.exe"))
        );
        assert_eq!(contents(&plan)[0].label(), "Desktop (without Discord.exe)");
        assert!(plan.warnings.is_empty());
        assert_disjoint(&plan);
    }

    /// Regression (Bug 1): Desktop + Spotify + Discord used to plan no
    /// desktop track at all (`DesktopRemainderUnavailable`), so everything
    /// else (game audio) was silently lost. Desktop must stay planned.
    #[test]
    fn desktop_and_two_apps_keeps_desktop_as_composite() {
        let plan = plan(
            &sel(vec![
                desktop(1, None),
                app(2, "Spotify.exe"),
                app(3, "Discord.exe"),
            ]),
            SUPPORTED,
        );
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        assert_eq!(
            contents(&plan),
            [
                TrackContent::DesktopExcludingMany(vec![
                    "Spotify.exe".into(),
                    "Discord.exe".into()
                ]),
                TrackContent::Application("Spotify.exe".into()),
                TrackContent::Application("Discord.exe".into()),
            ]
        );
        assert_eq!(
            plan.tracks[0].stream,
            StreamPlan::DesktopComposite(vec![exe("Spotify.exe"), exe("Discord.exe")])
        );
        assert_eq!(
            contents(&plan)[0].label(),
            "Desktop (without Spotify.exe, Discord.exe)"
        );
        assert_disjoint(&plan);
    }

    /// Desktop + two apps + an unrelated app that is not separated: the
    /// unrelated one stays in the desktop remainder (not excluded).
    #[test]
    fn unseparated_apps_stay_in_the_desktop_remainder() {
        let plan = plan(
            &sel(vec![
                app(1, "a.exe"),
                desktop(2, None),
                app(3, "b.exe"),
                mic(4, MicrophoneChoice::Default),
            ]),
            SUPPORTED,
        );
        assert_eq!(plan.tracks.len(), 4);
        assert_eq!(
            plan.tracks[1].stream,
            StreamPlan::DesktopComposite(vec![exe("a.exe"), exe("b.exe")])
        );
        assert_disjoint(&plan);
    }

    #[test]
    fn source_ids_never_collide_across_many_sources() {
        let plan = plan(
            &sel(vec![
                desktop(1, None),
                app(2, "a.exe"),
                app(3, "b.exe"),
                app(4, "c.exe"),
                mic(5, MicrophoneChoice::Default),
            ]),
            SUPPORTED,
        );
        let mut ids: Vec<u32> = plan.tracks.iter().map(|t| t.source).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), plan.tracks.len());
        assert_disjoint(&plan);
    }

    #[test]
    fn diff_keeps_unchanged_tracks_and_replaces_reshaped_desktop() {
        let before = plan(&sel(vec![desktop(1, None), app(2, "a.exe")]), SUPPORTED);
        let after = plan(
            &sel(vec![desktop(1, None), app(2, "a.exe"), app(3, "b.exe")]),
            SUPPORTED,
        );
        let d = diff(&before.tracks, &after);
        // The app track keeps running; the desktop changes shape.
        assert_eq!(d.remove, [0]);
        assert_eq!(d.add.iter().map(|t| t.source).collect::<Vec<_>>(), [1, 3]);
        assert!(matches!(d.add[0].stream, StreamPlan::DesktopComposite(_)));
    }

    #[test]
    fn diff_of_identical_plans_is_empty_and_removal_stops_only_that_track() {
        let full = plan(
            &sel(vec![desktop(1, None), mic(2, MicrophoneChoice::Default)]),
            SUPPORTED,
        );
        assert_eq!(diff(&full.tracks, &full), RoutingDiff::default());
        let no_mic = plan(&sel(vec![desktop(1, None)]), SUPPORTED);
        assert_eq!(
            diff(&full.tracks, &no_mic),
            RoutingDiff {
                remove: vec![1],
                add: vec![]
            }
        );
    }

    #[test]
    fn diff_changed_device_restarts_only_that_source() {
        let a = plan(
            &sel(vec![desktop(1, None), mic(2, MicrophoneChoice::Default)]),
            SUPPORTED,
        );
        let b = plan(
            &sel(vec![
                desktop(1, None),
                mic(2, MicrophoneChoice::Device("usb".into())),
            ]),
            SUPPORTED,
        );
        let d = diff(&a.tracks, &b);
        assert_eq!(d.remove, [1]);
        assert_eq!(d.add.len(), 1);
        assert_eq!(d.add[0].source, 2);
    }

    #[test]
    fn second_desktop_with_includes_is_duplicate() {
        let plan = plan(
            &sel(vec![
                app(1, "a.exe"),
                desktop(2, None),
                desktop(3, Some("x")),
            ]),
            SUPPORTED,
        );
        assert_eq!(plan.tracks.len(), 2);
        assert_eq!(plan.warnings, [(3, RoutingWarning::Duplicate)]);
        assert_disjoint(&plan);
    }

    #[test]
    fn desktops_on_different_devices_without_apps_are_both_kept() {
        let plan = plan(
            &sel(vec![desktop(1, None), desktop(2, Some("x"))]),
            SUPPORTED,
        );
        assert_eq!(plan.tracks.len(), 2);
    }

    #[test]
    fn same_device_desktop_twice_is_duplicate() {
        let plan = plan(&sel(vec![desktop(1, None), desktop(2, None)]), SUPPORTED);
        assert_eq!(plan.tracks.len(), 1);
        assert_eq!(plan.warnings, [(2, RoutingWarning::Duplicate)]);
    }

    #[test]
    fn legacy_windows_drops_apps_and_keeps_full_mix() {
        let plan = plan(
            &sel(vec![app(1, "a.exe"), desktop(2, None), app(3, "b.exe")]),
            LEGACY,
        );
        assert_eq!(contents(&plan), [TrackContent::DesktopFull]);
        assert_eq!(
            plan.warnings,
            [
                (1, RoutingWarning::ProcessLoopbackUnsupported),
                (3, RoutingWarning::ProcessLoopbackUnsupported)
            ]
        );
        assert_disjoint(&plan);
    }

    #[test]
    fn duplicate_app_ignores_exe_case_and_keeps_first() {
        let plan = plan(
            &sel(vec![app(1, "Discord.exe"), app(2, "discord.exe")]),
            SUPPORTED,
        );
        assert_eq!(plan.tracks.len(), 1);
        assert_eq!(plan.tracks[0].source, 1);
        assert_eq!(plan.warnings, [(2, RoutingWarning::Duplicate)]);
    }

    #[test]
    fn pid_target_is_labelled_pid() {
        let plan = plan(
            &sel(vec![SourceRequest {
                id: 1,
                kind: SourceRequestKind::Process(ProcessTarget::Pid(42)),
            }]),
            SUPPORTED,
        );
        assert_eq!(
            contents(&plan),
            [TrackContent::Application("pid 42".into())]
        );
    }

    #[test]
    fn every_combination_is_disjoint() {
        for caps in [SUPPORTED, LEGACY] {
            for mask in 0..32u8 {
                let mut sources = Vec::new();
                if mask & 1 != 0 {
                    sources.push(app(1, "a.exe"));
                }
                if mask & 2 != 0 {
                    sources.push(app(2, "b.exe"));
                }
                if mask & 4 != 0 {
                    sources.push(desktop(3, None));
                }
                if mask & 8 != 0 {
                    sources.push(desktop(4, Some("x")));
                }
                if mask & 16 != 0 {
                    sources.push(mic(5, MicrophoneChoice::Default));
                }
                assert_disjoint(&plan(&sel(sources), caps));
            }
        }
    }

    #[test]
    fn empty_selection_plans_nothing() {
        assert!(
            plan(&AudioSelection::default(), SUPPORTED)
                .tracks
                .is_empty()
        );
    }
}
