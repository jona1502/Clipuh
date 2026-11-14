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
use crate::recent_clips::{self, RecentClip, RecentClips, relative_time, size_label};
use crate::theme::{ActiveTheme, Theme};

const LIMIT: usize = 3;

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn meta(clip: &RecentClip, now: SystemTime) -> String {
    let mut parts: Vec<String> = clip.duration.map(clock).into_iter().collect();
    parts.push(size_label(clip.size_bytes));
    parts.push(relative_time(now, clip.modified));
    parts.join(" · ")
}

impl ClipperWindow {
    /// Starts a background reload (scan + probe + thumbnails), replacing the
    /// list when done and freeing the replaced thumbnails.
    pub(super) fn refresh_recent_clips(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.config.clone();
        self.recent_clips_generation += 1;
        let generation = self.recent_clips_generation;
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    match config.recording_options() {
                        Ok(options) => recent_clips::load(&options.output_directory, LIMIT),
                        Err(error) => RecentClips::Unavailable(error.to_string()),
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                // Two saves in quick succession start two scans; an older one
                // finishing last must not hide the newest clip.
                if generation != this.recent_clips_generation {
                    if let RecentClips::Ready(clips) = loaded {
                        for image in clips.into_iter().filter_map(|c| c.thumbnail) {
                            let _ = window.drop_image(image);
                        }
                    }
                    return;
                }
                let old = std::mem::replace(&mut this.recent_clips, loaded);
                if let RecentClips::Ready(clips) = old {
                    for image in clips.into_iter().filter_map(|c| c.thumbnail) {
                        let _ = window.drop_image(image);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        let body = match &self.recent_clips {
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
    let (open, folder): (PathBuf, PathBuf) = (clip.path.clone(), clip.path.clone());
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
            if let Err(error) = shell::open_path(&open) {
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
                .child(div().text_sm().truncate().child(clip.name.clone()))
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
                if let Err(error) = shell::reveal_in_folder(&folder) {
                    this.recording.fail(error.to_string());
                    cx.notify();
                }
            })),
        )
        .into_any_element()
}
