//! Tray icon with a state-colored dot and the native menu (Windows). On Linux the tray is stubbed:
//! closing hides the window, launching the app again shows it (single instance).

use zapret_core::AppState as CoreState;

#[cfg(not(windows))]
mod imp {
    use super::CoreState;
    use crate::actions::Ctx;

    pub const AVAILABLE: bool = false;

    pub fn create(
        _ctx: Ctx,
        _show: impl Fn() + Send + Sync + 'static,
        _quit: impl Fn() + Send + Sync + 'static,
    ) {
    }

    pub fn update(_state: &CoreState) {}

    pub fn destroy() {}
}

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;

    use tray_icon::menu::{
        CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
    };
    use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};
    use zapret_core::{Command, GameFilterMode, JobKind, RunState, ScanMode};

    use super::CoreState;
    use crate::actions::Ctx;

    pub const AVAILABLE: bool = true;

    struct Tray {
        icon: TrayIcon,
        status: MenuItem,
        toggle: MenuItem,
        game: CheckMenuItem,
        strategies: Submenu,
        strategy_items: Vec<CheckMenuItem>,
        strategy_names: Vec<String>,
        scan: MenuItem,
        color: [u8; 3],
    }

    thread_local! {
        static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    }

    fn circle_icon(color: [u8; 3]) -> Option<Icon> {
        const SIZE: u32 = 32;
        let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        let center = (SIZE as f32 - 1.0) / 2.0;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let distance = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
                let alpha = (14.5 - distance).clamp(0.0, 1.0);
                rgba.extend_from_slice(&[color[0], color[1], color[2], (alpha * 255.0) as u8]);
            }
        }
        Icon::from_rgba(rgba, SIZE, SIZE).ok()
    }

    fn color(state: &CoreState) -> [u8; 3] {
        if state
            .job
            .as_ref()
            .is_some_and(|job| matches!(job.kind, JobKind::Scan | JobKind::Test))
        {
            return [0x00, 0x78, 0xd4];
        }
        match state.run_state {
            RunState::Running => [0x0f, 0x9d, 0x58],
            RunState::Error => [0xc4, 0x2b, 0x1c],
            RunState::Starting => [0xe0, 0xa0, 0x00],
            RunState::Stopped => [0x80, 0x80, 0x80],
        }
    }

    pub fn create(
        ctx: Ctx,
        show: impl Fn() + Send + Sync + 'static,
        quit: impl Fn() + Send + Sync + 'static,
    ) {
        let status = MenuItem::new("zapret-gui", false, None);
        let toggle = MenuItem::with_id("toggle", "Включить обход", true, None);
        let game = CheckMenuItem::with_id("game", "Игровой режим", true, false, None);
        let strategies = Submenu::new("Стратегия", true);
        let scan = MenuItem::with_id("scan", "Автоподбор стратегии", true, None);
        let open = MenuItem::with_id("open", "Открыть", true, None);
        let quit_item = MenuItem::with_id("quit", "Выход", true, None);
        let menu = Menu::new();
        let appended = menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &toggle,
            &game,
            &strategies,
            &scan,
            &PredefinedMenuItem::separator(),
            &open,
            &quit_item,
        ]);
        if let Err(error) = appended {
            tracing::error!("tray menu: {error}");
            return;
        }
        let color = [0x80, 0x80, 0x80];
        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("zapret-gui");
        if let Some(icon) = circle_icon(color) {
            builder = builder.with_icon(icon);
        }
        let icon = match builder.build() {
            Ok(icon) => icon,
            Err(error) => {
                tracing::error!("tray icon: {error}");
                return;
            }
        };

        let show = std::sync::Arc::new(show);
        let show_on_click = show.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                show_on_click();
            }
        }));
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id = event.id().0.as_str();
            let state = crate::bridge::last_state;
            match id {
                "toggle" => {
                    let running = state().is_some_and(|s| {
                        matches!(s.run_state, RunState::Running | RunState::Starting)
                    });
                    ctx.send(if running {
                        Command::Disable
                    } else {
                        Command::Enable
                    });
                }
                "game" => ctx.send(Command::ToggleGameFilter),
                "scan" => ctx.send(Command::RunScan {
                    scan_mode: ScanMode::Quick,
                }),
                "open" => show(),
                "quit" => quit(),
                other => {
                    if let Some(name) = other.strip_prefix("strategy:") {
                        ctx.send(Command::SetActiveStrategy {
                            strategy_name: name.to_string(),
                        });
                    }
                }
            }
        }));

        TRAY.with(|tray| {
            *tray.borrow_mut() = Some(Tray {
                icon,
                status,
                toggle,
                game,
                strategies,
                strategy_items: Vec::new(),
                strategy_names: Vec::new(),
                scan,
                color,
            })
        });
    }

    pub fn update(state: &CoreState) {
        TRAY.with(|tray| {
            let mut tray = tray.borrow_mut();
            let Some(tray) = tray.as_mut() else { return };
            let status = format!(
                "{}{}",
                crate::bridge::status_text(state),
                state
                    .job
                    .as_ref()
                    .map(|job| format!(" — {}", job.message))
                    .unwrap_or_default()
            );
            tray.status.set_text(&status);
            let _ = tray.icon.set_tooltip(Some(format!("zapret-gui: {status}")));
            let running = matches!(state.run_state, RunState::Running | RunState::Starting);
            tray.toggle.set_text(if running {
                "Выключить обход"
            } else {
                "Включить обход"
            });
            let idle = state.job.is_none();
            tray.toggle.set_enabled(idle && state.elevated);
            tray.scan.set_enabled(idle && state.elevated);
            tray.game
                .set_checked(state.game_filter != GameFilterMode::Off);

            let names: Vec<String> = state
                .strategies
                .iter()
                .filter(|strategy| strategy.parse_error.is_none())
                .map(|strategy| strategy.name.clone())
                .collect();
            if names != tray.strategy_names {
                for item in tray.strategy_items.drain(..) {
                    let _ = tray.strategies.remove(&item);
                }
                for name in &names {
                    let item = CheckMenuItem::with_id(
                        MenuId::new(format!("strategy:{name}")),
                        name,
                        true,
                        false,
                        None,
                    );
                    let _ = tray.strategies.append(&item);
                    tray.strategy_items.push(item);
                }
                tray.strategy_names = names;
            }
            for (item, name) in tray.strategy_items.iter().zip(&tray.strategy_names) {
                item.set_checked(state.active_strategy.as_ref() == Some(name));
                item.set_enabled(idle);
            }

            let color = color(state);
            if color != tray.color {
                tray.color = color;
                let _ = tray.icon.set_icon(circle_icon(color));
            }
        });
    }

    pub fn destroy() {
        TRAY.with(|tray| tray.borrow_mut().take());
    }
}

pub use imp::*;
