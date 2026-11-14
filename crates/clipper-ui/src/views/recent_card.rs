//! "Recent Clips" card of the Capture page: the newest saved clips, loaded
//! off the UI thread after startup and after each saved clip or recording.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use clipper_platform::windows::shell;
use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, ObjectFit, RenderImage, SharedString, Window, div, img, px};

use super::{ClipperWindow, Page};
use crate::components::{ButtonVariant, button, card};
use crate::recent_clips::{RecentClip, RecentClips, relative_time, size_label};
use crate::theme::{ActiveTheme, Theme};

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn meta(clip: &RecentClip, now: SystemTime) -> String {
    let mut parts: Vec<String> = clip.clip.duration.map(clock).into_iter().collect();
    parts.push(size_label(clip.clip.size_bytes));
    parts.push(relative_time(
        now,
        clip.clip.created.unwrap_or(clip.clip.modified),
    ));
    parts.join(" · ")
}

impl ClipperWindow {
    /// Starts a background reload (scan + probe + thumbnails), replacing the
    /// list when done and freeing the replaced thumbnails.
    pub(super) fn refresh_recent_clips(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.config.recording_options() {
            Ok(options) => self.library.update(cx, |library, cx| {
                library.set_directory(options.output_directory, window, cx)
            }),
            Err(error) => self.recording.fail(error.to_string()),
        }
    }

    /// Right card of the Capture page.
    pub(super) fn recent_card(&self, cx: &mut Context<Self>) -> Div {
        let t = *cx.theme();
        let view_all = button(
            &t,
            "recent-view-all",
            "View All",
            ButtonVariant::Ghost,
            None,
            true,
        )
        .on_click(cx.listener(|this, _, window, cx| this.navigate(Page::Clips, window, cx)))
        .into_any_element();
        let recent = self.library.read(cx).recent();
        let body = match &recent {
            RecentClips::Loading => note(&t, "Loading clips…"),
            RecentClips::Unavailable(message) => note(&t, message.clone()),
            RecentClips::Ready(clips) if clips.is_empty() => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().child("No clips yet"))
                .child(
                    div()
                        .text_xs()
                        .text_color(t.colors.text_muted)
                        .child("Saved clips will appear here."),
                ),
            RecentClips::Ready(clips) => {
                let now = SystemTime::now();
                let mut rows = Vec::new();
                for (i, clip) in clips.iter().enumerate() {
                    rows.push(row(&t, i, clip, now, cx));
                }
                div().flex().flex_col().gap_2().children(rows)
            }
        };
        card(&t, Some("Recent Clips".into()), Some(view_all))
            .h_full()
            .child(body)
    }
}

fn note(t: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .text_color(t.colors.text_muted)
        .child(text.into())
}

fn row(
    t: &Theme,
    index: usize,
    clip: &RecentClip,
    now: SystemTime,
    cx: &mut Context<ClipperWindow>,
) -> AnyElement {
    let thumb = div()
        .flex_none()
        .w(px(64.))
        .h(px(36.))
        .rounded_md()
        .overflow_hidden()
        .bg(t.colors.surface_raised);
    let thumb = match &clip.thumbnail {
        Some(image) => thumb.child(
            img(Arc::<RenderImage>::clone(image))
                .size_full()
                .object_fit(ObjectFit::Cover),
        ),
        None => thumb,
    };
    let expected_open = clip.clip.clone();
    let expected_folder = clip.clip.clone();
    let (open, folder): (PathBuf, PathBuf) = (clip.clip.path.clone(), clip.clip.path.clone());
    div()
        .id(("recent-clip", index))
        .flex()
        .items_center()
        .gap_3()
        .p_1()
        .rounded_md()
        .cursor_pointer()
        .hover(|style| style.bg(t.colors.surface_hover))
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Err(error) = clipper_storage::library::verify(&expected_open)
                .and_then(|()| shell::open_path(&open))
            {
                this.recording.fail(error.to_string());
                cx.notify();
            }
        }))
        .child(thumb)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().truncate().child(clip.clip.name.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(t.colors.text_muted)
                        .truncate()
                        .child(meta(clip, now)),
                ),
        )
        .child(
            button(
                t,
                ("recent-folder", index),
                "Folder",
                ButtonVariant::Ghost,
                None,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if let Err(error) = clipper_storage::library::verify(&expected_folder)
                    .and_then(|()| shell::reveal_in_folder(&folder))
                {
                    this.recording.fail(error.to_string());
                    cx.notify();
                }
            })),
        )
        .into_any_element()
}
