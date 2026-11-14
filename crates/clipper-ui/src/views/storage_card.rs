//! Middle card of the Capture page: estimated clip size and encoding settings.

use clipper_core::recording::QualityProfile;
use clipper_storage::config::VideoCodec;
use clipper_storage::storage_projection::StorageProjection;
use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, Window, div, px};

use super::{CaptureMenu, ClipperWindow, Confirm, Dismiss, SelectNext, SelectPrevious};
use crate::components::{Decor, Icon, dropdown_decorated, icon, icon_card, tooltip};
use crate::state::{ReplayPhase, resolution_label};
use crate::theme::ActiveTheme;

/// Estimated clip size in decimal megabytes (10^6 bytes), from the same
/// target rates the projection uses: `(video_bps + audio_bps) * seconds / 8`.
/// `audio_bps` already counts every encoded AAC track exactly once
/// (`encoded_track_count()` = mix + isolated tracks, times the per-track rate;
/// see clipper-storage `storage_projection.rs` and clipper-core
/// `RecordingAudioOptions::track_count`), so it is not re-derived here.
fn clip_size_megabytes(projection: &StorageProjection, seconds: u64) -> f64 {
    (projection.video_bps + projection.audio_bps) as f64 * seconds as f64 / 8.0 / 1_000_000.0
}

/// "225 MB" for a clip of `seconds`; the card shows "≈" before it because VBR
/// output varies around the target.
fn size_megabytes_label(projection: &StorageProjection, seconds: u64) -> String {
    format!("{:.0} MB", clip_size_megabytes(projection, seconds))
}

fn codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
    }
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
    /// Middle card of the Capture page (capture.png): the estimated clip size
    /// as the focal value, a one-line format summary, the encoding details in
    /// small text, and the codec/quality selector at the bottom. Base bitrate
    /// lives in Settings › Video (same apply path).
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
        let quality = QualityProfile::from(self.config.recording.quality).label();
        let seconds = u64::from(self.config.replay.duration_seconds);

        let summary = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_3()
            .rounded_lg()
            .bg(t.colors.surface_raised);
        let summary = match projection {
            Some(p) => {
                let rate_note = if p.variable_bitrate { "VBR" } else { "CBR" };
                summary
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .whitespace_nowrap()
                            .child(div().text_2xl().text_color(t.colors.text_muted).child("≈"))
                            .child(
                                div()
                                    .text_3xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child(size_megabytes_label(&p, seconds)),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .text_color(t.colors.text_secondary)
                                    .child(format!("/ {seconds} s clip")),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(t.colors.text_secondary)
                            .truncate()
                            .child(format!(
                                "{} · {} FPS · {} ({quality})",
                                resolution_label(video.width, video.height),
                                video.fps,
                                codec_label(self.config.video.codec),
                            )),
                    )
                    .child(
                        div()
                            .id("storage-details")
                            .text_xs()
                            .text_color(t.colors.text_muted)
                            .truncate()
                            .tooltip(tooltip(format!(
                                "{caption}. Estimate from the target bitrates; excludes container overhead."
                            )))
                            .child(format!(
                                "{} · video {} {rate_note} · audio {}",
                                running_encoder.unwrap_or_else(|| "Auto (hardware)".to_owned()),
                                video_rate(p.video_bps),
                                audio_label(&p),
                            )),
                    )
            }
            None => summary.child(
                div()
                    .text_sm()
                    .text_color(t.colors.text_secondary)
                    .child(cache.error().unwrap_or("Estimate unavailable").to_owned()),
            ),
        };

        // Codec (H.264 is the only one the encoder offers) + the quality
        // profile, one control like the reference's codec selector.
        let menu = CaptureMenu::Quality;
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
        let leading = div()
            .flex()
            .items_center()
            .gap_2()
            .text_color(t.colors.text_secondary)
            .child(icon(Icon::Video, px(15.)))
            .child(format!("Codec: {} ·", codec_label(self.config.video.codec)));
        let codec = div()
            .id("storage-codec")
            .relative()
            .w_full()
            .tooltip(tooltip("Codec and quality profile"))
            .child(dropdown_decorated(
                &t,
                ("storage-menu", index),
                &options,
                selected,
                highlighted,
                focus,
                focus.is_focused(window),
                true,
                Decor {
                    leading: Some(leading.into_any_element()),
                    ..Decor::default()
                },
                move |_, app| toggle.update(app, |this, cx| this.capture_toggle(menu, cx)),
                move |i, _, app| select.update(app, |this, cx| this.capture_apply(menu, i, cx)),
                move |_, app| dismiss.update(app, |this, cx| this.capture_close(cx)),
            ))
            .on_action(cx.listener(move |this, _: &Confirm, _, cx| this.capture_confirm(menu, cx)))
            .on_action(
                cx.listener(move |this, _: &SelectNext, _, cx| this.capture_step(menu, 1, cx)),
            )
            .on_action(
                cx.listener(move |this, _: &SelectPrevious, _, cx| this.capture_step(menu, -1, cx)),
            )
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.capture_close(cx)));

        icon_card(
            &t,
            Icon::Drive,
            "Storage & Quality",
            "Estimated size and encoding settings.",
            None,
        )
        .child(summary)
        .children((!self.recording.settings_enabled()).then(|| {
            div()
                .text_xs()
                .text_color(t.colors.text_muted)
                .child("Changes apply to the next recording.")
        }))
        // The bottom control's wrapper takes the card's spare height and
        // keeps the control at its end, so all three cards' bottom controls
        // line up (no extra spacer child: that over-measured the row).
        .child(
            div()
                .flex_grow()
                .flex()
                .flex_col()
                .justify_end()
                .child(codec),
        )
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
        assert_eq!(size_megabytes_label(&p, 15), "46 MB");
        assert_eq!(size_megabytes_label(&p, 60), "183 MB");
        assert_eq!(size_megabytes_label(&p, 300), "914 MB");
    }

    #[test]
    fn zero_audio_is_video_only() {
        let p = projection(8_000_000, 0);
        assert_eq!(size_megabytes_label(&p, 60), "60 MB");
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
