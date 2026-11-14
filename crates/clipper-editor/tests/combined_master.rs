//! Combined/Master naming, ordering and the no-double-audio rule.

use std::time::Duration;

use clipper_editor::{
    AudioTrack, COMBINED_LABEL, EditSession, arrange_audio_tracks, display_label, effective_gain,
    is_combined,
};

const LEN: Duration = Duration::from_secs(10);

fn track(stream: u32, label: &str) -> AudioTrack {
    AudioTrack::new(stream, label.to_owned(), 2, 48_000, LEN)
}

fn session(tracks: Vec<AudioTrack>) -> EditSession {
    let mut session = EditSession::new(LEN);
    session.set_audio_tracks(arrange_audio_tracks(tracks));
    session
}

fn audible(session: &EditSession) -> Vec<(String, bool)> {
    session
        .audio_tracks()
        .iter()
        .map(|t| {
            (
                display_label(t).to_owned(),
                effective_gain(session.audio_tracks(), t.stream_index) > 0.,
            )
        })
        .collect()
}

#[test]
fn legacy_mix_is_shown_as_combined_master_without_renaming_the_file_label() {
    let mix = track(1, "Mix");
    assert!(is_combined(&mix));
    assert_eq!(display_label(&mix), COMBINED_LABEL);
    assert_eq!(mix.label, "Mix");
    assert!(is_combined(&track(1, "combined/master")));
    let mic = track(2, "Microphone");
    assert!(!is_combined(&mic));
    assert_eq!(display_label(&mic), "Microphone");
}

#[test]
fn combined_is_always_the_first_audio_track_and_sources_keep_their_order() {
    let arranged = arrange_audio_tracks(vec![
        track(3, "game.exe"),
        track(4, "Discord.exe"),
        track(1, "Mix"),
        track(5, "Microphone"),
    ]);
    let order: Vec<u32> = arranged.iter().map(|t| t.stream_index).collect();
    assert_eq!(order, [1, 3, 4, 5]);
    // Re-arranging is stable (refresh, reopen).
    let again: Vec<u32> = arrange_audio_tracks(arranged)
        .iter()
        .map(|t| t.stream_index)
        .collect();
    assert_eq!(again, [1, 3, 4, 5]);
}

#[test]
fn no_combined_track_is_invented_for_unnamed_or_single_streams() {
    let arranged = arrange_audio_tracks(vec![track(1, "Audio 1"), track(2, "Audio 2")]);
    assert!(arranged.iter().all(|t| !is_combined(t) && !t.muted));
    assert_eq!(arranged.len(), 2);
    let single = arrange_audio_tracks(vec![track(1, "Mix")]);
    assert_eq!(single.len(), 1);
    assert!(!single[0].muted);
}

#[test]
fn only_the_combined_track_is_heard_by_default() {
    let session = session(vec![
        track(1, "Mix"),
        track(2, "game.exe"),
        track(3, "Microphone"),
    ]);
    assert_eq!(
        audible(&session),
        [
            (COMBINED_LABEL.to_owned(), true),
            ("game.exe".to_owned(), false),
            ("Microphone".to_owned(), false),
        ]
    );
}

#[test]
fn unmuting_a_source_mutes_combined_and_back_in_one_undo_step() {
    let mut session = session(vec![
        track(1, "Mix"),
        track(2, "game.exe"),
        track(3, "Microphone"),
    ]);
    session.set_track_mute(2, false).unwrap(); // Microphone on
    let heard: Vec<bool> = audible(&session).into_iter().map(|(_, on)| on).collect();
    assert_eq!(heard, [false, false, true], "combined muted, mic heard");
    session.set_track_mute(1, false).unwrap(); // game on too: both sources
    let heard: Vec<bool> = audible(&session).into_iter().map(|(_, on)| on).collect();
    assert_eq!(heard, [false, true, true]);
    session.set_track_mute(0, false).unwrap(); // back to combined
    let heard: Vec<bool> = audible(&session).into_iter().map(|(_, on)| on).collect();
    assert_eq!(heard, [true, false, false]);
    // One undo restores the two-source state exactly.
    assert!(session.undo());
    let heard: Vec<bool> = audible(&session).into_iter().map(|(_, on)| on).collect();
    assert_eq!(heard, [false, true, true]);
}

#[test]
fn enabling_follows_the_same_rule_and_without_combined_nothing_changes() {
    let mut session = session(vec![track(1, "Mix"), track(2, "Microphone")]);
    session.set_track_enabled(0, false).unwrap();
    session.set_track_mute(1, false).unwrap();
    session.set_track_enabled(0, true).unwrap();
    let heard: Vec<bool> = audible(&session).into_iter().map(|(_, on)| on).collect();
    assert_eq!(heard, [true, false]);

    let mut plain = session_without_combined();
    plain.set_track_mute(0, true).unwrap();
    plain.set_track_mute(0, false).unwrap();
    let heard: Vec<bool> = audible(&plain).into_iter().map(|(_, on)| on).collect();
    assert_eq!(heard, [true, true], "independent tracks stay independent");
}

fn session_without_combined() -> EditSession {
    session(vec![track(1, "game.exe"), track(2, "Microphone")])
}
