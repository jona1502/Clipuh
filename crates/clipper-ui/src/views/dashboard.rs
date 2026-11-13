use clipper_storage::AppConfig;
use clipper_storage::config::{EncoderPreference, ReplayMode, VideoCodec};
use gpui::prelude::*;
use gpui::{Div, div};

use crate::components::{
    ButtonVariant, Tone, badge, button, empty_state, kbd, panel, property_row, section_header,
    stat_tile, value_text,
};

pub(super) fn content(config: &AppConfig) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(status(config))
        .child(overview(config))
        .child(quick_actions(config))
        .child(performance(config))
        .child(recent_clips())
}

fn section(title: &'static str, note: Option<&'static str>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(section_header(title, note.map(Into::into)))
}

fn status(config: &AppConfig) -> Div {
    let replay = format!(
        "Replay buffer inactive · {} s window",
        config.replay.duration_seconds
    );
    section("Status", None).child(
        panel()
            .flex()
            .flex_col()
            .child(property_row(
                "Recording",
                Some("Capture not connected".into()),
                badge("Idle", Tone::Neutral),
                true,
            ))
            .child(property_row(
                "Replay buffer",
                Some(replay.into()),
                badge("Inactive", Tone::Neutral),
                false,
            )),
    )
}

fn overview(config: &AppConfig) -> Div {
    let video = &config.video;
    let codec = match video.codec {
        VideoCodec::H264 => "H.264",
    };
    let encoder = match video.encoder {
        EncoderPreference::Auto => "Auto",
    };
    let mode = match config.replay.mode {
        ReplayMode::Hybrid => "Hybrid",
    };
    let mb_per_minute = video.bitrate_mbps * 60 / 8;

    section("Configuration", Some("From settings")).child(
        div()
            .flex()
            .gap_3()
            .child(
                stat_tile(
                    "Video",
                    format!("{}×{}", video.width, video.height),
                    format!("{} fps", video.fps),
                )
                .flex_1(),
            )
            .child(
                stat_tile(
                    "Encoder",
                    codec,
                    format!("{encoder} · {} Mbps", video.bitrate_mbps),
                )
                .flex_1(),
            )
            .child(
                stat_tile(
                    "Replay buffer",
                    format!("{} s", config.replay.duration_seconds),
                    format!("{mode} · {} MB RAM", config.replay.memory_limit_megabytes),
                )
                .flex_1(),
            )
            .child(
                stat_tile(
                    "Storage",
                    format!("~{mb_per_minute} MB/min"),
                    "Estimate from bitrate",
                )
                .flex_1(),
            ),
    )
}

fn quick_actions(config: &AppConfig) -> Div {
    section("Quick actions", Some("Available once capture is connected")).child(
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(button(
                "save-replay",
                "Save replay",
                ButtonVariant::Primary,
                None,
                false,
            ))
            .child(kbd(config.replay.hotkey.clone()))
            .child(button(
                "start-recording",
                "Start recording",
                ButtonVariant::Secondary,
                None,
                false,
            )),
    )
}

fn performance(config: &AppConfig) -> Div {
    let perf = &config.performance;
    section("Performance", Some("No metrics yet")).child(
        panel()
            .flex()
            .flex_col()
            .child(property_row(
                "Capture latency",
                None,
                value_text("Not measured"),
                true,
            ))
            .child(property_row(
                "Dropped frames",
                None,
                value_text("Not measured"),
                false,
            ))
            .child(property_row(
                "Encoder load",
                None,
                value_text("Not measured"),
                false,
            ))
            .child(property_row(
                "Command queue",
                None,
                value_text(format!("{} slots", perf.command_queue_capacity)),
                false,
            ))
            .child(property_row(
                "Event queue",
                None,
                value_text(format!("{} slots", perf.event_queue_capacity)),
                false,
            )),
    )
}

fn recent_clips() -> Div {
    section("Recent clips", Some("Library is empty")).child(panel().child(empty_state(
        "◌",
        "Nothing to replay yet",
        "When clips are available, your latest highlights will appear here.",
    )))
}
