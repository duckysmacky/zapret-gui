//! Value types of the core API: commands in, state and events out. All serde, so the API can move behind IPC.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Stopped,
    Starting,
    Running,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameFilterMode {
    #[default]
    Off,
    All,
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpsetMode {
    None,
    #[default]
    Loaded,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanMode {
    Quick,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestMode {
    Standard,
    Dpi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserListKind {
    /// `list-general-user.txt`: domains to bypass.
    General,
    /// `list-exclude-user.txt`: domains to exclude.
    Exclude,
    /// `ipset-exclude-user.txt`: IPs/subnets to exclude.
    IpsetExclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FakeSlot {
    DiscordUdp,
    GameUdp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixKind {
    EnableTcpTimestamps,
    RemoveWinDivert,
    RemoveConflictingServices,
    ClearDiscordCache,
    RedownloadBundle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warning,
    Problem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Download,
    Scan,
    Test,
    Diagnostics,
    Hosts,
    Watchdog,
}

/// User-editable settings (the Settings page).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub autostart: bool,
    pub start_on_launch: bool,
    pub watchdog_enabled: bool,
    pub watchdog_period_minutes: u32,
    pub auto_rescan: bool,
    pub auto_check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            autostart: false,
            start_on_launch: true,
            watchdog_enabled: false,
            watchdog_period_minutes: 10,
            auto_rescan: false,
            auto_check_updates: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserLists {
    pub general: Vec<String>,
    pub exclude: Vec<String>,
    pub ipset_exclude: Vec<String>,
}

impl UserLists {
    pub fn get(&self, kind: UserListKind) -> &Vec<String> {
        match kind {
            UserListKind::General => &self.general,
            UserListKind::Exclude => &self.exclude,
            UserListKind::IpsetExclude => &self.ipset_exclude,
        }
    }

    pub fn get_mut(&mut self, kind: UserListKind) -> &mut Vec<String> {
        match kind {
            UserListKind::General => &mut self.general,
            UserListKind::Exclude => &mut self.exclude,
            UserListKind::IpsetExclude => &mut self.ipset_exclude,
        }
    }
}

/// Result of one HTTP variant probe (upstream's OK / SSL / UNSUP / ERROR).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeOutcome {
    Ok,
    Ssl,
    Unsupported,
    Error,
}

/// Result of one DPI-suite variant probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DpiStatus {
    Ok,
    LikelyBlocked,
    Fail,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetResult {
    pub name: String,
    pub value: String,
    pub critical: bool,
    /// HTTP/1.1, TLS 1.2, TLS 1.3. `None` for ping-only targets.
    pub http: Option<[ProbeOutcome; 3]>,
    /// Ping latency; `None` = timeout or not pinged.
    pub ping_ms: Option<u32>,
}

impl TargetResult {
    /// A target works when no variant failed outright and at least one variant (or the ping) succeeded.
    pub fn works(&self) -> bool {
        match &self.http {
            Some(outcomes) => {
                outcomes
                    .iter()
                    .all(|outcome| matches!(outcome, ProbeOutcome::Ok | ProbeOutcome::Unsupported))
                    && outcomes.contains(&ProbeOutcome::Ok)
            }
            None => self.ping_ms.is_some(),
        }
    }

    pub fn ok_count(&self) -> u32 {
        let http_ok = self
            .http
            .map(|outcomes| {
                outcomes
                    .iter()
                    .filter(|outcome| **outcome == ProbeOutcome::Ok)
                    .count() as u32
            })
            .unwrap_or(0);
        http_ok + u32::from(self.ping_ms.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DpiResult {
    pub id: String,
    pub provider: String,
    pub host: String,
    /// HTTP/1.1, TLS 1.2, TLS 1.3.
    pub statuses: [DpiStatus; 3],
}

/// Last test result of one strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyResult {
    pub bundle_version: String,
    /// Unix seconds.
    pub tested_at: u64,
    pub passed: bool,
    pub critical_ok: u32,
    pub critical_total: u32,
    pub ok_count: u32,
    pub avg_ping_ms: Option<u32>,
    /// winws exited during the test.
    pub winws_failed: bool,
    pub targets: Vec<TargetResult>,
    #[serde(default)]
    pub dpi: Vec<DpiResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyInfo {
    pub name: String,
    pub parse_error: Option<String>,
    pub result: Option<StrategyResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub tag: String,
    pub downloaded: bool,
    pub current: bool,
    pub latest: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub kind: JobKind,
    pub message: String,
    /// 0.0..=1.0, `None` when unknown.
    pub progress: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub message: String,
    pub output_tail: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    /// Upstream order, 1..=17.
    pub number: u8,
    pub title: String,
    pub status: CheckStatus,
    pub detail: String,
    pub fix: Option<FixKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanSummary {
    pub mode: ScanMode,
    /// Strategy names, best first.
    pub ranking: Vec<String>,
    pub winner: Option<String>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestEntry {
    pub strategy: String,
    pub result: StrategyResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestReport {
    pub mode: TestMode,
    pub entries: Vec<TestEntry>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchdogStatus {
    pub last_check: Option<u64>,
    pub last_ok: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fakes {
    /// Fake payload names (file names without `.bin`) of the current bundle.
    pub available: Vec<String>,
    pub discord_udp: Option<String>,
    pub game_udp: Option<String>,
}

/// Full snapshot of everything the UI shows. The single source of truth for the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    pub app_version: String,
    pub elevated: bool,
    pub run_state: RunState,
    pub active_strategy: Option<String>,
    pub error: Option<ErrorInfo>,
    pub game_filter: GameFilterMode,
    pub game_tcp_ports: String,
    pub game_udp_ports: String,
    pub ipset_mode: IpsetMode,
    pub ipset_list_updated: bool,
    pub current_version: Option<String>,
    pub pinned: bool,
    pub strategies: Vec<StrategyInfo>,
    pub versions: Vec<VersionInfo>,
    pub update_available: Option<String>,
    pub job: Option<Job>,
    pub last_scan: Option<ScanSummary>,
    pub last_test: Option<TestReport>,
    pub watchdog: WatchdogStatus,
    pub settings: Settings,
    pub fakes: Option<Fakes>,
    pub user_lists: UserLists,
    pub upstream_list_files: Vec<String>,
    pub checks: Vec<CheckResult>,
    pub hosts_backup_available: bool,
    pub first_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    GetSnapshot,
    RestartAsAdmin,
    Enable,
    Disable,
    SetActiveStrategy {
        strategy_name: String,
    },
    SetGameFilter {
        game_filter_mode: GameFilterMode,
    },
    /// Tray toggle: on restores the last non-off mode (default TCP+UDP), off sets off.
    ToggleGameFilter,
    SetGameFilterPorts {
        tcp_ports: String,
        udp_ports: String,
    },
    SetIpsetMode {
        ipset_mode: IpsetMode,
    },
    UpdateIpsetList,
    RunScan {
        scan_mode: ScanMode,
    },
    /// Empty `strategy_names` = all strategies.
    TestStrategies {
        strategy_names: Vec<String>,
        test_mode: TestMode,
    },
    /// Cancels the running scan, test or download.
    CancelJob,
    CheckForUpdates,
    SelectUpstreamVersion {
        version_tag: String,
    },
    DeleteUpstreamVersion {
        version_tag: String,
    },
    /// Replaces the whole list. Reply: `Rejected` with the entries that failed validation.
    UpdateUserList {
        list_kind: UserListKind,
        entries: Vec<String>,
    },
    /// Reply: `Text` with the contents of an upstream list of the current bundle.
    GetUpstreamList {
        file_name: String,
    },
    RunDiagnostics,
    ApplyFix {
        fix: FixKind,
    },
    RemoveServices,
    ClearDiscordCache,
    /// Reply: `Lines` with the upstream hosts lines missing from the system hosts file.
    PrepareHostsUpdate,
    ApplyHostsUpdate,
    RestoreHostsBackup,
    /// Reply: `Lines`.
    GetLogTail,
    /// Reply: `Text`.
    GetDiagnosticsReport,
    UpdateSettings {
        settings: Settings,
    },
    SetFake {
        fake_slot: FakeSlot,
        fake_name: Option<String>,
    },
    FinishFirstRun,
    /// Stops jobs and the watchdog and kills winws (Quit).
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Ok,
    Snapshot(Box<AppState>),
    Text(String),
    Lines(Vec<String>),
    Rejected(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CoreEvent {
    StateChanged(Box<AppState>),
    LogLine(String),
    Notification(Notification),
}

/// Errors returned to the UI. Messages are user-facing (Russian).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
pub enum CoreError {
    #[error("Нужны права администратора. Нажмите «Перезапустить от имени администратора».")]
    NotElevated,
    #[error("Сейчас выполняется другая операция. Дождитесь её окончания или отмените её.")]
    Busy,
    #[error("Версия zapret ещё не скачана.")]
    NoBundle,
    #[error("Стратегия не выбрана.")]
    NoStrategy,
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Conflict(String),
    #[error("Ошибка сети: {0}")]
    Network(String),
    #[error("Ошибка: {0}")]
    Failed(String),
    #[error("Операция отменена.")]
    Cancelled,
}
