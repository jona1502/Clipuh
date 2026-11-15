#![forbid(unsafe_code)]
// Release builds are GUI programs: no console window opens with the app, and
// closing one cannot kill it. Debug builds keep the console for development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
use gpui::actions;

#[cfg(windows)]
actions!(clipper_app, [Quit]);

#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        // Release builds have no console: the box is what the user sees.
        eprintln!("clipuh could not start: {error}");
        clipper_diagnostics::log::error!("clipuh could not start: {error}");
        clipper_diagnostics::flush();
        let mut text = format!("clipuh could not start:\n\n{error}");
        if let Some(dir) = clipper_diagnostics::log_dir() {
            text.push_str(&format!(
                "\n\nDetails are in the log files in {}",
                dir.display()
            ));
        }
        clipper_platform::windows::dialog::show_error("clipuh", &text);
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
    use clipper_platform::windows::tray::{Tray, TrayCommand};
    use clipper_recorder::Recorder;
    use clipper_storage::AppConfig;
    use clipper_ui::ClipperWindow;
    use gpui::prelude::*;
    use gpui::{
        App, Application, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, px, size,
    };

    use clipper_diagnostics::log;
    use clipper_platform::windows::single_instance::{self, Instance};

    // One clipuh per user session: a second start (autostart plus a manual
    // launch) would run a second replay, tray icon and hotkey. It hands over
    // to the running instance before touching the log file or settings.
    // Debug builds use their own name so a dev build runs next to the app.
    let instance_name = if cfg!(debug_assertions) {
        "clipuh-debug"
    } else {
        "clipuh"
    };
    let instance = single_instance::acquire(instance_name);
    if let Ok(Instance::AlreadyRunning(activator)) = &instance {
        if let Err(error) = activator.activate() {
            eprintln!("clipuh is already running and could not be activated: {error}");
        }
        return Ok(());
    }

    // File logging first, at the default level, so a broken settings file is
    // logged too. A logging failure never prevents startup.
    let log_dir = clipper_diagnostics::default_log_dir();
    let logging = clipper_diagnostics::init(&clipper_diagnostics::LogConfig::new(
        log_dir,
        clipper_diagnostics::Verbosity::Info,
    ));
    if let Err(error) = &logging {
        eprintln!("clipuh: file logging unavailable: {error}");
    }
    // Every panic reaches the log; one on the UI thread ends the app, so the
    // user gets a box instead of a window that just vanishes. Background
    // threads only log: the UI reports a stopped recorder itself.
    clipper_diagnostics::install_panic_hook(|report| {
        if report.thread != "main" {
            return;
        }
        let text = format!(
            "clipuh stopped because of an internal error.\n\n{}\n\nThe log files in {} contain the details.",
            report.summary(),
            clipper_diagnostics::default_log_dir().display()
        );
        clipper_platform::windows::dialog::show_error("clipuh", &text);
    });
    log::info!(
        "clipuh {} starting (pid {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    // Without the lock clipuh still starts: refusing to run would be worse
    // than a possible second instance.
    let instance_lock = match instance {
        Ok(Instance::Primary(lock)) => Some(lock),
        Ok(Instance::AlreadyRunning(_)) => None,
        Err(error) => {
            log::warn!("single-instance lock unavailable: {error}");
            None
        }
    };

    // Previous startup policy, superseded to preserve explicit audio device selection:
    // A broken or outdated settings file must not prevent startup. The file is
    // left untouched until the user changes a setting.
    // Reject invalid settings instead of silently recording a different endpoint.
    let config = AppConfig::load().inspect_err(|error| {
        log::error!("settings could not be loaded: {error}");
        clipper_diagnostics::flush();
    })?;
    clipper_ui::apply_log_settings(&config.diagnostics);
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

    // Tray icon on its own thread; picks arrive on the UI thread through this
    // channel. A tray failure (e.g. no shell) never blocks startup.
    let tray = if config.app.tray_icon {
        let (tray_tx, tray_rx) = async_channel::bounded::<TrayCommand>(8);
        match Tray::spawn(move |command| {
            let _ = tray_tx.try_send(command);
        }) {
            Ok(tray) => Some((tray, tray_rx)),
            Err(error) => {
                log::warn!("tray icon unavailable: {error}");
                None
            }
        }
    } else {
        None
    };
    let tray_link = tray
        .as_ref()
        .map(|(tray, commands)| (tray.handle(), commands.clone()));
    // A later start of clipuh asks this one to show its window.
    let (activations_tx, activations) = async_channel::bounded::<()>(1);
    let activation_listener =
        instance_lock
            .as_ref()
            .and_then(|lock| match lock.listen(activations_tx) {
                Ok(listener) => Some(listener),
                Err(error) => {
                    log::warn!("activation requests unavailable: {error}");
                    None
                }
            });
    let window_opened = Rc::new(Cell::new(false));
    let window_opened_in_callback = Rc::clone(&window_opened);

    Application::new().run(move |cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);
        clipper_ui::bind_keys(cx);
        // Every window reads the theme global from its first frame on.
        clipper_ui::apply_theme(&config.appearance, cx);
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

        let bounds = WindowBounds::centered(size(px(1280.), px(900.)), cx);
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                window_min_size: Some(size(px(1080.), px(640.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("clipuh".into()),
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
                        tray_link.clone(),
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
                let activations = activations.clone();
                cx.spawn(async move |cx| {
                    while activations.recv().await.is_ok() {
                        let shown = handle.update(cx, |view, window, _| {
                            view.bring_to_front(window);
                        });
                        if shown.is_err() {
                            return;
                        }
                    }
                })
                .detach();
            }
            Err(error) => {
                log::error!("main window could not be opened: {error}");
                cx.quit();
            }
        }
    });

    // Remove the tray icon and release the global hotkey first so no save is
    // queued during shutdown, then join the recorder worker (finalizing any
    // active recording).
    drop(activation_listener);
    drop(tray);
    drop(hotkey);
    drop(recorder);

    log::info!("clipuh shut down");
    clipper_diagnostics::shutdown();
    // Released last: a new start may begin only once this one is gone.
    drop(instance_lock);

    if !window_opened.get() {
        return Err(std::io::Error::other("GPUI could not open the main window").into());
    }

    Ok(())
}
