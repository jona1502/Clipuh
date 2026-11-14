//! Middle card of the Capture page: estimated clip size and encoding settings.

use clipper_storage::storage_projection::StorageProjection;
use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, Window, div, px};

use super::{CaptureMenu, ClipperWindow, Confirm, Dismiss, SelectNext, SelectPrevious};
use crate::components::{card, dropdown};
use crate::state::{ReplayPhase, resolution_label};
use crate::theme::ActiveTheme;

const MENUS: [CaptureMenu; 2] = [CaptureMenu::Quality, CaptureMenu::Bitrate];

/// Estimated clip size in decimal megabytes (10^6 bytes), from the same
/// target rates the projection uses: `(video_bps + audio_bps) * seconds / 8`.
/// `audio_bps` already counts every encoded AAC track exactly once
/// (`encoded_track_count()` = mix + isolated tracks, times the per-track rate;
/// see clipper-storage `storage_projection.rs` and clipper-core
/// `RecordingAudioOptions::track_count`), so it is not re-derived here.
fn clip_size_megabytes(projection: &StorageProjection, seconds: u64) -> f64 {
    (projection.video_bps + projection.audio_bps) as f64 * seconds as f64 / 8.0 / 1_000_000.0
}

/// "≈ 225 MB / 60 sec"; "≈" because VBR output varies around the target.
fn clip_size_label(projection: &StorageProjection, seconds: u64) -> String {
    format!(
        "≈ {:.0} MB / {seconds} sec",
        clip_size_megabytes(projection, seconds)
    )
}

fn video_rate(bps: u64) -> String {
    let mbps = bps as f64 / 1_000_000.0;
    if mbps.fract().abs() < 0.05 {
        format!("{mbps:.0} Mbps")
    } else {
        format!("{mbps:.1} Mbps")
    }
}

fn audio_label(projection: &StorageProjection) -> String {
    match projection.audio_tracks {
        0 => "none".to_owned(),
        n => format!(
            "{n} track{} · {} kbps",
            if n == 1 { "" } else { "s" },
            projection.audio_bps / 1_000
        ),
    }
}

impl ClipperWindow {
    /// Middle card of the Capture page.
    pub(super) fn storage_card(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let t = *cx.theme();
        let cache = &self.storage_projection_cache;
        // Mirror the old overview(): the run snapshot wins while a session runs.
        let run = self.storage_projection_run.displayed();
        let projection = run.map(|s| s.projection()).or(cache.projection());
        let next_differs = self
            .storage_projection_run
            .active()
            .is_some_and(|snapshot| !snapshot.matches_key(cache.key()));
        let caption = if self.storage_projection_run.pending().is_some() {
            "Active (starting)"
        } else if next_differs {
            "Active · next start differs"
        } else if run.is_some() {
            "Active"
        } else {
            "Next start"
        };
        let video = &self.config.video;
        let running_encoder = match &self.recording.replay.phase {
            ReplayPhase::Running { encoder, .. } => Some(encoder.clone()),
            _ => self.recording.encoder.clone(),
        };
        let row = |label: &'static str, value: String| {
            div()
                .flex()
                .justify_between()
                .gap_3()
                .text_xs()
                .child(div().text_color(t.colors.text_muted).child(label))
                .child(div().text_color(t.colors.text_secondary).child(value))
        };

        let mut body = div().flex().flex_col().gap_2();
        if let Some(p) = projection {
            let rate_note = if p.variable_bitrate {
                "VBR target"
            } else {
                "CBR"
            };
            body =
                body.child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(
                    clip_size_label(&p, u64::from(self.config.replay.duration_seconds)),
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(t.colors.text_muted)
                        .child(format!("{caption} · excludes container overhead")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(t.colors.text_secondary)
                        .child(format!(
                            "{} · {} FPS · {}",
                            resolution_label(video.width, video.height),
                            video.fps,
                            running_encoder.unwrap_or_else(|| "Auto (hardware)".to_owned())
                        )),
                )
                .child(row(
                    "Codec",
                    match self.config.video.codec {
                        clipper_storage::config::VideoCodec::H264 => "H.264".into(),
                    },
                ))
                .child(row(
                    "Video bitrate",
                    format!("{} · {rate_note}", video_rate(p.video_bps)),
                ))
                .child(row("Audio", audio_label(&p)));
        } else {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(t.colors.text_secondary)
                    .child(cache.error().unwrap_or("Estimate unavailable").to_owned()),
            );
        }

