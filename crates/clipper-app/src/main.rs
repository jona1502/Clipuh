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

    use clipper_core::recording::RecorderCommand;
    use clipper_platform::windows::hotkey::GlobalHotkey;
    use clipper_recorder::Recorder;
    use clipper_storage::AppConfig;
    use clipper_ui::ClipperWindow;
    use gpui::prelude::*;
    use gpui::{
        App, Application, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, px, size,
    };

    // A broken or outdated settings file must not prevent startup. The file is
    // left untouched until the user changes a setting.
    let config = AppConfig::load().unwrap_or_else(|error| {
        eprintln!("Clipper: {error}; using default settings");
        AppConfig::default()
    });
    let recorder = Recorder::spawn(
        config.performance.command_queue_capacity as usize,
        config.performance.event_queue_capacity as usize,
    )?;
    let handle = recorder.handle();

    // The hotkey thread queues SaveReplay itself, so saving never waits on the
    // UI thread. Only a failure to queue is forwarded for a toast.
    let (hotkey_failures_tx, hotkey_failures) = async_channel::bounded::<String>(4);
    let hotkey_recorder = handle.clone();
    let hotkey = GlobalHotkey::spawn(move || {
        if let Err(error) = hotkey_recorder.send(RecorderCommand::SaveReplay) {
            let _ = hotkey_failures_tx.try_send(format!("Could not save the replay: {error}."));
        }
    })?;
    let hotkeys = hotkey.handle();
    let window_opened = Rc::new(Cell::new(false));
    let window_opened_in_callback = Rc::clone(&window_opened);

    Application::new().run(move |cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);
        clipper_ui::bind_keys(cx);
        // Quit with the main window: a visible toast is also a window and must
        // not keep the process (and the global hotkey) alive on its own.
        let main_window = Rc::new(Cell::new(None));
        let main_window_in_callback = Rc::clone(&main_window);
        cx.on_window_closed(move |cx| {
            let main_open = main_window_in_callback
                .get()
                .is_some_and(|main| cx.windows().contains(&main));
            if !main_open {
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
                cx.new(|cx| {
                    ClipperWindow::new(
                        handle.clone(),
                        hotkeys.clone(),
                        hotkey_failures.clone(),
                        config.clone(),
                        window,
                        cx,
                    )
                })
            },
        );

        match result {
            Ok(handle) => {
                main_window.set(Some(handle.into()));
                window_opened_in_callback.set(true);
                cx.activate(true);
            }
            Err(error) => {
                eprintln!("Clipper could not open its window: {error}");
                cx.quit();
            }
        }
    });

    // Release the global hotkey first so no save is queued during shutdown,
    // then join the recorder worker (finalizing any active recording).
    drop(hotkey);
    drop(recorder);

    if !window_opened.get() {
        return Err(std::io::Error::other("GPUI could not open the main window").into());
    }

    Ok(())
}
