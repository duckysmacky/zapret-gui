// No console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod actions;
mod bridge;
mod single_instance;
mod tray;

use std::sync::{Arc, Mutex};

use slint::{CloseRequestResponse, ComponentHandle};
use tokio::sync::broadcast::error::RecvError;
use zapret_core::{AppState as CoreState, Command, CoreEvent, Notification, Paths, Reply};

slint::include_modules!();

fn show_notification(notification: Notification) {
    std::thread::spawn(move || {
        let shown = notify_rust::Notification::new()
            .appname("zapret-gui")
            .summary(&notification.title)
            .body(&notification.body)
            .show();
        if let Err(error) = shown {
            tracing::warn!("notification failed: {error}");
        }
    });
}

/// The Uliana cat picture is not shipped (D32): without the file the cat just isn't shown.
pub(crate) fn load_cat(ui: &MainWindow, paths: &Paths) {
    let file = paths.uliana_dir().join("cat.png");
    match slint::Image::load_from_path(&file) {
        Ok(image) => ui.global::<AppState>().set_cat_image(image),
        Err(error) => tracing::warn!("no Uliana cat at {}: {error:?}", file.display()),
    }
}

/// Core events → UI. State snapshots are coalesced: only the newest one is applied.
fn pump_events(ctx: &actions::Ctx) {
    let mut events = ctx.client.subscribe();
    let window = ctx.window.clone();
    let ctx = ctx.clone();
    ctx.runtime.clone().spawn(async move {
        let pending: Arc<Mutex<Option<Box<CoreState>>>> = Arc::default();
        loop {
            match events.recv().await {
                Ok(CoreEvent::StateChanged(state)) => {
                    let first = pending
                        .lock()
                        .map(|mut slot| slot.replace(state).is_none())
                        .unwrap_or(false);
                    if first {
                        let pending = pending.clone();
                        let _ = window.upgrade_in_event_loop(move |ui| {
                            let state = pending.lock().ok().and_then(|mut slot| slot.take());
                            if let Some(state) = state {
                                bridge::apply(&ui, &state);
                            }
                        });
                    }
                }
                Ok(CoreEvent::LogLine(line)) => {
                    let _ = window.upgrade_in_event_loop(move |ui| bridge::append_log(&ui, &line));
                }
                Ok(CoreEvent::Notification(notification)) => show_notification(notification),
                Err(RecvError::Lagged(_)) => ctx.send_then(Command::GetSnapshot, |ui, reply| {
                    if let Reply::Snapshot(state) = reply {
                        bridge::apply(ui, &state);
                    }
                }),
                Err(RecvError::Closed) => return,
            }
        }
    });
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let from_autostart = args.iter().any(|arg| arg == "--autostart");
    let relaunched = args.iter().any(|arg| arg == "--relaunched");

    let Some(instance) = single_instance::acquire(relaunched)? else {
        // Another instance is running and has been asked to show its window.
        return Ok(());
    };

    let paths = Paths::from_env();
    let core = zapret_core::start(paths.clone())?;
    let ui = MainWindow::new()?;
    let ctx = actions::Ctx {
        client: core.client.clone(),
        runtime: core.runtime(),
        window: ui.as_weak(),
    };

    // Closing the window hides it; Quit (tray or Settings) ends the app and kills winws (D5).
    ui.window()
        .on_close_requested(|| CloseRequestResponse::HideWindow);
    actions::wire(&ui, ctx.clone(), || {
        let _ = slint::quit_event_loop();
    });
    pump_events(&ctx);

    let initial = core
        .runtime()
        .block_on(core.client.request(Command::GetSnapshot));
    if let Ok(Reply::Snapshot(state)) = initial {
        bridge::apply(&ui, &state);
        if state.uliana_mode {
            load_cat(&ui, &paths);
        }
    }
    if let Ok(Reply::Lines(lines)) = core
        .runtime()
        .block_on(core.client.request(Command::GetLogTail))
    {
        bridge::set_logs(&ui, &lines);
    }

    let show_window = {
        let window = ui.as_weak();
        move || {
            let _ = window.upgrade_in_event_loop(|ui| {
                let _ = ui.show();
                ui.window().set_minimized(false);
            });
        }
    };
    single_instance::listen(instance, show_window.clone());
    tray::create(ctx, show_window, || {
        let _ = slint::quit_event_loop();
    });

    if !(from_autostart && tray::AVAILABLE) {
        ui.show()?;
    }
    slint::run_event_loop_until_quit()?;

    tray::destroy();
    let _ = ui.hide();
    core.shutdown();
    Ok(())
}
