//! "Copy diagnostics report": plain text, no user list contents (D27, T9).

use std::fmt::Write as _;

use crate::api::{
    CheckStatus, DpiStatus, GameFilterMode, IpsetMode, ProbeOutcome, RunState, StrategyResult,
};
use crate::engine::Engine;
use crate::upstream::bundle;

const LOG_LINES: usize = 150;

fn outcome(outcome: ProbeOutcome) -> &'static str {
    match outcome {
        ProbeOutcome::Ok => "OK",
        ProbeOutcome::Ssl => "SSL",
        ProbeOutcome::Unsupported => "UNSUP",
        ProbeOutcome::Error => "ERROR",
    }
}

fn dpi_status(status: DpiStatus) -> &'static str {
    match status {
        DpiStatus::Ok => "OK",
        DpiStatus::LikelyBlocked => "LIKELY_BLOCKED",
        DpiStatus::Fail => "FAIL",
        DpiStatus::Unsupported => "UNSUPPORTED",
    }
}

/// One line per target, upstream-like (`HTTP:OK TLS1.2:OK TLS1.3:UNSUP ping 23 ms`).
pub fn result_lines(result: &StrategyResult) -> Vec<String> {
    let mut lines = Vec::new();
    if result.winws_failed {
        lines.push("winws завершился во время проверки".into());
    }
    for target in &result.targets {
        let http = target
            .http
            .map(|outcomes| {
                format!(
                    "HTTP:{} TLS1.2:{} TLS1.3:{} ",
                    outcome(outcomes[0]),
                    outcome(outcomes[1]),
                    outcome(outcomes[2])
                )
            })
            .unwrap_or_default();
        let ping = target
            .ping_ms
            .map(|ms| format!("ping {ms} мс"))
            .unwrap_or_else(|| "ping —".into());
        let marker = if target.critical { "*" } else { " " };
        lines.push(format!("{marker} {}: {http}{ping}", target.name));
    }
    for dpi in &result.dpi {
        lines.push(format!(
            "  {} {} ({}): HTTP:{} TLS1.2:{} TLS1.3:{}",
            dpi.id,
            dpi.provider,
            dpi.host,
            dpi_status(dpi.statuses[0]),
            dpi_status(dpi.statuses[1]),
            dpi_status(dpi.statuses[2])
        ));
    }
    lines
}

pub fn build(engine: &Engine) -> String {
    let state = engine.snapshot();
    let mut text = String::new();
    let _ = writeln!(text, "zapret-gui {} — отчёт диагностики", state.app_version);
    let _ = writeln!(
        text,
        "Платформа: {}{}",
        std::env::consts::OS,
        if engine.is_mock {
            " (имитация)"
        } else {
            ""
        }
    );
    let _ = writeln!(
        text,
        "Права администратора: {}",
        if state.elevated { "да" } else { "нет" }
    );
    let version = state
        .current_version
        .clone()
        .unwrap_or_else(|| "не скачана".into());
    let sha = state
        .current_version
        .as_ref()
        .and_then(|version| bundle::read_meta(&engine.paths.bundle_dir(version)))
        .map(|meta| meta.sha256)
        .unwrap_or_default();
    let _ = writeln!(
        text,
        "Версия zapret: {version}{} {}",
        if state.pinned {
            " (закреплена)"
        } else {
            ""
        },
        if sha.is_empty() {
            String::new()
        } else {
            format!("SHA256 {sha}")
        }
    );
    let run_state = match state.run_state {
        RunState::Stopped => "выключен",
        RunState::Starting => "запускается",
        RunState::Running => "работает",
        RunState::Error => "ошибка",
    };
    let _ = writeln!(
        text,
        "Обход: {run_state}, стратегия: {}",
        state.active_strategy.as_deref().unwrap_or("не выбрана")
    );
    if let Some(error) = &state.error {
        let _ = writeln!(text, "Ошибка: {}", error.message);
        for line in &error.output_tail {
            let _ = writeln!(text, "  winws: {line}");
        }
    }
    let game = match state.game_filter {
        GameFilterMode::Off => "выключен",
        GameFilterMode::All => "TCP+UDP",
        GameFilterMode::Tcp => "TCP",
        GameFilterMode::Udp => "UDP",
    };
    let _ = writeln!(
        text,
        "Игровой фильтр: {game} (TCP {}, UDP {})",
        state.game_tcp_ports, state.game_udp_ports
    );
    let ipset = match state.ipset_mode {
        IpsetMode::None => "none",
        IpsetMode::Loaded => "loaded",
        IpsetMode::Any => "any",
    };
    let _ = writeln!(
        text,
        "IPSet: {ipset}{}",
        if state.ipset_list_updated {
            " (список обновлён)"
        } else {
            ""
        }
    );
    let lists = &state.user_lists;
    let _ = writeln!(
        text,
        "Свои списки (только количество): домены {}, исключения {}, IP-исключения {}",
        lists.general.len(),
        lists.exclude.len(),
        lists.ipset_exclude.len()
    );
    if let Some(fakes) = &state.fakes {
        let _ = writeln!(
            text,
            "Фейки: Discord UDP {}, игры UDP {}",
            fakes.discord_udp.as_deref().unwrap_or("по умолчанию"),
            fakes.game_udp.as_deref().unwrap_or("по умолчанию")
        );
    }
    let _ = writeln!(
        text,
        "Сторож: {}, последняя проверка: {}",
        if state.settings.watchdog_enabled {
            "включён"
        } else {
            "выключен"
        },
        match state.watchdog.last_ok {
            Some(true) => "всё работает",
            Some(false) => "сбой",
            None => "не было",
        }
    );

    let _ = writeln!(text, "\n== Проверки ==");
    if state.checks.is_empty() {
        let _ = writeln!(text, "(не запускались)");
    }
    for check in &state.checks {
        let status = match check.status {
            CheckStatus::Ok => "OK",
            CheckStatus::Warning => "ВНИМАНИЕ",
            CheckStatus::Problem => "ПРОБЛЕМА",
        };
        let _ = writeln!(
            text,
            "[{status}] {}. {}: {}",
            check.number, check.title, check.detail
        );
    }

    let _ = writeln!(text, "\n== Последний подбор ==");
    match &state.last_scan {
        Some(scan) => {
            let _ = writeln!(
                text,
                "{:?}, победитель: {}, отменён: {}",
                scan.mode,
                scan.winner.as_deref().unwrap_or("нет"),
                scan.cancelled
            );
            for name in &scan.ranking {
                if let Some(result) = state
                    .strategies
                    .iter()
                    .find(|strategy| &strategy.name == name)
                    .and_then(|strategy| strategy.result.as_ref())
                {
                    let _ = writeln!(
                        text,
                        "{name}: {} критичных {}/{}, OK {}",
                        if result.passed {
                            "работает"
                        } else {
                            "не работает"
                        },
                        result.critical_ok,
                        result.critical_total,
                        result.ok_count
                    );
                }
            }
        }
        None => {
            let _ = writeln!(text, "(не запускался)");
        }
    }
    if let Some(test) = &state.last_test {
        let _ = writeln!(text, "\n== Последняя проверка ({:?}) ==", test.mode);
        for entry in &test.entries {
            let _ = writeln!(text, "{}:", entry.strategy);
            for line in result_lines(&entry.result) {
                let _ = writeln!(text, "  {line}");
            }
        }
    }

    let _ = writeln!(text, "\n== Журнал (последние {LOG_LINES} строк) ==");
    for line in engine.log.tail(LOG_LINES) {
        let _ = writeln!(text, "{line}");
    }
    text
}
