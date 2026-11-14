use gpui::prelude::*;
use gpui::{Context, Div, div, px};

use super::ClipperWindow;
use crate::components::{ButtonVariant, button, empty_state, panel};
use crate::state::{SavedClip, format_elapsed};
use crate::theme::{ActiveTheme, Theme};
use clipper_platform::windows::shell;

pub(super) fn content(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let t = *cx.theme();
    let clips = &window.recording.saved_clips;
    let count = match clips.len() {
        1 => "1 clip this session".to_owned(),
        n => format!("{n} clips this session"),
    };

    let toolbar = div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .text_sm()
                .text_color(t.colors.text_muted)
                .child(count),
        )
        .child(button(
            &t,
            "sort",
            "Newest",
            ButtonVariant::Secondary,
            None,
            false,
        ));

    let header = div()
        .flex()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(t.colors.border)
        .text_xs()
        .text_color(t.colors.text_muted)
        .child(div().flex_1().min_w_0().child("Name"))
        .child(div().w(px(80.)).child("Duration"))
        .child(div().w(px(290.)).child("Actions"));

    let mut list = panel(&t).child(header);
    if clips.is_empty() {
        list = list.child(empty_state(&t,
            "▤",
            "No clips yet",
            "Replays you save during this session appear here. Turn on instant replay on the Dashboard, then press your save hotkey.",
        ));
    } else {
        list = list.children(
            clips
                .iter()
                .enumerate()
                .map(|(index, clip)| row(&t, index, clip, cx)),
        );
    }

    div().flex().flex_col().gap_3().child(toolbar).child(list)
}

fn row(t: &Theme, index: usize, clip: &SavedClip, cx: &mut Context<ClipperWindow>) -> Div {
    let edit = clip.path.clone();
    let open = clip.path.clone();
    let folder = clip.path.clone();
    div()
        .flex()
        .items_center()
        .px_4()
        .py_2()
        .when(index > 0, |this| {
            this.border_t_1().border_color(t.colors.border)
        })
        .text_sm()
        .child(div().flex_1().min_w_0().truncate().child(clip.name()))
        .child(
            div()
                .w(px(80.))
                .text_color(t.colors.text_secondary)
                .child(format_elapsed(clip.duration)),
        )
        .child(
            div()
                .w(px(290.))
                .flex()
                .gap_2()
                .child(
                    button(
                        t,
                        ("clip-edit", index),
                        "Edit",
                        ButtonVariant::Primary,
                        None,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_in_editor(edit.clone(), window, cx)
                    })),
                )
                .child(
                    button(
                        t,
                        ("clip-open", index),
                        "Open",
                        ButtonVariant::Secondary,
                        None,
                        true,
                    )
                    .on_click(move |_, _, _| {
                        if let Err(err) = shell::open_path(&open) {
                            eprintln!("clips: open failed: {err}");
                        }
                    }),
                )
                .child(
                    button(
                        t,
                        ("clip-folder", index),
                        "Open Folder",
                        ButtonVariant::Ghost,
                        None,
                        true,
                    )
                    .on_click(move |_, _, _| {
                        if let Err(err) = shell::reveal_in_folder(&folder) {
                            eprintln!("clips: open folder failed: {err}");
                        }
                    }),
                ),
        )
}
