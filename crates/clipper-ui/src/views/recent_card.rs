//! "Recent Clips" card of the Capture page: the newest saved clips, loaded
//! off the UI thread after startup and after each saved clip or recording.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use clipper_platform::windows::shell;
use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Div, ObjectFit, RenderImage, SharedString, Window, anchored, deferred,
    div, img, px,
};

use super::{ClipperWindow, Page};
use crate::components::{
    ButtonVariant, Icon, RasterIcon, button_with_icon, icon, icon_card, raster_icon, tooltip,
};
use crate::recent_clips::{RecentClip, RecentClips, relative_time};
use crate::theme::{ActiveTheme, Theme};

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// The row's second line: what the clip came from (the recorded source when
/// known, else replay or recording) and when it was saved.
fn origin(clip: &RecentClip, now: SystemTime) -> String {
    let what = match &clip.clip.source {
        Some(source) => source.clone(),
        None if clip.clip.is_replay() => "Instant Replay".to_owned(),
        None => "Recording".to_owned(),
    };
    format!(
        "{what} · {}",
        relative_time(now, clip.clip.created.unwrap_or(clip.clip.modified))
    )
}

/// Thumbnail size: 16:9, compact like the reference rows.
const THUMB_W: f32 = 72.0;
const THUMB_H: f32 = 40.5;

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

    fn recent_menu_toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        self.recent_menu = if self.recent_menu == Some(index) {
            None
        } else {
            Some(index)
        };
        cx.notify();
    }

    /// Right card of the Capture page (capture.png): the three newest clips
    /// as compact rows, then View All Clips.
    pub(super) fn recent_card(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let t = *cx.theme();
        let recent = self.library.read(cx).recent();
        let body = match &recent {
            RecentClips::Loading => note(&t, "Loading clips…"),
            RecentClips::Unavailable(message) => note(&t, message.clone()),
            RecentClips::Ready(clips) if clips.is_empty() => div()
                .flex()
                .flex_col()
                .gap_1()
                .py_2()
                .child(div().text_sm().child("No clips yet"))
                .child(
                    div()
                        .text_xs()
                        .text_color(t.colors.text_muted)
                        .child("Save a replay with your Save Clip hotkey."),
                ),
            RecentClips::Ready(clips) => {
                let now = SystemTime::now();
                let rows: Vec<AnyElement> = clips
                    .iter()
                    .enumerate()
                    .map(|(i, clip)| self.recent_row(&t, i, clip, now, window, cx))
                    .collect();
                div().flex().flex_col().gap_1().children(rows)
            }
        };
        let view_all = button_with_icon(
            &t,
            "recent-view-all",
            icon(Icon::ChevronRight, px(12.)).into_any_element(),
            "View All Clips",
            ButtonVariant::Secondary,
            true,
        )
        .w_full()
        .justify_center()
        .flex_row_reverse()
        .on_click(cx.listener(|this, _, window, cx| this.navigate(Page::Clips, window, cx)));
        icon_card(
            &t,
            Icon::Video,
            "Recent Clips",
            "Your latest saved moments.",
            None,
        )
        .child(body)
        // The bottom control's wrapper takes the card's spare height and
        // keeps the control at its end, so all three cards' bottom controls
        // line up (no extra spacer child: that over-measured the row).
        .child(
            div()
                .flex_grow()
                .flex()
                .flex_col()
                .justify_end()
                .child(view_all),
        )
    }

    fn recent_row(
        &self,
        t: &Theme,
        index: usize,
        clip: &RecentClip,
        now: SystemTime,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut thumb = div()
            .relative()
            .flex_none()
            .w(px(THUMB_W))
            .h(px(THUMB_H))
            .rounded_md()
            .overflow_hidden()
            .bg(t.colors.surface_active)
            .flex()
            .items_center()
            .justify_center();
        thumb = match &clip.thumbnail {
            Some(image) => thumb.child(
                img(Arc::<RenderImage>::clone(image))
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            ),
            // Not decoded (yet): a neutral tile, never a fake frame.
            None => thumb.child(icon(Icon::Video, px(16.)).text_color(t.colors.text_muted)),
        };
        if let Some(duration) = clip.clip.duration {
            thumb = thumb.child(
                div()
                    .absolute()
                    .bottom(px(3.))
                    .right(px(3.))
                    .px_1()
                    .rounded_sm()
                    .bg(gpui::black().opacity(0.72))
                    .text_color(gpui::white())
                    .text_size(px(10.))
                    .child(clock(duration)),
            );
        }

        let play_path = clip.clip.path.clone();
        let play_check = clip.clip.clone();
        let play = row_button(t, ("recent-play", index), "Play")
            .child(raster_icon(
                RasterIcon::Play,
                px(14.),
                t.colors.text_secondary,
                window,
            ))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if let Err(error) = clipper_storage::library::verify(&play_check)
                    .and_then(|()| shell::open_path(&play_path))
                {
                    this.recording.fail(error.to_string());
                    cx.notify();
                }
            }));

        let open = self.recent_menu == Some(index);
        let more = row_button(t, ("recent-more", index), "More actions")
            .when(open, |this| this.bg(t.colors.surface_hover))
            .child(raster_icon(
                RasterIcon::More,
                px(14.),
                t.colors.text_secondary,
                window,
            ))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.recent_menu_toggle(index, cx);
            }));
        let menu = open.then(|| {
            let edit = clip.clip.path.clone();
            let folder = clip.clip.path.clone();
            let folder_check = clip.clip.clone();
            let item = |id: &'static str, glyph: Icon, label: &'static str| {
                div()
                    .id((id, index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(t.colors.surface_hover))
                    .child(icon(glyph, px(14.)).text_color(t.colors.text_secondary))
                    .child(label)
            };
            deferred(
                anchored().snap_to_window_with_margin(px(8.)).child(
                    div()
                        .id(("recent-menu", index))
                        .occlude()
                        .w(px(180.))
                        .p_1()
                        .flex()
                        .flex_col()
                        .rounded_lg()
                        .border_1()
                        .border_color(t.colors.border_strong)
                        .bg(t.colors.surface)
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.recent_menu = None;
                            cx.notify();
                        }))
                        .child(
                            item("recent-menu-edit", Icon::Cut, "Open in Editor").on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.recent_menu = None;
                                    this.open_in_editor(edit.clone(), window, cx);
                                }),
                            ),
                        )
                        .child(
                            item("recent-menu-folder", Icon::FolderOpen, "Open Folder").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.recent_menu = None;
                                    if let Err(error) =
                                        clipper_storage::library::verify(&folder_check)
                                            .and_then(|()| shell::reveal_in_folder(&folder))
                                    {
                                        this.recording.fail(error.to_string());
                                    }
                                    cx.notify();
                                }),
                            ),
                        ),
                ),
            )
        });

        div()
            .id(("recent-clip", index))
            .flex()
            .items_center()
            .gap_3()
            .p_1()
            .rounded_lg()
            .bg(t.colors.surface_raised)
            .child(thumb)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .truncate()
                            .child(clip.clip.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(t.colors.text_muted)
                            .truncate()
                            .child(origin(clip, now)),
                    ),
            )
            .child(play)
            .child(div().relative().flex_none().child(more).children(
                menu.map(|menu| div().absolute().top(px(32.)).right_0().size_0().child(menu)),
            ))
            .into_any_element()
    }
}

fn note(t: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .text_color(t.colors.text_muted)
        .child(text.into())
}

/// Square row action (Play, ⋯) with a tooltip; the icon goes in as a child.
fn row_button(
    t: &Theme,
    id: impl Into<gpui::ElementId>,
    label: &'static str,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(t.colors.border)
        .cursor_pointer()
        .hover(|style| style.bg(t.colors.surface_hover))
        .tooltip(tooltip(label))
}
