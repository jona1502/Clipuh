use super::ClipperWindow;
use clipper_core::recording::QualityProfile;
use clipper_storage::{
    AppConfig,
    config::{EncoderPreference, ReplayMode, Theme, VideoCodec},
    config_path,
};
use gpui::Context;
use gpui::prelude::*;
use gpui::{AnyElement, Div, SharedString, div, rgb};

use crate::components::{kbd, panel, property_row, section_header, switch, value_text};
use crate::theme;

type Row = (&'static str, Option<&'static str>, AnyElement);

/// One group: header plus a single panel of rows; the first row has no divider.
fn group(title: &'static str, rows: Vec<Row>) -> Div {
    let mut body = panel();
    for (i, (label, description, value)) in rows.into_iter().enumerate() {
        let description = description.map(SharedString::from);
        body = body.child(property_row(label, description, value, i == 0));
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(section_header(title, None))
        .child(body)
}

fn text(value: impl Into<SharedString>) -> AnyElement {
    value_text(value).into_any_element()
}

fn path_text(path: String) -> AnyElement {
    div()
        .max_w(gpui::px(420.))
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(value_text(path))
        .into_any_element()
}

fn toggle(on: bool) -> AnyElement {
    switch(on).into_any_element()
}

pub(super) fn content(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let config: &AppConfig = &window.config;
    let video = &config.video;
    let replay = &config.replay;

    let app = &config.app;
    let perf = &config.performance;

    let codec = match video.codec {
        VideoCodec::H264 => "H.264",
    };
    let encoder = match video.encoder {
        EncoderPreference::Auto => "Auto",
    };
    let mode = match replay.mode {
        ReplayMode::Hybrid => "Hybrid",
    };
    let theme_name = match app.theme {
        Theme::Dark => "Dark",
        Theme::Light => "Light",
        Theme::System => "System",
    };

    let mut root = div()
        .flex()
        .flex_col()
        .gap_6()
        .child(group(
            "Video",
            vec![
                (
                    "Resolution",
                    Some("Resolution, frame rate and quality change on the Dashboard."),
                    text(format!("{}×{}", video.width, video.height)),
                ),
                ("Frame rate", None, text(format!("{} fps", video.fps))),
                (
                    "Quality",
                    None,
                    text(QualityProfile::from(config.recording.quality).label()),
                ),
                ("Codec", None, text(codec)),
                ("Encoder", None, text(encoder)),
                (
                    "Bitrate",
                    None,
                    text(format!("{} Mbps", video.bitrate_mbps)),
                ),
            ],
        ))
        .child(group(
            "Replay",
            vec![
                ("Mode", None, text(mode)),
                (
                    "Duration",
                    None,
                    text(format!("{} s", replay.duration_seconds)),
                ),
                (
                    "Memory budget",
                    Some("Older footage spills to disk."),
                    text(format!("{} MB", replay.memory_limit_megabytes)),
                ),
                (
                    "Save hotkey",
                    None,
                    kbd(replay.hotkey.clone()).into_any_element(),
                ),
            ],
        ))
        .child(super::audio::content(window, cx));

    let recordings = match config.recording_options() {
        Ok(options) => path_text(options.output_directory.display().to_string()),
        Err(error) => text(error.to_string()),
    };
    let settings_file = match config_path() {
        Ok(path) => path_text(path.display().to_string()),
        Err(_) => text("Unavailable"),
    };
    root = root.child(group(
        "Storage",
        vec![
            ("Recordings folder", None, recordings),
            ("Settings file", None, settings_file),
        ],
    ));

    root.child(group(
        "App",
        vec![
            ("Theme", None, text(theme_name)),
            (
                "Automatic game detection",
                None,
                toggle(app.automatic_detection),
            ),
            ("Manual game selection", None, toggle(app.manual_detection)),
            ("Start on login", None, toggle(app.start_on_login)),
            ("Tray icon", None, toggle(app.tray_icon)),
            ("Minimize to tray", None, toggle(app.minimize_to_tray)),
        ],
    ))
    .child(group(
        "Performance",
        vec![
            (
                "Command queue capacity",
                Some("UI to worker commands."),
                text(perf.command_queue_capacity.to_string()),
            ),
            (
                "Event queue capacity",
                Some("Worker to UI events."),
                text(perf.event_queue_capacity.to_string()),
            ),
        ],
    ))
    .child(div().text_xs().text_color(rgb(theme::TEXT_TERTIARY)).child(
        "Other settings are read from settings.toml; editing them in the app is coming later.",
    ))
}
