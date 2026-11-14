//! Non-activating toast window shown when a clip is being saved, saved or failed.

use std::path::PathBuf;
use std::time::Duration;

use clipper_platform::windows::{notify, shell};
use clipper_storage::NotificationSettings;
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Div, SharedString, Stateful, Task, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point, px, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::components::{ButtonVariant, Tone, button, status_dot};
use crate::theme::ActiveTheme;

#[derive(Clone, Debug, PartialEq)]
pub enum Toast {
    Saving,
    Saved { path: PathBuf, duration: Duration },
    Failed { message: String },
}

/// What to do for one toast request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plan {
    show_window: bool,
    play_sound: bool,
    hide_after: Duration,
}

const SAVING_TIMEOUT: Duration = Duration::from_secs(15);
const MARGIN: f32 = 16.0;
/// How often an expired toast re-checks whether the pointer has left it.
const HOVER_POLL: Duration = Duration::from_millis(250);
// Toast layout in logical px (scaled by DPI when shown). Every row has an
// explicit height, so the window height derives from these and content fits.
const TOAST_W: f32 = 360.0;
const PAD: f32 = 12.0;
const BORDER: f32 = 1.0;
const GAP: f32 = 6.0;
const TITLE_H: f32 = 20.0;
const DESC_LINES: usize = 2;
const DESC_LINE_H: f32 = 16.0;
const DESC_H: f32 = DESC_LINES as f32 * DESC_LINE_H;
const ACTION_H: f32 = 28.0;
const ICON_W: f32 = 18.0;
const TOAST_H: f32 = (PAD + BORDER) * 2.0 + TITLE_H + GAP + DESC_H + GAP + ACTION_H;

/// `None` means do nothing at all (no window, no sound).
fn plan(
    toast: &Toast,
    settings: &NotificationSettings,
    exclusive_fullscreen: bool,
) -> Option<Plan> {
    if !settings.enabled {
        return None;
    }
    let secs = u64::from(settings.duration_seconds);
    let (hide_after, sound) = match toast {
        Toast::Saving => (SAVING_TIMEOUT, false),
        Toast::Saved { .. } => (Duration::from_secs(secs), settings.sound),
        Toast::Failed { .. } if !settings.notify_on_failure => return None,
        Toast::Failed { .. } => (Duration::from_secs(secs + 2), settings.sound),
    };
    Some(Plan {
        show_window: !exclusive_fullscreen,
        play_sound: sound,
        hide_after,
    })
}

/// Owns at most one toast window.
pub struct Notifier {
    window: Option<WindowHandle<ToastView>>,
}

impl Notifier {
    pub fn new() -> Self {
        Self { window: None }
    }

    pub fn show(&mut self, toast: Toast, settings: &NotificationSettings, cx: &mut App) {
        let Some(plan) = plan(&toast, settings, notify::exclusive_fullscreen_active()) else {
            return;
        };
        if plan.play_sound {
            notify::play_notification_sound();
        }
        if !plan.show_window {
            return;
        }
        let open_clip = settings.open_clip_action;

        // A removed window makes `update` fail, which falls through to reopening.
        let reused = self.window.is_some_and(|handle| {
            let toast = toast.clone();
            handle
                .update(cx, |view, window, cx| {
                    view.toast = toast;
                    view.open_clip = open_clip;
                    view.hide_after = plan.hide_after;
                    view.start_timer(window, cx);
                    cx.notify();
                })
                .is_ok()
        });
        if !reused {
            self.window = open_toast(toast, open_clip, plan.hide_after, cx);
        }
    }
}

fn hwnd_of(window: &Window) -> Option<isize> {
    match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
        _ => None,
    }
}

