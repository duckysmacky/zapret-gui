//! Slint callbacks → core commands. Requests run on the core runtime; replies come back via the event loop.

use std::sync::Arc;

use slint::ComponentHandle;
use zapret_core::{
    Command, CoreClient, FakeSlot, Reply, ScanMode, Settings, TestMode, Theme, UserListKind,
};

use crate::bridge::{self, FIXES};
use crate::{AppState, Commands, Dialog, MainWindow};

const MAX_LIST_VIEW_LINES: usize = 5000;

#[derive(Clone)]
pub struct Ctx {
    pub client: Arc<dyn CoreClient>,
    pub runtime: tokio::runtime::Handle,
    pub window: slint::Weak<MainWindow>,
}

impl Ctx {
    pub fn send(&self, command: Command) {
        self.send_then(command, |_, _| {});
    }

    /// Runs the command; `then` gets the reply on the UI thread. Errors are shown in the message bar.
    pub fn send_then(
        &self,
        command: Command,
        then: impl FnOnce(&MainWindow, Reply) + Send + 'static,
    ) {
        let client = self.client.clone();
        let window = self.window.clone();
        self.runtime.spawn(async move {
            let result = client.request(command).await;
            let _ = window.upgrade_in_event_loop(move |ui| match result {
                Ok(reply) => then(&ui, reply),
                Err(error) => ui
                    .global::<AppState>()
                    .set_message(error.to_string().into()),
            });
        });
    }
}

fn message(ui: &MainWindow, text: impl Into<String>) {
    ui.global::<AppState>().set_message(text.into().into());
}

fn settings_with(key: &str, value: i32) -> Option<Settings> {
    let mut settings = bridge::last_state()?.settings;
    let flag = value != 0;
    match key {
        "theme" => {
            settings.theme = match value {
                1 => Theme::Light,
                2 => Theme::Dark,
                _ => Theme::System,
            }
        }
        "autostart" => settings.autostart = flag,
        "start-on-launch" => settings.start_on_launch = flag,
        "watchdog" => settings.watchdog_enabled = flag,
        "watchdog-period" => settings.watchdog_period_minutes = value.clamp(1, 1440) as u32,
        "auto-rescan" => settings.auto_rescan = flag,
        "auto-check" => settings.auto_check_updates = flag,
        _ => return None,
    }
    Some(settings)
}

fn fix_command(id: i32) -> Option<Command> {
    let fix = *FIXES.get(usize::try_from(id).ok()?)?;
    Some(Command::ApplyFix { fix })
}

fn show_lines_reply(ui: &MainWindow, reply: Reply) {
    if let Reply::Lines(lines) = reply {
        message(ui, lines.join("\n"));
    }
}

