//! Core state → Slint globals. The mapping is plain functions (tested); `apply` pushes them into the window.

use std::cell::RefCell;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use zapret_core::report::result_lines;
use zapret_core::{
    AppState as CoreState, CheckStatus, FixKind, GameFilterMode, IpsetMode, JobKind, RunState,
    ScanSummary, TestMode, Theme,
};

use crate::{AppState, CheckRow, MainWindow, StrategyRow};

const LOG_LINES: usize = 400;

thread_local! {
    /// The last state applied to the window (UI thread only).
    static LAST: RefCell<Option<CoreState>> = const { RefCell::new(None) };
}

pub fn last_state() -> Option<CoreState> {
    LAST.with(|last| last.borrow().clone())
}

pub fn ago(unix_seconds: u64, now: u64) -> String {
    let seconds = now.saturating_sub(unix_seconds);
    match seconds {
        0..=59 => "только что".into(),
        60..=3599 => format!("{} мин назад", seconds / 60),
        3600..=86_399 => format!("{} ч назад", seconds / 3600),
        _ => format!("{} дн назад", seconds / 86_400),
    }
}

pub fn status_text(state: &CoreState) -> String {
    let strategy = state.active_strategy.as_deref().unwrap_or("");
    match state.run_state {
        RunState::Running => format!("Обход включён: «{strategy}»"),
        RunState::Starting => "Запуск…".into(),
        RunState::Error => "Ошибка: обход не работает".into(),
        RunState::Stopped if state.current_version.is_none() => "zapret ещё не скачан".into(),
        RunState::Stopped if strategy.is_empty() => {
            "Стратегия не выбрана: запустите автоподбор".into()
        }
        RunState::Stopped => "Обход выключен".into(),
    }
}

pub fn strategy_rows(state: &CoreState, now: u64) -> Vec<StrategyRow> {
    let mut strategies = state.strategies.clone();
    if let Some(scan) = &state.last_scan {
        let position = |name: &str| {
            scan.ranking
                .iter()
                .position(|ranked| ranked == name)
                .unwrap_or(usize::MAX)
        };
        strategies.sort_by_key(|strategy| position(&strategy.name));
    }
    strategies
        .iter()
        .map(|strategy| {
            let (status, details) = match (&strategy.parse_error, &strategy.result) {
                (Some(error), _) => (format!("Не разобрана: {error}"), String::new()),
                (None, None) => ("Не проверялась".to_string(), String::new()),
                (None, Some(result)) => {
                    let when = ago(result.tested_at, now);
                    let status = if result.winws_failed {
                        format!("winws не запустился ({when})")
                    } else if result.passed {
                        format!("Работает ({when})")
                    } else {
                        format!(
                            "Не работает: открылось {} из {} важных сайтов ({when})",
                            result.critical_ok, result.critical_total
                        )
                    };
                    let failing: Vec<&str> = result
                        .targets
                        .iter()
                        .filter(|target| !target.works())
                        .map(|target| target.name.as_str())
                        .collect();
                    let mut details = Vec::new();
                    if !failing.is_empty() {
                        details.push(format!("Не открылись: {}", failing.join(", ")));
                    }
                    if let Some(ping) = result.avg_ping_ms {
                        details.push(format!("пинг ~{ping} мс"));
                    }
                    (status, details.join("; "))
                }
            };
            StrategyRow {
                name: strategy.name.clone().into(),
                active: state.active_strategy.as_ref() == Some(&strategy.name),
                usable: strategy.parse_error.is_none(),
                status: status.into(),
                details: details.into(),
            }
        })
        .collect()
}

pub const FIXES: [FixKind; 5] = [
    FixKind::EnableTcpTimestamps,
    FixKind::RemoveWinDivert,
    FixKind::RemoveConflictingServices,
    FixKind::ClearDiscordCache,
    FixKind::RedownloadBundle,
];

fn fix_texts(fix: FixKind) -> (&'static str, &'static str) {
    match fix {
        FixKind::EnableTcpTimestamps => ("Включить", ""),
        FixKind::RemoveWinDivert => (
            "Удалить WinDivert",
            "Служба WinDivert будет остановлена и удалена (при неудаче — также GoodbyeDPI). Продолжить?",
        ),
        FixKind::RemoveConflictingServices => (
            "Удалить службы",
            "Конфликтующие службы, а также WinDivert и WinDivert14 будут остановлены и удалены. Продолжить?",
        ),
        FixKind::ClearDiscordCache => (
            "Очистить кэш",
            "Discord будет закрыт, его кэш (Cache, Code Cache, GPUCache) удалён. Продолжить?",
        ),
        FixKind::RedownloadBundle => ("Скачать заново", ""),
    }
}