fn open_toast(
    toast: Toast,
    open_clip: bool,
    hide_after: Duration,
    cx: &mut App,
) -> Option<WindowHandle<ToastView>> {
    let Some(display) = cx.primary_display() else {
        eprintln!("notifications: no primary display");
        return None;
    };
    let area = display.bounds();
    let origin = point(
        area.origin.x + area.size.width - px(TOAST_W + MARGIN),
        area.origin.y + area.size.height - px(TOAST_H + MARGIN),
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            origin,
            size(px(TOAST_W), px(TOAST_H)),
        ))),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };
    let handle = match cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            let mut view = ToastView {
                toast,
                open_clip,
                hide_after,
                hide_task: None,
            };
            view.start_timer(window, cx);
            view
        })
    }) {
        Ok(h) => h,
        Err(err) => {
            eprintln!("notifications: open_window failed: {err}");
            return None;
        }
    };
    let hwnd = handle
        .update(cx, |_, window, _| hwnd_of(window))
        .ok()
        .flatten();
    // Sizing sends WM_SIZE synchronously. Inside this update the app is
    // borrowed, so GPUI drops the resize and keeps laying the toast out at the
    // hidden window's default size: everything below the title ends up
    // outside the 360x118 window. A task runs outside the update.
    cx.spawn(async move |cx| {
        let shown = match hwnd {
            Some(hwnd) => {
                notify::show_without_activation(hwnd, TOAST_W as i32, TOAST_H as i32, MARGIN as i32)
                    .map_err(|e| e.to_string())
            }
            None => Err("no Win32 window handle".to_string()),
        };
        if let Err(err) = shown {
            eprintln!("notifications: could not show toast: {err}");
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    })
    .detach();
    Some(handle)
}

struct ToastView {
    toast: Toast,
    open_clip: bool,
    hide_after: Duration,
    /// Dropping the task cancels the auto-hide.
    hide_task: Option<Task<()>>,
}

impl ToastView {
    /// (Re)starts the auto-hide countdown with the full `hide_after`.
    /// (Re)starts the auto-hide countdown with the full `hide_after`. While the
    /// pointer is over the toast it stays open. Checked with Win32 because the
    /// non-activated toast gets no reliable hover-leave events from GPUI.
    fn start_timer(&mut self, window: &Window, cx: &mut Context<Self>) {
        let after = self.hide_after;
        self.hide_task = Some(cx.spawn_in(window, async move |_, cx| {
            cx.background_executor().timer(after).await;
            loop {
                let Ok(hovered) =
                    cx.update(|window, _| hwnd_of(window).is_some_and(notify::cursor_over_window))
                else {
                    return;
                };
                if !hovered {
                    break;
                }
                cx.background_executor().timer(HOVER_POLL).await;
            }
            let _ = cx.update(|window, _| window.remove_window());
        }));
    }

    /// Closes the toast on success, else shows the error in place.
    fn finish_action(
        &mut self,
        result: std::io::Result<()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Err(err) = result else {
            window.remove_window();
            return;
        };
        let message = if err.kind() == std::io::ErrorKind::NotFound {
            "Clip file not found. It may have been moved or deleted.".to_string()
        } else {
            err.to_string()
        };
        self.toast = Toast::Failed { message };
        self.start_timer(window, cx);
        cx.notify();
    }
}

/// Pins a button to the action row: fixed height, never shrinks or wraps.
fn compact(b: Stateful<Div>) -> Stateful<Div> {
    b.flex_none().h(px(ACTION_H)).whitespace_nowrap()
}