pub fn wire(ui: &MainWindow, ctx: Ctx, quit: impl Fn() + 'static) {
    let commands = ui.global::<Commands>();

    let c = ctx.clone();
    commands.on_restart_as_admin(move || {
        c.send_then(Command::RestartAsAdmin, |_, _| {
            let _ = slint::quit_event_loop();
        })
    });
    let c = ctx.clone();
    commands.on_enable(move || c.send(Command::Enable));
    let c = ctx.clone();
    commands.on_disable(move || c.send(Command::Disable));
    let c = ctx.clone();
    commands.on_select_strategy(move |name| {
        c.send(Command::SetActiveStrategy {
            strategy_name: name.to_string(),
        })
    });
    let c = ctx.clone();
    commands.on_set_game_filter(move |index| {
        c.send(Command::SetGameFilter {
            game_filter_mode: bridge::game_filter_from_index(index),
        })
    });
    let c = ctx.clone();
    commands.on_set_game_ports(move |tcp, udp| {
        c.send_then(
            Command::SetGameFilterPorts {
                tcp_ports: tcp.to_string(),
                udp_ports: udp.to_string(),
            },
            |ui, _| message(ui, "Порты сохранены."),
        )
    });
    let c = ctx.clone();
    commands.on_set_ipset_mode(move |index| {
        c.send(Command::SetIpsetMode {
            ipset_mode: bridge::ipset_from_index(index),
        })
    });
    let c = ctx.clone();
    commands.on_update_ipset_list(move || {
        c.send_then(Command::UpdateIpsetList, |ui, _| {
            message(ui, "Список IPSet обновлён.")
        })
    });
    let c = ctx.clone();
    commands.on_run_scan(move |full| {
        c.send(Command::RunScan {
            scan_mode: if full {
                ScanMode::Full
            } else {
                ScanMode::Quick
            },
        })
    });
    let c = ctx.clone();
    commands.on_test_strategy(move |name| {
        c.send(Command::TestStrategies {
            strategy_names: vec![name.to_string()],
            test_mode: TestMode::Standard,
        })
    });
    let c = ctx.clone();
    commands.on_run_tests(move |mode, all| {
        let names = if all {
            Vec::new()
        } else {
            match bridge::last_state().and_then(|state| state.active_strategy) {
                Some(active) => vec![active],
                None => {
                    if let Some(ui) = c.window.upgrade() {
                        message(&ui, "Стратегия не выбрана.");
                    }
                    return;
                }
            }
        };
        c.send(Command::TestStrategies {
            strategy_names: names,
            test_mode: if mode == 1 {
                TestMode::Dpi
            } else {
                TestMode::Standard
            },
        })
    });
    let c = ctx.clone();
    commands.on_cancel_job(move || c.send(Command::CancelJob));
    let c = ctx.clone();
    commands.on_check_updates(move || c.send(Command::CheckForUpdates));
    let c = ctx.clone();
    commands.on_select_version(move |tag| {
        c.send(Command::SelectUpstreamVersion {
            version_tag: tag.to_string(),
        })
    });
    let c = ctx.clone();
    commands.on_delete_version(move |tag| {
        c.send(Command::DeleteUpstreamVersion {
            version_tag: tag.to_string(),
        })
    });
    let c = ctx.clone();
    commands.on_save_list(move |kind, text| {
        let list_kind = match kind {
            1 => UserListKind::Exclude,
            2 => UserListKind::IpsetExclude,
            _ => UserListKind::General,
        };
        let entries = text.lines().map(str::to_string).collect();
        c.send_then(
            Command::UpdateUserList { list_kind, entries },
            |ui, reply| match reply {
                Reply::Rejected(rejected) if !rejected.is_empty() => message(
                    ui,
                    format!(
                        "Сохранено. Не приняты (не домен / не IP): {}",
                        rejected.join(", ")
                    ),
                ),
                _ => message(ui, "Список сохранён."),
            },
        )
    });
    let c = ctx.clone();
    commands.on_load_upstream_list(move |name, filter| {
        let filter = filter.to_lowercase();
        c.send_then(
            Command::GetUpstreamList {
                file_name: name.to_string(),
            },
            move |ui, reply| {
                if let Reply::Text(text) = reply {
                    let lines: Vec<&str> = text
                        .lines()
                        .filter(|line| filter.is_empty() || line.to_lowercase().contains(&filter))
                        .take(MAX_LIST_VIEW_LINES)
                        .collect();
                    ui.global::<AppState>()
                        .set_upstream_list_text(lines.join("\n").into());
                }
            },
        )
    });
    let c = ctx.clone();
    commands.on_run_diagnostics(move || c.send(Command::RunDiagnostics));
    let c = ctx.clone();
    commands.on_apply_fix(move |id| {
        if let Some(command) = fix_command(id) {
            c.send_then(command, show_lines_reply);
        }
    });
    let c = ctx.clone();
    commands.on_check_hosts(move || {
        c.send_then(Command::PrepareHostsUpdate, |ui, reply| {
            let Reply::Lines(lines) = reply else { return };
            if lines.is_empty() {
                message(ui, "Файл hosts уже содержит все записи zapret.");
                return;
            }
            let mut preview: Vec<String> = lines.iter().take(25).cloned().collect();
            if lines.len() > preview.len() {
                preview.push(format!("… и ещё {}", lines.len() - preview.len()));
            }
            ui.global::<Dialog>().invoke_ask(
                "Обновление hosts".into(),
                format!(
                    "В файл hosts будут добавлены {} строк (перед этим создаётся резервная копия, ваши записи не удаляются):\n\n{}",
                    lines.len(),
                    preview.join("\n")
                )
                .into(),
                "apply-hosts".into(),
                "Добавить".into(),
            );
        })
    });
    let c = ctx.clone();
    commands.on_open_report(move || {
        c.send_then(Command::GetDiagnosticsReport, |ui, reply| {
            if let Reply::Text(text) = reply {
                ui.global::<AppState>().set_report_text(text.into());
            }
        })
    });
    let c = ctx.clone();
    commands.on_set_setting(move |key, value| {
        if let Some(settings) = settings_with(key.as_str(), value) {
            c.send(Command::UpdateSettings { settings });
        }
    });
    let c = ctx.clone();
    commands.on_set_fake(move |slot, index| {
        let fake_name = if index <= 0 {
            None
        } else {
            bridge::last_state()
                .and_then(|state| state.fakes)
                .and_then(|fakes| fakes.available.get(index as usize - 1).cloned())
        };
        c.send(Command::SetFake {
            fake_slot: if slot == 1 {
                FakeSlot::GameUdp
            } else {
                FakeSlot::DiscordUdp
            },
            fake_name,
        })
    });
    let c = ctx.clone();
    commands.on_finish_first_run(move || c.send(Command::FinishFirstRun));
    let c = ctx.clone();
    commands.on_confirmed(move |action| {
        let action = action.as_str();
        if let Some(tag) = action.strip_prefix("delete-version:") {
            c.send(Command::DeleteUpstreamVersion {
                version_tag: tag.to_string(),
            });
        } else if let Some(id) = action.strip_prefix("fix:") {
            if let Some(command) = id.parse().ok().and_then(fix_command) {
                c.send_then(command, show_lines_reply);
            }
        } else {
            match action {
                "remove-services" => c.send_then(Command::RemoveServices, |ui, _| {
                    message(ui, "Службы удалены.")
                }),
                "clear-discord" => c.send_then(Command::ClearDiscordCache, show_lines_reply),
                "apply-hosts" => c.send_then(Command::ApplyHostsUpdate, |ui, _| {
                    message(ui, "Файл hosts обновлён, резервная копия сохранена.")
                }),
                "restore-hosts" => c.send_then(Command::RestoreHostsBackup, |ui, _| {
                    message(ui, "Файл hosts восстановлен из резервной копии.")
                }),
                other => tracing::warn!("unknown confirmed action {other:?}"),
            }
        }
    });
    commands.on_quit(quit);
}