pub fn check_rows(state: &CoreState) -> Vec<CheckRow> {
    state
        .checks
        .iter()
        .map(|check| {
            let (fix_label, fix_confirm) = check.fix.map(fix_texts).unwrap_or(("", ""));
            CheckRow {
                title: format!("{}. {}", check.number, check.title).into(),
                status: match check.status {
                    CheckStatus::Ok => 0,
                    CheckStatus::Warning => 1,
                    CheckStatus::Problem => 2,
                },
                detail: check.detail.clone().into(),
                fix_label: fix_label.into(),
                fix_id: check
                    .fix
                    .and_then(|fix| FIXES.iter().position(|known| *known == fix))
                    .map_or(-1, |index| index as i32),
                fix_confirm: fix_confirm.into(),
            }
        })
        .collect()
}

/// (labels, tags, deletable, index of the current version).
pub fn versions(state: &CoreState) -> (Vec<String>, Vec<String>, Vec<bool>, i32) {
    let labels = state
        .versions
        .iter()
        .map(|version| {
            let mut notes = Vec::new();
            if version.latest {
                notes.push("последняя");
            }
            if version.downloaded {
                notes.push("скачана");
            }
            if version.current {
                notes.push("используется");
            }
            if notes.is_empty() {
                version.tag.clone()
            } else {
                format!("{} ({})", version.tag, notes.join(", "))
            }
        })
        .collect();
    let tags = state
        .versions
        .iter()
        .map(|version| version.tag.clone())
        .collect();
    let deletable = state
        .versions
        .iter()
        .map(|version| version.downloaded && !version.current)
        .collect();
    let index = state
        .versions
        .iter()
        .position(|version| version.current)
        .unwrap_or(0) as i32;
    (labels, tags, deletable, index)
}

pub fn scan_summary(scan: Option<&ScanSummary>) -> String {
    let Some(scan) = scan else {
        return String::new();
    };
    if scan.cancelled {
        return "Подбор отменён, прежнее состояние восстановлено.".into();
    }
    match &scan.winner {
        Some(winner) => {
            let mut text = format!("Подобрана стратегия «{winner}».");
            if scan.ranking.len() > 1 {
                let top: Vec<&str> = scan.ranking.iter().take(3).map(String::as_str).collect();
                text.push_str(&format!(" Проверено {}, лучшие: {}.", scan.ranking.len(), top.join(", ")));
            }
            text
        }
        None => "Ни одна стратегия не помогла. Запустите диагностику, настройте защищённый DNS или обновите zapret."
            .into(),
    }
}

pub fn test_report(state: &CoreState) -> String {
    let Some(report) = &state.last_test else {
        return String::new();
    };
    let mut lines = vec![format!(
        "{} тест{}",
        match report.mode {
            TestMode::Standard => "Стандартный",
            TestMode::Dpi => "DPI",
        },
        if report.cancelled {
            " (отменён)"
        } else {
            ""
        }
    )];
    for entry in &report.entries {
        let verdict = match report.mode {
            TestMode::Standard if entry.result.passed => "работает",
            TestMode::Standard => "не работает",
            TestMode::Dpi => "",
        };
        lines.push(format!("\n{} {verdict}", entry.strategy));
        lines.extend(
            result_lines(&entry.result)
                .into_iter()
                .map(|line| format!("  {line}")),
        );
    }
    lines.join("\n")
}

pub fn watchdog_text(state: &CoreState, now: u64) -> String {
    if !state.settings.watchdog_enabled {
        return String::new();
    }
    match (state.watchdog.last_check, state.watchdog.last_ok) {
        (Some(when), Some(ok)) => format!(
            "Сторож: проверка {} — {}",
            ago(when, now),
            if ok {
                "всё работает"
            } else {
                "обход не работает"
            }
        ),
        _ => "Сторож включён, проверок ещё не было".into(),
    }
}