        let menus = MENUS.into_iter().map(|menu| {
            let index = menu as usize;
            let options = self.capture_options(menu);
            let selected = self.capture_selected(menu);
            let focus = &self.capture_focus[index];
            let highlighted = self
                .capture_menu
                .filter(|(open, _)| *open == menu)
                .map(|(_, h)| h);
            let entity = cx.entity();
            let (toggle, select, dismiss) = (entity.clone(), entity.clone(), entity);
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(t.colors.text_muted)
                        .child(menu.label()),
                )
                .child(dropdown(
                    &t,
                    ("storage-menu", index),
                    &options,
                    selected,
                    highlighted,
                    focus,
                    focus.is_focused(window),
                    true,
                    move |_, app| toggle.update(app, |this, cx| this.capture_toggle(menu, cx)),
                    move |i, _, app| select.update(app, |this, cx| this.capture_apply(menu, i, cx)),
                    move |_, app| dismiss.update(app, |this, cx| this.capture_close(cx)),
                ))
                .on_action(
                    cx.listener(move |this, _: &Confirm, _, cx| this.capture_confirm(menu, cx)),
                )
                .on_action(
                    cx.listener(move |this, _: &SelectNext, _, cx| this.capture_step(menu, 1, cx)),
                )
                .on_action(cx.listener(move |this, _: &SelectPrevious, _, cx| {
                    this.capture_step(menu, -1, cx)
                }))
                .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.capture_close(cx)))
        });

        let mut c = card(&t, Some("Storage & Quality".into()), None)
            .flex_1()
            .min_w(px(240.))
            .child(
                div()
                    .text_xs()
                    .text_color(t.colors.text_muted)
                    .child("Estimated size and encoding settings."),
            )
            .child(body)
            .child(div().flex().gap_3().children(menus));
        if !self.recording.settings_enabled() {
            c = c.child(
                div()
                    .text_xs()
                    .text_color(t.colors.text_muted)
                    .child("Changes apply to the next recording."),
            );
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipper_core::recording::AAC_TRACK_BITRATE_BPS;
    use clipper_storage::AppConfig;

    fn projection(video_bps: u64, tracks: u32) -> StorageProjection {
        let mut p = AppConfig::default().storage_projection().unwrap();
        p.video_bps = video_bps;
        p.audio_tracks = tracks;
        p.audio_bps = u64::from(tracks) * u64::from(AAC_TRACK_BITRATE_BPS);
        p
    }

    #[test]
    fn sizes_scale_with_duration() {
        let p = projection(24_000_000, 2);
        // (24_000_000 + 384_000) / 8 = 3.048 MB/s
        assert_eq!(clip_size_label(&p, 15), "≈ 46 MB / 15 sec");
        assert_eq!(clip_size_label(&p, 60), "≈ 183 MB / 60 sec");
        assert_eq!(clip_size_label(&p, 300), "≈ 914 MB / 300 sec");
    }

    #[test]
    fn zero_audio_is_video_only() {
        let p = projection(8_000_000, 0);
        assert_eq!(clip_size_label(&p, 60), "≈ 60 MB / 60 sec");
        assert_eq!(audio_label(&p), "none");
    }

    #[test]
    fn audio_counted_once_per_encoded_track() {
        let one = projection(0, 1);
        let three = projection(0, 3);
        assert_eq!(
            clip_size_megabytes(&three, 60),
            3.0 * clip_size_megabytes(&one, 60)
        );
        assert_eq!(audio_label(&three), "3 tracks · 576 kbps");
        assert_eq!(audio_label(&one), "1 track · 192 kbps");
    }
}
