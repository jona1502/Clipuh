use clipper_storage::AppConfig;
use gpui::prelude::*;
use gpui::{Div, div};

use crate::components::settings_row;
use crate::state::resolution_label;

pub(super) fn content(config: &AppConfig) -> Div {
    let (summary, directory) = match config.recording_options() {
        Ok(options) => (
            format!(
                "{} \u{b7} {} fps \u{b7} {} \u{b7} {} Mbps",
                resolution_label(options.width, options.height),
                options.fps,
                options.profile.label(),
                options.bitrate_mbps
            ),
            options.output_directory.display().to_string(),
        ),
        Err(error) => ("Unavailable".to_owned(), error.to_string()),
    };

    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(settings_row(
            "Recording",
            "Resolution, frame rate and quality. Change them on the Dashboard.",
            summary,
        ))
        .child(settings_row(
            "Output directory",
            "Recordings are saved here.",
            directory,
        ))
        .child(settings_row(
            "Storage",
            "Clip storage preferences will appear when the clip library is connected.",
            "Not connected",
        ))
        .child(settings_row(
            "About",
            "Clipper desktop workspace",
            "Version 0.1.0",
        ))
}