/// 0 off, 1 no checks yet, 2 last check ok, 3 last check failed.
pub fn watchdog_state(state: &CoreState) -> i32 {
    if !state.settings.watchdog_enabled {
        return 0;
    }
    match state.watchdog.last_ok {
        None => 1,
        Some(true) => 2,
        Some(false) => 3,
    }
}

/// Average ping from the active strategy's last test, e.g. "~45 мс".
pub fn active_ping(state: &CoreState) -> String {
    state
        .active_strategy
        .as_ref()
        .and_then(|active| state.strategies.iter().find(|s| &s.name == active))
        .and_then(|strategy| strategy.result.as_ref())
        .and_then(|result| result.avg_ping_ms)
        .map(|ping| format!("~{ping} мс"))
        .unwrap_or_default()
}

pub fn game_filter_index(mode: GameFilterMode) -> i32 {
    match mode {
        GameFilterMode::Off => 0,
        GameFilterMode::All => 1,
        GameFilterMode::Tcp => 2,
        GameFilterMode::Udp => 3,
    }
}

pub fn game_filter_from_index(index: i32) -> GameFilterMode {
    match index {
        1 => GameFilterMode::All,
        2 => GameFilterMode::Tcp,
        3 => GameFilterMode::Udp,
        _ => GameFilterMode::Off,
    }
}

pub fn ipset_index(mode: IpsetMode) -> i32 {
    match mode {
        IpsetMode::None => 0,
        IpsetMode::Loaded => 1,
        IpsetMode::Any => 2,
    }
}

pub fn ipset_from_index(index: i32) -> IpsetMode {
    match index {
        0 => IpsetMode::None,
        2 => IpsetMode::Any,
        _ => IpsetMode::Loaded,
    }
}

pub fn theme_index(theme: Theme) -> i32 {
    match theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    }
}

fn strings(values: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}

