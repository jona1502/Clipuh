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
//! - desktop with two or more: impossible with a single exclusion, so no
//!   desktop track is planned and a warning explains why. Subtracting signals
//!   is not attempted: loopback copies are not sample-aligned.
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
    Microphone(MicrophoneChoice),
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
    /// Desktop remainder with two or more separate apps cannot be captured.
    DesktopRemainderUnavailable,
    /// Same source selected more than once; recorded only once.
    Duplicate,
}

impl RoutingWarning {
    pub fn message(&self) -> &'static str {
        match self {
            Self::ProcessLoopbackUnsupported => {
                "Per-app audio needs Windows 11 (or Windows 10 build 20348+)."
            }
            Self::DesktopRemainderUnavailable => {
                "Desktop audio is off: Windows can exclude only one app from desktop capture, \
                 so other desktop sounds cannot be separated from several apps."
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
                    _ => out
                        .warnings
                        .push((id, RoutingWarning::DesktopRemainderUnavailable)),
                }
            }
        }
    }
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

    #[test]
    fn desktop_and_two_apps_drops_desktop_with_warning() {
        let plan = plan(
            &sel(vec![app(1, "a.exe"), desktop(2, None), app(3, "b.exe")]),
            SUPPORTED,
        );
        assert_eq!(plan.tracks.len(), 2);
        assert_eq!(
            plan.warnings,
            [(2, RoutingWarning::DesktopRemainderUnavailable)]
        );
        assert_disjoint(&plan);
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
