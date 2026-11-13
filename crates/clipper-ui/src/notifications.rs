//! Non-activating toast window shown when a clip is being saved, saved or failed.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use clipper_platform::windows::notify;
use clipper_storage::NotificationSettings;
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, SharedString, Task, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, div, point, px, rgb, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::components::{ButtonVariant, Tone, button, status_dot};
use crate::theme;

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
const TOAST_W: f32 = 360.0;
const TOAST_H: f32 = 96.0;
const MARGIN: f32 = 16.0;

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
    hide_timer: Option<Task<()>>,
}

impl Notifier {
    pub fn new() -> Self {
        Self {
            window: None,
            hide_timer: None,
        }
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

        let reused = self.window.is_some_and(|handle| {
            let toast = toast.clone();
            handle
                .update(cx, |view, _, cx| {
                    view.toast = toast;
                    view.open_clip = open_clip;
                    cx.notify();
                })
                .is_ok()
        });
        if !reused {
            self.window = open_toast(toast, open_clip, cx);
        }
        let Some(handle) = self.window else {
            self.hide_timer = None;
            return;
        };
        // Replacing the task cancels the previous timer.
        self.hide_timer = Some(cx.spawn(async move |cx| {
            cx.background_executor().timer(plan.hide_after).await;
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }));
    }
}

fn open_toast(toast: Toast, open_clip: bool, cx: &mut App) -> Option<WindowHandle<ToastView>> {
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
    let handle = match cx.open_window(options, |_, cx| cx.new(|_| ToastView { toast, open_clip })) {
        Ok(h) => h,
        Err(err) => {
            eprintln!("notifications: open_window failed: {err}");
            return None;
        }
    };
    let hwnd = handle
        .update(cx, |_, window, _| {
            match window.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
                _ => None,
            }
        })
        .ok()
        .flatten();
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
        return None;
    }
    Some(handle)
}

struct ToastView {
    toast: Toast,
    open_clip: bool,
}

pub(crate) fn run_explorer(arg: impl AsRef<std::ffi::OsStr>) {
    if let Err(err) = Command::new("explorer").arg(arg).spawn() {
        eprintln!("notifications: explorer failed: {err}");
    }
}

impl Render for ToastView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (title, description, tone): (&str, SharedString, Tone) = match &self.toast {
            Toast::Saving => (
                "Saving clip…",
                "Writing the last seconds of your replay.".into(),
                Tone::Warning,
            ),
            Toast::Saved { duration, .. } => (
                "Clip saved",
                format!("Your last {} seconds were saved.", duration.as_secs()).into(),
                Tone::Success,
            ),
            Toast::Failed { message } => ("Clip failed", message.clone().into(), Tone::Danger),
        };

        let mut actions = div().flex().gap_2();
        if let Toast::Saved { path, .. } = &self.toast {
            if self.open_clip {
                let p = path.clone();
                actions = actions.child(
                    button(
                        "toast-open-clip",
                        "Open Clip",
                        ButtonVariant::Primary,
                        None,
                        true,
                    )
                    .on_click(move |_, window, _| {
                        run_explorer(&p);
                        window.remove_window();
                    }),
                );
            }
            let p = path.clone();
            actions = actions.child(
                button(
                    "toast-open-folder",
                    "Open Folder",
                    ButtonVariant::Secondary,
                    None,
                    true,
                )
                .on_click(move |_, window, _| {
                    run_explorer(format!("/select,{}", p.display()));
                    window.remove_window();
                }),
            );
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_xl()
            .border_1()
            .border_color(rgb(theme::BORDER_STRONG))
            .bg(rgb(theme::SURFACE))
            .shadow_lg()
            .text_color(rgb(theme::TEXT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(status_dot(tone))
                    .child(div().flex_1().text_sm().child(title))
                    .child(
                        div()
                            .id("toast-close")
                            .px_1()
                            .cursor_pointer()
                            .text_color(rgb(theme::TEXT_TERTIARY))
                            .hover(|s| s.text_color(rgb(theme::TEXT)))
                            .on_click(|_, window, _| window.remove_window())
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
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