/// Pushes the state into the window. Editable fields are only overwritten when the core value changed,
/// so typing isn't clobbered by unrelated updates.
pub fn apply(ui: &MainWindow, state: &CoreState) {
    let now = zapret_core::paths::unix_now();
    let previous = last_state();
    let changed = |pick: &dyn Fn(&CoreState) -> String| {
        previous.as_ref().is_none_or(|old| pick(old) != pick(state))
    };
    let global = ui.global::<AppState>();

    global.set_app_version(state.app_version.clone().into());
    global.set_elevated(state.elevated);
    global.set_run_state(match state.run_state {
        RunState::Stopped => 0,
        RunState::Starting => 1,
        RunState::Running => 2,
        RunState::Error => 3,
    });
    global.set_status_text(status_text(state).into());
    global.set_active_strategy(state.active_strategy.clone().unwrap_or_default().into());
    global.set_active_ping(active_ping(state).into());
    global.set_pinned(state.pinned);
    global.set_error_text(
        state
            .error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_default()
            .into(),
    );
    global.set_error_tail(
        state
            .error
            .as_ref()
            .map(|error| error.output_tail.join("\n"))
            .unwrap_or_default()
            .into(),
    );
    global.set_has_bundle(state.current_version.is_some() && !state.strategies.is_empty());
    global.set_current_version(state.current_version.clone().unwrap_or_default().into());
    global.set_update_available(state.update_available.clone().unwrap_or_default().into());

    let names: Vec<String> = state
        .strategies
        .iter()
        .filter(|strategy| strategy.parse_error.is_none())
        .map(|strategy| strategy.name.clone())
        .collect();
    let active_index = state
        .active_strategy
        .as_ref()
        .and_then(|active| names.iter().position(|name| name == active))
        .map_or(-1, |index| index as i32);
    if changed(&|s| {
        format!(
            "{:?}",
            s.strategies.iter().map(|x| &x.name).collect::<Vec<_>>()
        )
    }) {
        global.set_strategy_names(strings(names));
    }
    global.set_active_index(active_index);
    global.set_strategies(ModelRc::new(VecModel::from(strategy_rows(state, now))));
    global.set_scan_summary(scan_summary(state.last_scan.as_ref()).into());
    global.set_scan_done(state.last_scan.as_ref().is_some_and(|scan| !scan.cancelled));

    global.set_game_filter(game_filter_index(state.game_filter));
    if changed(&|s| s.game_tcp_ports.clone() + "|" + &s.game_udp_ports) {
        global.set_game_tcp_ports(state.game_tcp_ports.clone().into());
        global.set_game_udp_ports(state.game_udp_ports.clone().into());
    }
    global.set_ipset_mode(ipset_index(state.ipset_mode));
    global.set_ipset_list_updated(state.ipset_list_updated);
    if changed(&|s| s.user_lists.general.join("\n")) {
        global.set_list_general(state.user_lists.general.join("\n").into());
    }
    if changed(&|s| s.user_lists.exclude.join("\n")) {
        global.set_list_exclude(state.user_lists.exclude.join("\n").into());
    }
    if changed(&|s| s.user_lists.ipset_exclude.join("\n")) {
        global.set_list_ipset_exclude(state.user_lists.ipset_exclude.join("\n").into());
    }
    if changed(&|s| s.upstream_list_files.join("|")) {
        global.set_upstream_lists(strings(state.upstream_list_files.clone()));
    }

    if changed(&|s| format!("{:?}", s.versions)) {
        let (labels, tags, deletable, index) = versions(state);
        global.set_version_labels(strings(labels));
        global.set_version_tags(strings(tags));
        global.set_version_deletable(ModelRc::new(VecModel::from(deletable)));
        global.set_version_index(index);
    }

    global.set_job_active(state.job.is_some());
    global.set_job_cancellable(
        state.job.as_ref().is_some_and(|job| {
            matches!(job.kind, JobKind::Scan | JobKind::Test | JobKind::Download)
        }),
    );
    global.set_job_text(
        state
            .job
            .as_ref()
            .map(|job| job.message.clone())
            .unwrap_or_default()
            .into(),
    );
    global.set_job_progress(
        state
            .job
            .as_ref()
            .and_then(|job| job.progress)
            .unwrap_or(-1.0),
    );
    global.set_watchdog_text(watchdog_text(state, now).into());
    global.set_watchdog_state(watchdog_state(state));

    let settings = &state.settings;
    global.set_theme(theme_index(settings.theme));
    global.set_autostart(settings.autostart);
    global.set_start_on_launch(settings.start_on_launch);
    global.set_watchdog_enabled(settings.watchdog_enabled);
    global.set_watchdog_period(settings.watchdog_period_minutes as i32);
    global.set_auto_rescan(settings.auto_rescan);
    global.set_auto_check(settings.auto_check_updates);

    global.set_fakes_supported(state.fakes.is_some());
    if let Some(fakes) = &state.fakes {
        let mut names = vec!["По умолчанию (из zapret)".to_string()];
        names.extend(fakes.available.iter().cloned());
        let index_of = |chosen: &Option<String>| {
            chosen
                .as_ref()
                .and_then(|name| fakes.available.iter().position(|fake| fake == name))
                .map_or(0, |index| index as i32 + 1)
        };
        if changed(&|s| format!("{:?}", s.fakes.as_ref().map(|f| &f.available))) {
            global.set_fake_names(strings(names));
        }
        global.set_fake_discord_index(index_of(&fakes.discord_udp));
        global.set_fake_game_index(index_of(&fakes.game_udp));
    }

    global.set_checks(ModelRc::new(VecModel::from(check_rows(state))));
    global.set_test_report(test_report(state).into());
    global.set_hosts_backup_available(state.hosts_backup_available);
    global.set_first_run(state.first_run);

    crate::tray::update(state);
    LAST.with(|last| *last.borrow_mut() = Some(state.clone()));
}

pub fn set_logs(ui: &MainWindow, lines: &[String]) {
    let start = lines.len().saturating_sub(LOG_LINES);
    ui.global::<AppState>()
        .set_logs(lines[start..].join("\n").into());
}

