//! Turns a user [`AudioSelection`] into non-overlapping capture streams.
//!
//! Windows offers three loopback shapes:
//! - endpoint loopback: the full render mix (contains every process),
//! - process loopback INCLUDE: one process tree,
//! - process loopback EXCLUDE: everything except ONE process tree.
//!
//! Tracks must be disjoint so separate tracks and the mixed output never
//! contain a signal twice. Hence:
//! - game / Discord: INCLUDE streams (need process loopback),
//! - desktop with no separately captured process: endpoint loopback, labelled
//!   "full mix",
//! - desktop with exactly one separately captured process: EXCLUDE that one,
//! - desktop with both game and Discord separate: impossible with a single
//!   exclusion, so no desktop track is planned and a warning explains why.
//!   Subtracting signals is not attempted: loopback copies are not
//!   sample-aligned, so subtraction would leave phase artefacts.

use crate::types::{
    AudioCapabilities, AudioSelection, AudioSourceKind, MicrophoneChoice, ProcessTarget,
    TrackContent,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamPlan {
    /// Loopback of the default render device (follows default changes).
    EndpointLoopback,
    /// Only this process tree.
    ProcessInclude(ProcessTarget),
    /// Everything except this process tree. While the target is not running
    /// the remainder equals the full mix, so backends may capture endpoint
    /// loopback until the target appears.
    ProcessExclude(ProcessTarget),
    Microphone(MicrophoneChoice),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedTrack {
    pub content: TrackContent,
    pub stream: StreamPlan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoutingWarning {
    /// Process loopback needs Windows 10 build 20348+ / Windows 11.
    ProcessLoopbackUnsupported(AudioSourceKind),
    /// Desktop remainder without both game and Discord cannot be captured.
    DesktopRemainderUnavailable,
    /// The same process was selected as game and Discord.
    DuplicateProcess,
}

impl RoutingWarning {
    pub fn message(&self) -> &'static str {
        match self {
            Self::ProcessLoopbackUnsupported(AudioSourceKind::Game) => {
                "Separate game audio needs Windows 11 (or Windows 10 build 20348+)."
            }
            Self::ProcessLoopbackUnsupported(_) => {
                "Separate Discord audio needs Windows 11 (or Windows 10 build 20348+)."
            }
            Self::DesktopRemainderUnavailable => {
                "Desktop audio is off: Windows can exclude only one app from desktop capture, \
                 so other desktop sounds cannot be separated from both game and Discord."
            }
            Self::DuplicateProcess => {
                "Game and Discord point to the same app; recorded once as game."
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingPlan {
    pub tracks: Vec<PlannedTrack>,
    pub warnings: Vec<RoutingWarning>,
}

pub fn plan(selection: &AudioSelection, caps: AudioCapabilities) -> RoutingPlan {
    let mut out = RoutingPlan::default();

    let mut game = selection.game.clone();
    let mut discord = selection.discord.clone();
    if game.is_some() && game == discord {
        out.warnings.push(RoutingWarning::DuplicateProcess);
        discord = None;
    }
    if !caps.process_loopback {
        if game.take().is_some() {
            out.warnings
                .push(RoutingWarning::ProcessLoopbackUnsupported(
                    AudioSourceKind::Game,
                ));
        }
        if discord.take().is_some() {
            out.warnings
                .push(RoutingWarning::ProcessLoopbackUnsupported(
                    AudioSourceKind::Discord,
                ));
        }
    }

    if let Some(target) = &game {
        out.tracks.push(PlannedTrack {
            content: TrackContent::Game,
            stream: StreamPlan::ProcessInclude(target.clone()),
        });
    }
    if let Some(target) = &discord {
        out.tracks.push(PlannedTrack {
            content: TrackContent::Discord,
            stream: StreamPlan::ProcessInclude(target.clone()),
        });
    }

    if selection.desktop {
        match (&game, &discord) {
            (None, None) => out.tracks.push(PlannedTrack {
                content: TrackContent::DesktopFull,
                stream: StreamPlan::EndpointLoopback,
            }),
            (Some(target), None) => out.tracks.push(PlannedTrack {
                content: TrackContent::DesktopExcluding(AudioSourceKind::Game),
                stream: StreamPlan::ProcessExclude(target.clone()),
            }),
            (None, Some(target)) => out.tracks.push(PlannedTrack {
                content: TrackContent::DesktopExcluding(AudioSourceKind::Discord),
                stream: StreamPlan::ProcessExclude(target.clone()),
            }),
            (Some(_), Some(_)) => out
                .warnings
                .push(RoutingWarning::DesktopRemainderUnavailable),
        }
    }

    if let Some(choice) = &selection.microphone {
        out.tracks.push(PlannedTrack {
            content: TrackContent::Microphone,
            stream: StreamPlan::Microphone(choice.clone()),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPPORTED: AudioCapabilities = AudioCapabilities {
        process_loopback: true,
    };
    const LEGACY: AudioCapabilities = AudioCapabilities {
        process_loopback: false,
    };

    fn game() -> ProcessTarget {
        ProcessTarget::Executable("game.exe".into())
    }

    fn contents(plan: &RoutingPlan) -> Vec<TrackContent> {
        plan.tracks.iter().map(|t| t.content.clone()).collect()
    }

    /// No two tracks may capture the same process: an INCLUDE target must not
    /// also be audible in a desktop track.
    fn assert_disjoint(plan: &RoutingPlan) {
        let includes: Vec<&ProcessTarget> = plan
            .tracks
            .iter()
            .filter_map(|t| match &t.stream {
                StreamPlan::ProcessInclude(target) => Some(target),
                _ => None,
            })
            .collect();
        for track in &plan.tracks {
            match &track.stream {
                StreamPlan::EndpointLoopback => {
                    assert!(includes.is_empty(), "full mix overlaps {includes:?}")
                }
                StreamPlan::ProcessExclude(excluded) => {
                    assert!(
                        includes.iter().all(|i| *i == excluded),
                        "remainder excluding {excluded:?} overlaps {includes:?}"
                    )
                }
                _ => {}
            }
        }
    }

    #[test]
    fn everything_selected_drops_desktop_with_warning() {
        let selection = AudioSelection {
            game: Some(game()),
            discord: Some(ProcessTarget::discord()),
            desktop: true,
            microphone: Some(MicrophoneChoice::Default),
        };
        let plan = plan(&selection, SUPPORTED);
        assert_eq!(
            contents(&plan),
            [
                TrackContent::Game,
                TrackContent::Discord,
                TrackContent::Microphone
            ]
        );
        assert_eq!(plan.warnings, [RoutingWarning::DesktopRemainderUnavailable]);
        assert_disjoint(&plan);
    }

    #[test]
    fn game_and_desktop_use_exclusion_for_the_remainder() {
        let selection = AudioSelection {
            game: Some(game()),
            desktop: true,
            ..Default::default()
        };
        let plan = plan(&selection, SUPPORTED);
        assert_eq!(
            plan.tracks[1],
            PlannedTrack {
                content: TrackContent::DesktopExcluding(AudioSourceKind::Game),
                stream: StreamPlan::ProcessExclude(game()),
            }
        );
        assert!(plan.warnings.is_empty());
        assert_disjoint(&plan);
    }

    #[test]
    fn discord_and_desktop_use_exclusion_for_the_remainder() {
        let selection = AudioSelection {
            discord: Some(ProcessTarget::discord()),
            desktop: true,
            ..Default::default()
        };
        let plan = plan(&selection, SUPPORTED);
        assert_eq!(
            contents(&plan),
            [
                TrackContent::Discord,
                TrackContent::DesktopExcluding(AudioSourceKind::Discord)
            ]
        );
        assert_disjoint(&plan);
    }

    #[test]
    fn desktop_alone_is_an_honestly_labelled_full_mix() {
        let selection = AudioSelection {
            desktop: true,
            ..Default::default()
        };
        let plan = plan(&selection, SUPPORTED);
        assert_eq!(
            plan.tracks,
            [PlannedTrack {
                content: TrackContent::DesktopFull,
                stream: StreamPlan::EndpointLoopback,
            }]
        );
        assert_eq!(TrackContent::DesktopFull.label(), "Desktop (full mix)");
    }

    #[test]
    fn legacy_windows_falls_back_to_full_mix_and_warns() {
        let selection = AudioSelection {
            game: Some(game()),
            discord: Some(ProcessTarget::discord()),
            desktop: true,
            microphone: None,
        };
        let plan = plan(&selection, LEGACY);
        assert_eq!(contents(&plan), [TrackContent::DesktopFull]);
        assert_eq!(
            plan.warnings,
            [
                RoutingWarning::ProcessLoopbackUnsupported(AudioSourceKind::Game),
                RoutingWarning::ProcessLoopbackUnsupported(AudioSourceKind::Discord),
            ]
        );
        assert_disjoint(&plan);
    }

    #[test]
    fn same_process_for_game_and_discord_is_captured_once() {
        let selection = AudioSelection {
            game: Some(ProcessTarget::discord()),
            discord: Some(ProcessTarget::discord()),
            desktop: true,
            microphone: None,
        };
        let plan = plan(&selection, SUPPORTED);
        assert_eq!(
            contents(&plan),
            [
                TrackContent::Game,
                TrackContent::DesktopExcluding(AudioSourceKind::Game)
            ]
        );
        assert_eq!(plan.warnings, [RoutingWarning::DuplicateProcess]);
        assert_disjoint(&plan);
    }

    #[test]
    fn every_combination_is_disjoint() {
        for caps in [SUPPORTED, LEGACY] {
            for mask in 0..16u8 {
                let selection = AudioSelection {
                    game: (mask & 1 != 0).then(game),
                    discord: (mask & 2 != 0).then(ProcessTarget::discord),
                    desktop: mask & 4 != 0,
                    microphone: (mask & 8 != 0).then_some(MicrophoneChoice::Default),
                };
                assert_disjoint(&plan(&selection, caps));
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
