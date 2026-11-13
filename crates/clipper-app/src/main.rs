#![forbid(unsafe_code)]

#[cfg(windows)]
use gpui::actions;

#[cfg(windows)]
actions!(clipper_app, [Quit]);

#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("Clipper could not start: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Clipper currently runs on Windows only.");
    std::process::exit(1);
}

#[cfg(windows)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::cell::Cell;
    use std::rc::Rc;

    use clipper_recorder::Recorder;
    use clipper_storage::AppConfig;
    use clipper_ui::ClipperWindow;
    use gpui::prelude::*;
    use gpui::{
        App, Application, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, px, size,
    };

    // Previous startup policy, superseded to preserve explicit audio device selection:
    // A broken or outdated settings file must not prevent startup. The file is
    // left untouched until the user changes a setting.
    // Reject invalid settings instead of silently recording a different endpoint.
    let config = AppConfig::load()?;
    let recorder = Recorder::spawn(
        config.performance.command_queue_capacity as usize,
        config.performance.event_queue_capacity as usize,
    )?;
    let handle = recorder.handle();
    let window_opened = Rc::new(Cell::new(false));
    let window_opened_in_callback = Rc::clone(&window_opened);

    Application::new().run(move |cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);
        clipper_ui::bind_keys(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = WindowBounds::centered(size(px(1200.), px(760.)), cx);
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                window_min_size: Some(size(px(900.), px(620.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Clipper".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| ClipperWindow::new(handle.clone(), config.clone(), window, cx))
            },
        );

        match result {
            Ok(_) => {
                window_opened_in_callback.set(true);
                cx.activate(true);
            }
            Err(error) => {
                eprintln!("Clipper could not open its window: {error}");
                cx.quit();
            }
        }
    });

    // Joins the recorder worker (finalizing any active recording).
    drop(recorder);

    if !window_opened.get() {
        return Err(std::io::Error::other("GPUI could not open the main window").into());
    }

    Ok(())
}