impl Render for ToastView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *cx.theme();
        let (title, description, tone): (&str, SharedString, Tone) = match &self.toast {
            Toast::Saving => (
                "Saving clip…",
                "Writing the last seconds of your replay.".into(),
                Tone::Warning,
            ),
            Toast::Saved { duration, .. } => (
                "Clip saved",
                format!("Last {} seconds captured", duration.as_secs()).into(),
                Tone::Success,
            ),
            Toast::Failed { message } => ("Clip not saved", message.clone().into(), Tone::Danger),
        };

        let mut actions = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(ACTION_H))
            .pl(px(ICON_W));
        if let Toast::Saved { path, .. } = &self.toast {
            if self.open_clip {
                let p = path.clone();
                actions = actions.child(compact(
                    button(
                        &t,
                        "toast-open-clip",
                        "Open Clip",
                        ButtonVariant::Primary,
                        None,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.finish_action(shell::open_path(&p), window, cx);
                    })),
                ));
            }
            let p = path.clone();
            actions = actions.child(compact(
                button(
                    &t,
                    "toast-open-folder",
                    "Open Folder",
                    ButtonVariant::Secondary,
                    None,
                    true,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.finish_action(shell::reveal_in_folder(&p), window, cx);
                })),
            ));
        }
        actions = actions.child(div().flex_1()).child(
            compact(
                button(&t, "toast-dismiss", "×", ButtonVariant::Ghost, None, true)
                    .w(px(ACTION_H))
                    .px_0()
                    .justify_center(),
            )
            .on_click(|_, window, _| window.remove_window()),
        );

        div()
            .id("toast")
            .size_full()
            .flex()
            .flex_col()
            .justify_between()
            .p(px(PAD))
            .rounded_xl()
            .border_1()
            .border_color(t.colors.border_strong)
            .bg(t.colors.surface)
            .shadow_lg()
            .text_color(t.colors.text)
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(TITLE_H))
                    .child(
                        div()
                            .w(px(ICON_W))
                            .flex_none()
                            .flex()
                            .items_center()
                            .child(status_dot(&t, tone)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .line_height(px(TITLE_H))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(title),
                    ),
            )
            .child(
                div()
                    .h(px(DESC_H))
                    .pl(px(ICON_W))
                    .overflow_hidden()
                    .text_xs()
                    .line_height(px(DESC_LINE_H))
                    .line_clamp(DESC_LINES)
                    .text_color(t.colors.text_secondary)
                    .child(description),
            )
            .child(actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> NotificationSettings {
        NotificationSettings {
            enabled: true,
            duration_seconds: 5,
            sound: true,
            open_clip_action: true,
            notify_on_failure: true,
        }
    }
    fn saved() -> Toast {
        Toast::Saved {
            path: "a.mp4".into(),
            duration: Duration::from_secs(30),
        }
    }
    fn failed() -> Toast {
        Toast::Failed {
            message: "x".into(),
        }
    }

    #[test]
    fn toast_height_fits_rows() {
        let rows = TITLE_H + DESC_H + ACTION_H + 2.0 * GAP + 2.0 * (PAD + BORDER);
        assert!(TOAST_H >= rows);
        assert_eq!(DESC_H, DESC_LINES as f32 * DESC_LINE_H);
    }

    #[test]
    fn disabled_does_nothing() {
        let mut st = s();
        st.enabled = false;
        for t in [Toast::Saving, saved(), failed()] {
            assert_eq!(plan(&t, &st, false), None);
        }
    }

    #[test]
    fn failure_suppressed() {
        let mut st = s();
        st.notify_on_failure = false;
        assert_eq!(plan(&failed(), &st, false), None);
        assert!(plan(&saved(), &st, false).is_some());
    }

    #[test]
    fn durations_and_sound() {
        let p = plan(&saved(), &s(), false).unwrap();
        assert_eq!(
            (p.hide_after, p.play_sound, p.show_window),
            (Duration::from_secs(5), true, true)
        );
        let p = plan(&failed(), &s(), false).unwrap();
        assert_eq!(p.hide_after, Duration::from_secs(7));
        assert!(p.play_sound);
        let p = plan(&Toast::Saving, &s(), false).unwrap();
        assert_eq!(p.hide_after, Duration::from_secs(15));
        assert!(!p.play_sound);
        let mut st = s();
        st.sound = false;
        assert!(!plan(&saved(), &st, false).unwrap().play_sound);
    }

    #[test]
    fn exclusive_fullscreen_no_window() {
        let p = plan(&saved(), &s(), true).unwrap();
        assert!(!p.show_window);
        assert!(p.play_sound);
        let mut st = s();
        st.sound = false;
        assert!(!plan(&saved(), &st, true).unwrap().play_sound);
    }
}