pub fn append_log(ui: &MainWindow, line: &str) {
    let global = ui.global::<AppState>();
    let current = global.get_logs();
    let mut lines: Vec<&str> = if current.is_empty() {
        Vec::new()
    } else {
        current.split('\n').collect()
    };
    lines.push(line);
    let start = lines.len().saturating_sub(LOG_LINES);
    global.set_logs(lines[start..].join("\n").into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use zapret_core::*;

    fn state() -> CoreState {
        CoreState {
            app_version: "0.1.0".into(),
            elevated: true,
            run_state: RunState::Stopped,
            active_strategy: Some("b".into()),
            error: None,
            game_filter: GameFilterMode::Off,
            game_tcp_ports: "1024-65535".into(),
            game_udp_ports: "1024-65535".into(),
            ipset_mode: IpsetMode::Loaded,
            ipset_list_updated: false,
            current_version: Some("1.0".into()),
            pinned: false,
            strategies: vec![
                StrategyInfo {
                    name: "a".into(),
                    parse_error: None,
                    result: None,
                },
                StrategyInfo {
                    name: "b".into(),
                    parse_error: None,
                    result: Some(StrategyResult {
                        bundle_version: "1.0".into(),
                        tested_at: 100,
                        passed: false,
                        critical_ok: 1,
                        critical_total: 2,
                        ok_count: 3,
                        avg_ping_ms: Some(20),
                        winws_failed: false,
                        targets: vec![TargetResult {
                            name: "Discord".into(),
                            value: "https://discord.com".into(),
                            critical: true,
                            http: Some([ProbeOutcome::Error; 3]),
                            ping_ms: None,
                        }],
                        dpi: vec![],
                    }),
                },
                StrategyInfo {
                    name: "c".into(),
                    parse_error: Some("плохо".into()),
                    result: None,
                },
            ],
            versions: vec![
                VersionInfo {
                    tag: "2.0".into(),
                    downloaded: false,
                    current: false,
                    latest: true,
                },
                VersionInfo {
                    tag: "1.0".into(),
                    downloaded: true,
                    current: true,
                    latest: false,
                },
            ],
            update_available: None,
            job: None,
            last_scan: None,
            last_test: None,
            watchdog: WatchdogStatus::default(),
            settings: Settings::default(),
            fakes: None,
            user_lists: UserLists::default(),
            upstream_list_files: vec![],
            checks: vec![],
            hosts_backup_available: false,
            first_run: false,
        }
    }

    #[test]
    fn rows_and_texts() {
        let state = state();
        assert_eq!(status_text(&state), "Обход выключен");
        let rows = strategy_rows(&state, 100 + 120);
        assert_eq!(rows.len(), 3);
        assert!(rows[1].active);
        assert_eq!(
            rows[1].status.as_str(),
            "Не работает: открылось 1 из 2 важных сайтов (2 мин назад)"
        );
        assert_eq!(
            rows[1].details.as_str(),
            "Не открылись: Discord; пинг ~20 мс"
        );
        assert!(!rows[2].usable);
        assert_eq!(active_ping(&state), "~20 мс");

        let (labels, tags, deletable, index) = versions(&state);
        assert_eq!(
            labels,
            vec!["2.0 (последняя)", "1.0 (скачана, используется)"]
        );
        assert_eq!(tags, vec!["2.0", "1.0"]);
        assert_eq!(deletable, vec![false, false]);
        assert_eq!(index, 1);
    }

    #[test]
    fn ranking_order_and_summary() {
        let mut state = state();
        let scan = ScanSummary {
            mode: ScanMode::Full,
            ranking: vec!["c".into(), "b".into()],
            winner: Some("b".into()),
            cancelled: false,
        };
        assert!(scan_summary(Some(&scan)).starts_with("Подобрана стратегия «b»."));
        state.last_scan = Some(scan);
        let names: Vec<String> = strategy_rows(&state, 0)
            .iter()
            .map(|row| row.name.to_string())
            .collect();
        assert_eq!(names, vec!["c", "b", "a"]);
    }

    #[test]
    fn index_mappings_round_trip() {
        for mode in [
            GameFilterMode::Off,
            GameFilterMode::All,
            GameFilterMode::Tcp,
            GameFilterMode::Udp,
        ] {
            assert_eq!(game_filter_from_index(game_filter_index(mode)), mode);
        }
        for mode in [IpsetMode::None, IpsetMode::Loaded, IpsetMode::Any] {
            assert_eq!(ipset_from_index(ipset_index(mode)), mode);
        }
        assert_eq!(ago(0, 30), "только что");
        assert_eq!(ago(0, 7200), "2 ч назад");
    }

    #[test]
    fn watchdog_states() {
        let mut state = state();
        assert_eq!(watchdog_state(&state), 0);
        state.settings.watchdog_enabled = true;
        assert_eq!(watchdog_state(&state), 1);
        state.watchdog.last_ok = Some(true);
        assert_eq!(watchdog_state(&state), 2);
        state.watchdog.last_ok = Some(false);
        assert_eq!(watchdog_state(&state), 3);
    }
}
