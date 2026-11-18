//! The core: owns config + runtime state, handles commands, runs winws and the long jobs.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::{Notify, broadcast, mpsc};

use crate::api::*;
use crate::config::{Config, result_key};
use crate::logging::LogBuffer;
use crate::paths::Paths;
use crate::platform::{Platform, PlatformError, RunningWinws, System, WinwsDriver};
use crate::prober::ProbeBackend;
use crate::upstream::github::{self, Release};
use crate::upstream::parser::Strategy;
use crate::upstream::{UpstreamError, bundle};
use crate::{diagnostics, hosts, lists, runner};

const OUTPUT_TAIL_LINES: usize = 20;
const MAX_UPSTREAM_LIST_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) async fn blocking<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, CoreError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| CoreError::Failed(error.to_string()))
}

impl From<UpstreamError> for CoreError {
    fn from(error: UpstreamError) -> Self {
        match error {
            UpstreamError::Network(message) => CoreError::Network(message),
            UpstreamError::Cancelled => CoreError::Cancelled,
            other => CoreError::Failed(other.to_string()),
        }
    }
}

impl From<PlatformError> for CoreError {
    fn from(error: PlatformError) -> Self {
        CoreError::Failed(error.to_string())
    }
}

/// What the current bundle offers.
#[derive(Default)]
pub(crate) struct BundleInfo {
    pub strategies: Vec<Strategy>,
    pub fakes: Option<Vec<String>>,
    pub list_files: Vec<String>,
}

/// State that isn't persisted.
#[derive(Default)]
pub(crate) struct Runtime {
    pub elevated: bool,
    pub run_state: RunState,
    pub error: Option<ErrorInfo>,
    pub job: Option<Job>,
    pub last_scan: Option<ScanSummary>,
    pub last_test: Option<TestReport>,
    pub checks: Vec<CheckResult>,
    pub update_available: Option<String>,
    pub watchdog: WatchdogStatus,
    pub installed: Vec<String>,
    pub releases: Vec<Release>,
    pub hosts_backup_available: bool,
}

pub struct Engine {
    pub(crate) paths: Paths,
    pub(crate) system: Arc<dyn System>,
    pub(crate) driver: Arc<dyn WinwsDriver>,
    pub(crate) probe: Arc<dyn ProbeBackend>,
    pub(crate) is_mock: bool,
    fail_downloads: bool,
    http: reqwest::Client,
    events: broadcast::Sender<CoreEvent>,
    pub(crate) log: Arc<LogBuffer>,
    pub(crate) config: Mutex<Config>,
    pub(crate) runtime: Mutex<Runtime>,
    pub(crate) bundle: Mutex<BundleInfo>,
    winws: tokio::sync::Mutex<Option<RunningWinws>>,
    generation: AtomicU64,
    output_tail: Arc<Mutex<VecDeque<String>>>,
    cancel: Mutex<Arc<AtomicBool>>,
    pending_hosts: Mutex<Option<Vec<String>>>,
    pub(crate) shutting_down: AtomicBool,
    pub(crate) watchdog_kick: Notify,
}

impl Engine {
    pub fn new(
        paths: Paths,
        platform: Platform,
        log: Arc<LogBuffer>,
        events: broadcast::Sender<CoreEvent>,
    ) -> Self {
        let config = Config::load(&paths.config_file());
        Self {
            paths,
            system: platform.system,
            driver: platform.driver,
            probe: platform.probe,
            is_mock: platform.is_mock,
            fail_downloads: platform.fail_downloads,
            http: github::client(),
            events,
            log,
            config: Mutex::new(config),
            runtime: Mutex::new(Runtime::default()),
            bundle: Mutex::new(BundleInfo::default()),
            winws: tokio::sync::Mutex::new(None),
            generation: AtomicU64::new(0),
            output_tail: Arc::new(Mutex::new(VecDeque::new())),
            cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            pending_hosts: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
            watchdog_kick: Notify::new(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.events.subscribe()
    }

    // ----- state -------------------------------------------------------------------------------------------------

    pub fn snapshot(&self) -> AppState {
        let config = lock(&self.config).clone();
        let runtime = lock(&self.runtime);
        let bundle = lock(&self.bundle);
        let version = config.current_version.clone().unwrap_or_default();
        let strategies = bundle
            .strategies
            .iter()
            .map(|strategy| StrategyInfo {
                name: strategy.name.clone(),
                parse_error: strategy.parse_error.clone(),
                result: config
                    .strategy_results
                    .get(&result_key(&version, &strategy.name))
                    .cloned(),
            })
            .collect();
        let latest = runtime.releases.first().map(|release| release.tag.clone());
        let mut tags: Vec<String> = runtime
            .releases
            .iter()
            .map(|release| release.tag.clone())
            .collect();
        for tag in &runtime.installed {
            if !tags.contains(tag) {
                tags.push(tag.clone());
            }
        }
        tags.sort_by_key(|tag| std::cmp::Reverse(crate::upstream::parser::natural_key(tag)));
        let versions = tags
            .into_iter()
            .map(|tag| VersionInfo {
                downloaded: runtime.installed.contains(&tag),
                current: config.current_version.as_ref() == Some(&tag),
                latest: latest.as_ref() == Some(&tag),
                tag,
            })
            .collect();
        AppState {
            app_version: env!("CARGO_PKG_VERSION").into(),
            elevated: runtime.elevated,
            run_state: runtime.run_state,
            active_strategy: config.active_strategy.clone(),
            error: runtime.error.clone(),
            game_filter: config.game_filter,
            game_tcp_ports: config.game_tcp_ports.clone(),
            game_udp_ports: config.game_udp_ports.clone(),
            ipset_mode: config.ipset_mode,
            ipset_list_updated: self.paths.ipset_override_file().exists(),
            current_version: config.current_version.clone(),
            pinned: config.pinned,
            strategies,
            versions,
            update_available: runtime.update_available.clone(),
            job: runtime.job.clone(),
            last_scan: runtime.last_scan.clone(),
            last_test: runtime.last_test.clone(),
            watchdog: runtime.watchdog.clone(),
            settings: config.settings.clone(),
            fakes: bundle.fakes.as_ref().map(|available| Fakes {
                available: available.clone(),
                discord_udp: config.fake_discord_udp.clone(),
                game_udp: config.fake_game_udp.clone(),
            }),
            user_lists: config.user_lists.clone(),
            upstream_list_files: bundle.list_files.clone(),
            checks: runtime.checks.clone(),
            hosts_backup_available: runtime.hosts_backup_available,
            first_run: !config.first_run_done,
        }
    }

    pub(crate) fn emit(&self) {
        let _ = self
            .events
            .send(CoreEvent::StateChanged(Box::new(self.snapshot())));
    }

    pub(crate) fn notify(&self, title: &str, body: impl Into<String>) {
        let body = body.into();
        tracing::info!("notification: {title}: {body}");
        let _ = self.events.send(CoreEvent::Notification(Notification {
            title: title.into(),
            body,
        }));
    }

    /// Mutates and saves the config.
    pub(crate) fn edit_config<R>(&self, edit: impl FnOnce(&mut Config) -> R) -> R {
        let mut config = lock(&self.config);
        let result = edit(&mut config);
        if let Err(error) = config.save(&self.paths.config_file()) {
            tracing::error!("cannot save config: {error}");
        }
        result
    }

    pub(crate) fn current_bundle_dir(&self) -> Option<PathBuf> {
        lock(&self.config)
            .current_version
            .as_ref()
            .map(|version| self.paths.bundle_dir(version))
    }

    pub(crate) fn is_elevated(&self) -> bool {
        lock(&self.runtime).elevated
    }

    fn require_elevated(&self) -> Result<(), CoreError> {
        if self.is_elevated() {
            Ok(())
        } else {
            Err(CoreError::NotElevated)
        }
    }

    fn ensure_no_job(&self) -> Result<(), CoreError> {
        if lock(&self.runtime).job.is_some() {
            Err(CoreError::Busy)
        } else {
            Ok(())
        }
    }

    // ----- jobs --------------------------------------------------------------------------------------------------

    /// Marks a long job as running; fails with `Busy` if one already runs. Returns its cancel flag.
    pub(crate) fn begin_job(
        &self,
        kind: JobKind,
        message: &str,
    ) -> Result<Arc<AtomicBool>, CoreError> {
        {
            let mut runtime = lock(&self.runtime);
            if runtime.job.is_some() {
                return Err(CoreError::Busy);
            }
            runtime.job = Some(Job {
                kind,
                message: message.into(),
                progress: None,
            });
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *lock(&self.cancel) = cancel.clone();
        self.emit();
        Ok(cancel)
    }

    pub(crate) fn job_progress(&self, message: impl Into<String>, progress: Option<f32>) {
        if let Some(job) = lock(&self.runtime).job.as_mut() {
            job.message = message.into();
            job.progress = progress;
        }
        self.emit();
    }

    pub(crate) fn end_job(&self) {
        lock(&self.runtime).job = None;
        self.emit();
    }

    // ----- startup -----------------------------------------------------------------------------------------------

    pub async fn startup(self: &Arc<Self>) {
        let system = self.system.clone();
        let elevated = blocking(move || system.is_elevated())
            .await
            .unwrap_or(false);
        lock(&self.runtime).elevated = elevated;
        tracing::info!(
            "zapret-gui {} starting; elevated: {elevated}, mock: {}",
            env!("CARGO_PKG_VERSION"),
            self.is_mock
        );
        if elevated {
            let system = self.system.clone();
            let data_dir = self.paths.data_dir.clone();
            let secured = blocking(move || -> Result<(), String> {
                std::fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
                system
                    .secure_data_dir(&data_dir)
                    .map_err(|error| error.to_string())
            })
            .await;
            match secured {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::error!("cannot secure data dir: {error}"),
                Err(error) => tracing::error!("cannot secure data dir: {error}"),
            }
        }
        self.refresh_installed().await;
        {
            let installed = lock(&self.runtime).installed.clone();
            let current = lock(&self.config).current_version.clone();
            let valid = current.filter(|version| installed.contains(version));
            let fallback = valid.or_else(|| installed.first().cloned());
            if lock(&self.config).current_version != fallback {
                self.edit_config(|config| config.current_version = fallback);
            }
        }
        self.reload_bundle().await;
        lock(&self.runtime).hosts_backup_available =
            hosts::latest_backup(&self.paths.hosts_backups_dir()).is_some();
        self.emit();

        tokio::spawn(self.clone().watchdog_loop());
        tokio::spawn(self.clone().update_loop());

        let (start_on_launch, ready) = {
            let config = lock(&self.config);
            (
                config.settings.start_on_launch,
                config.first_run_done && config.active_strategy.is_some(),
            )
        };
        if start_on_launch
            && ready
            && elevated
            && let Err(error) = self.start_winws(None, None).await
        {
            tracing::warn!("start on launch failed: {error}");
        }
    }

    pub(crate) async fn refresh_installed(&self) {
        let bundles_dir = self.paths.bundles_dir();
        let installed = blocking(move || bundle::installed_versions(&bundles_dir))
            .await
            .unwrap_or_default();
        lock(&self.runtime).installed = installed;
    }

    /// Re-reads strategies, fakes and list names of the current bundle.
    pub(crate) async fn reload_bundle(&self) {
        let info = match self.current_bundle_dir() {
            Some(dir) => blocking(move || BundleInfo {
                strategies: bundle::load_strategies(&dir),
                fakes: bundle::fakes(&dir),
                list_files: bundle::list_files(&dir),
            })
            .await
            .unwrap_or_default(),
            None => BundleInfo::default(),
        };
        for strategy in &info.strategies {
            if let Some(reason) = &strategy.parse_error {
                tracing::warn!("strategy {:?} unparseable: {reason}", strategy.name);
            }
        }
        *lock(&self.bundle) = info;
    }

    // ----- commands ----------------------------------------------------------------------------------------------

    pub async fn handle(self: &Arc<Self>, command: Command) -> Result<Reply, CoreError> {
        if self.shutting_down.load(Ordering::Relaxed) && command != Command::Shutdown {
            return Err(CoreError::Cancelled);
        }
        match command {
            Command::GetSnapshot => Ok(Reply::Snapshot(Box::new(self.snapshot()))),
            Command::RestartAsAdmin => {
                let system = self.system.clone();
                blocking(move || system.relaunch_elevated()).await??;
                Ok(Reply::Ok)
            }
            Command::Enable => {
                self.ensure_no_job()?;
                self.start_winws(None, None).await?;
                Ok(Reply::Ok)
            }
            Command::Disable => {
                self.ensure_no_job()?;
                self.stop_winws().await;
                lock(&self.runtime).error = None;
                self.emit();
                Ok(Reply::Ok)
            }
            Command::SetActiveStrategy { strategy_name } => {
                self.ensure_no_job()?;
                let usable = lock(&self.bundle)
                    .strategies
                    .iter()
                    .any(|strategy| strategy.name == strategy_name && strategy.is_usable());
                if !usable {
                    return Err(CoreError::InvalidInput(format!(
                        "Стратегия «{strategy_name}» недоступна."
                    )));
                }
                self.edit_config(|config| config.active_strategy = Some(strategy_name));
                self.restart_if_running().await;
                self.emit();
                Ok(Reply::Ok)
            }
            Command::SetGameFilter { game_filter_mode } => {
                self.set_game_filter(game_filter_mode).await;
                Ok(Reply::Ok)
            }
            Command::ToggleGameFilter => {
                let mode = {
                    let config = lock(&self.config);
                    if config.game_filter == GameFilterMode::Off {
                        config.last_game_filter
                    } else {
                        GameFilterMode::Off
                    }
                };
                self.set_game_filter(mode).await;
                Ok(Reply::Ok)
            }
            Command::SetGameFilterPorts {
                tcp_ports,
                udp_ports,
            } => {
                let tcp = runner::validate_port_ranges(&tcp_ports).ok_or_else(|| {
                    CoreError::InvalidInput(format!("Неверные порты TCP: {tcp_ports}"))
                })?;
                let udp = runner::validate_port_ranges(&udp_ports).ok_or_else(|| {
                    CoreError::InvalidInput(format!("Неверные порты UDP: {udp_ports}"))
                })?;
                self.edit_config(|config| {
                    config.game_tcp_ports = tcp;
                    config.game_udp_ports = udp;
                });
                self.restart_if_running().await;
                self.emit();
                Ok(Reply::Ok)
            }
            Command::SetIpsetMode { ipset_mode } => {
                self.edit_config(|config| config.ipset_mode = ipset_mode);
                self.restart_if_running().await;
                self.emit();
                Ok(Reply::Ok)
            }
            Command::UpdateIpsetList => {
                self.update_ipset_list().await?;
                Ok(Reply::Ok)
            }
            Command::RunScan { scan_mode } => {
                self.require_elevated()?;
                self.require_bundle()?;
                let cancel = self.begin_job(JobKind::Scan, "Подбор стратегии…")?;
                tokio::spawn(self.clone().run_scan(scan_mode, cancel));
                Ok(Reply::Ok)
            }
            Command::TestStrategies {
                strategy_names,
                test_mode,
            } => {
                self.require_elevated()?;
                self.require_bundle()?;
                let cancel = self.begin_job(JobKind::Test, "Проверка стратегий…")?;
                tokio::spawn(self.clone().run_tests(strategy_names, test_mode, cancel));
                Ok(Reply::Ok)
            }
            Command::CancelJob => {
                lock(&self.cancel).store(true, Ordering::Relaxed);
                Ok(Reply::Ok)
            }
            Command::CheckForUpdates => {
                self.require_elevated()?;
                let cancel = self.begin_job(JobKind::Download, "Проверка обновлений…")?;
                let engine = self.clone();
                tokio::spawn(async move {
                    if let Err(error) = engine.check_for_updates(true, &cancel).await {
                        engine.notify("Обновление zapret", error.to_string());
                    }
                    engine.end_job();
                });
                Ok(Reply::Ok)
            }
            Command::SelectUpstreamVersion { version_tag } => {
                self.require_elevated()?;
                if !github::is_valid_tag(&version_tag) {
                    return Err(CoreError::InvalidInput("Неверная версия.".into()));
                }
                let cancel = self.begin_job(
                    JobKind::Download,
                    &format!("Переключение на {version_tag}…"),
                )?;
                let engine = self.clone();
                tokio::spawn(async move {
                    if let Err(error) = engine.select_version(&version_tag, &cancel).await {
                        engine.notify("Версия zapret", error.to_string());
                    }
                    engine.end_job();
                });
                Ok(Reply::Ok)
            }
            Command::DeleteUpstreamVersion { version_tag } => {
                self.require_elevated()?;
                self.ensure_no_job()?;
                if !github::is_valid_tag(&version_tag) {
                    return Err(CoreError::InvalidInput("Неверная версия.".into()));
                }
                if lock(&self.config).current_version.as_ref() == Some(&version_tag) {
                    return Err(CoreError::InvalidInput(
                        "Нельзя удалить используемую версию.".into(),
                    ));
                }
                let dir = self.paths.bundle_dir(&version_tag);
                blocking(move || std::fs::remove_dir_all(dir))
                    .await?
                    .map_err(|error| CoreError::Failed(error.to_string()))?;
                self.refresh_installed().await;
                self.emit();
                Ok(Reply::Ok)
            }
            Command::UpdateUserList { list_kind, entries } => {
                let (accepted, rejected) = lists::validate_entries(list_kind, &entries);
                self.edit_config(|config| *config.user_lists.get_mut(list_kind) = accepted);
                self.restart_if_running().await;
                self.emit();
                Ok(Reply::Rejected(rejected))
            }
            Command::GetUpstreamList { file_name } => {
                let known = lock(&self.bundle).list_files.contains(&file_name);
                let dir = self.current_bundle_dir().ok_or(CoreError::NoBundle)?;
                if !known {
                    return Err(CoreError::InvalidInput("Нет такого списка.".into()));
                }
                let path = dir.join("lists").join(file_name);
                let text = blocking(move || -> std::io::Result<String> {
                    use std::io::Read;
                    let mut bytes = Vec::new();
                    std::fs::File::open(path)?
                        .take(MAX_UPSTREAM_LIST_BYTES)
                        .read_to_end(&mut bytes)?;
                    Ok(String::from_utf8_lossy(&bytes).into_owned())
                })
                .await?
                .map_err(|error| CoreError::Failed(error.to_string()))?;
                Ok(Reply::Text(text))
            }
            Command::RunDiagnostics => {
                self.begin_job(JobKind::Diagnostics, "Диагностика…")?;
                let engine = self.clone();
                tokio::spawn(async move {
                    engine.run_diagnostics().await;
                    engine.end_job();
                });
                Ok(Reply::Ok)
            }
            Command::ApplyFix { fix } => self.apply_fix(fix).await,
            Command::RemoveServices => {
                self.require_elevated()?;
                self.ensure_no_job()?;
                self.stop_winws().await;
                let system = self.system.clone();
                blocking(move || diagnostics::remove_old_services(system.as_ref())).await??;
                self.refresh_checks().await;
                Ok(Reply::Ok)
            }
            Command::ClearDiscordCache => self.apply_fix(FixKind::ClearDiscordCache).await,
            Command::PrepareHostsUpdate => {
                let upstream = self.fetch_upstream_text(github::HOSTS_URL).await?;
                let hosts_path = self.system.hosts_path();
                let current = blocking(move || std::fs::read(hosts_path))
                    .await?
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .map_err(|error| {
                        CoreError::Failed(format!("не удалось прочитать hosts: {error}"))
                    })?;
                let missing = hosts::missing_lines(&upstream, &current);
                *lock(&self.pending_hosts) = Some(missing.clone());
                Ok(Reply::Lines(missing))
            }
            Command::ApplyHostsUpdate => {
                self.require_elevated()?;
                let lines = lock(&self.pending_hosts).take().ok_or_else(|| {
                    CoreError::InvalidInput("Сначала проверьте файл hosts.".into())
                })?;
                if lines.is_empty() {
                    return Ok(Reply::Ok);
                }
                let hosts_path = self.system.hosts_path();
                let backups = self.paths.hosts_backups_dir();
                let backup = blocking(move || -> std::io::Result<PathBuf> {
                    let backup = hosts::backup(&hosts_path, &backups)?;
                    hosts::append(&hosts_path, &lines)?;
                    Ok(backup)
                })
                .await?
                .map_err(|error| CoreError::Failed(format!("hosts: {error}")))?;
                tracing::info!("hosts updated, backup at {}", backup.display());
                lock(&self.runtime).hosts_backup_available = true;
                self.emit();
                Ok(Reply::Ok)
            }
            Command::RestoreHostsBackup => {
                self.require_elevated()?;
                let hosts_path = self.system.hosts_path();
                let backups = self.paths.hosts_backups_dir();
                let backups_for_check = backups.clone();
                blocking(move || hosts::restore_latest(&hosts_path, &backups))
                    .await?
                    .map_err(|error| CoreError::Failed(format!("hosts: {error}")))?;
                lock(&self.runtime).hosts_backup_available =
                    hosts::latest_backup(&backups_for_check).is_some();
                self.emit();
                Ok(Reply::Ok)
            }
            Command::GetLogTail => Ok(Reply::Lines(self.log.tail(500))),
            Command::GetDiagnosticsReport => Ok(Reply::Text(self.diagnostics_report())),
            Command::UpdateSettings { settings } => {
                self.update_settings(settings).await?;
                Ok(Reply::Ok)
            }
            Command::SetFake {
                fake_slot,
                fake_name,
            } => {
                if let Some(name) = &fake_name {
                    let known = lock(&self.bundle)
                        .fakes
                        .as_ref()
                        .is_some_and(|fakes| fakes.contains(name));
                    if !known {
                        return Err(CoreError::InvalidInput("Нет такого файла фейка.".into()));
                    }
                }
                self.edit_config(|config| match fake_slot {
                    FakeSlot::DiscordUdp => config.fake_discord_udp = fake_name,
                    FakeSlot::GameUdp => config.fake_game_udp = fake_name,
                });
                self.restart_if_running().await;
                self.emit();
                Ok(Reply::Ok)
            }
            Command::FinishFirstRun => {
                self.edit_config(|config| config.first_run_done = true);
                self.emit();
                Ok(Reply::Ok)
            }
            Command::Shutdown => {
                self.shutting_down.store(true, Ordering::Relaxed);
                lock(&self.cancel).store(true, Ordering::Relaxed);
                self.stop_winws().await;
                tracing::info!("shut down");
                Ok(Reply::Ok)
            }
        }
    }

    fn require_bundle(&self) -> Result<(), CoreError> {
        if lock(&self.bundle).strategies.is_empty() {
            Err(CoreError::NoBundle)
        } else {
            Ok(())
        }
    }

    async fn set_game_filter(self: &Arc<Self>, mode: GameFilterMode) {
        self.edit_config(|config| {
            config.game_filter = mode;
            if mode != GameFilterMode::Off {
                config.last_game_filter = mode;
            }
        });
        self.restart_if_running().await;
        self.emit();
    }

    async fn update_settings(&self, mut settings: Settings) -> Result<(), CoreError> {
        settings.watchdog_period_minutes = settings.watchdog_period_minutes.clamp(1, 24 * 60);
        let old_autostart = lock(&self.config).settings.autostart;
        if settings.autostart != old_autostart {
            self.require_elevated()?;
            let system = self.system.clone();
            let enabled = settings.autostart;
            blocking(move || system.set_autostart(enabled)).await??;
        }
        self.edit_config(|config| config.settings = settings);
        self.watchdog_kick.notify_one();
        self.emit();
        Ok(())
    }

    async fn fetch_upstream_text(&self, url: &str) -> Result<String, CoreError> {
        if self.fail_downloads {
            return Err(CoreError::Network(
                "имитация: загрузка не удалась (download_fails)".into(),
            ));
        }
        Ok(github::fetch_text(&self.http, url).await?)
    }

    async fn update_ipset_list(self: &Arc<Self>) -> Result<(), CoreError> {
        self.require_elevated()?;
        let text = self.fetch_upstream_text(github::IPSET_URL).await?;
        let entries: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        let valid: Vec<String> = entries
            .iter()
            .filter_map(|entry| lists::validate_entry(UserListKind::IpsetExclude, entry))
            .collect();
        if valid.is_empty() {
            return Err(CoreError::Failed(
                "скачанный список IPSet пуст или повреждён".into(),
            ));
        }
        if valid.len() != entries.len() {
            tracing::warn!(
                "ipset list: dropped {} invalid lines",
                entries.len() - valid.len()
            );
        }
        let path = self.paths.ipset_override_file();
        let content = valid.join("\n") + "\n";
        blocking(move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, content)
        })
        .await?
        .map_err(|error| CoreError::Failed(error.to_string()))?;
        tracing::info!("ipset list updated: {} entries", valid.len());
        self.restart_if_running().await;
        self.emit();
        Ok(())
    }

    // ----- winws -------------------------------------------------------------------------------------------------

    fn set_run_state(&self, run_state: RunState, error: Option<ErrorInfo>) {
        {
            let mut runtime = lock(&self.runtime);
            runtime.run_state = run_state;
            runtime.error = error;
        }
        self.emit();
    }

    fn fail_start(&self, error: CoreError) -> CoreError {
        let tail: Vec<String> = lock(&self.output_tail).iter().cloned().collect();
        let message = match &error {
            CoreError::Failed(message) => message.clone(),
            other => other.to_string(),
        };
        self.set_run_state(
            RunState::Error,
            Some(ErrorInfo {
                message,
                output_tail: tail,
            }),
        );
        error
    }

    /// Starts winws with `strategy_name` (default: the active strategy), replacing a running instance.
    /// `ipset_mode` overrides the configured mode for this run only (scans).
    pub(crate) async fn start_winws(
        self: &Arc<Self>,
        strategy_name: Option<&str>,
        ipset_mode: Option<IpsetMode>,
    ) -> Result<(), CoreError> {
        self.require_elevated()?;
        let config = lock(&self.config).clone();
        let version = config.current_version.clone().ok_or(CoreError::NoBundle)?;
        let name = strategy_name
            .map(str::to_string)
            .or(config.active_strategy.clone())
            .ok_or(CoreError::NoStrategy)?;
        let (strategy, fakes) = {
            let bundle = lock(&self.bundle);
            let strategy = bundle
                .strategies
                .iter()
                .find(|strategy| strategy.name == name && strategy.is_usable())
                .cloned();
            (strategy, bundle.fakes.clone().unwrap_or_default())
        };
        let strategy = strategy
            .ok_or_else(|| CoreError::InvalidInput(format!("Стратегия «{name}» недоступна.")))?;
        let known_fake = |fake: &Option<String>| fake.clone().filter(|name| fakes.contains(name));
        let fake_discord = known_fake(&config.fake_discord_udp);
        let fake_game = known_fake(&config.fake_game_udp);

        let mut slot = self.winws.lock().await;
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(running) = slot.take() {
            running.stop().await;
        }
        lock(&self.output_tail).clear();
        self.set_run_state(RunState::Starting, None);

        let bundle_dir = self.paths.bundle_dir(&version);
        let runtime_lists = self.paths.runtime_lists_dir();
        {
            let bundle_lists = bundle_dir.join("lists");
            let runtime_lists = runtime_lists.clone();
            let user_lists = config.user_lists.clone();
            let mode = ipset_mode.unwrap_or(config.ipset_mode);
            let override_file = self.paths.ipset_override_file();
            let system = self.system.clone();
            let prepared = blocking(move || -> Result<(), CoreError> {
                let override_file = override_file.exists().then_some(override_file);
                lists::materialize(
                    &bundle_lists,
                    &runtime_lists,
                    &user_lists,
                    mode,
                    override_file.as_deref(),
                )
                .map_err(|error| {
                    CoreError::Failed(format!("не удалось подготовить списки: {error}"))
                })?;
                // Parity with upstream: winws strategies expect TCP timestamps.
                if system.system_facts().tcp_timestamps == Some(false) {
                    match system.enable_tcp_timestamps() {
                        Ok(()) => tracing::info!("TCP timestamps enabled"),
                        Err(error) => tracing::warn!("cannot enable TCP timestamps: {error}"),
                    }
                }
                Ok(())
            })
            .await
            .and_then(|result| result);
            if let Err(error) = prepared {
                return Err(self.fail_start(error));
            }
        }
        match self.driver.kill_leftovers().await {
            Ok(0) => {}
            Ok(count) => tracing::info!("killed {count} leftover winws process(es)"),
            Err(error) => tracing::warn!("cannot kill leftover winws: {error}"),
        }
        let system = self.system.clone();
        let conflicts = blocking(move || {
            let services = system.services().unwrap_or_default();
            let mut found: Vec<String> = services
                .iter()
                .filter(|service| {
                    service.name.eq_ignore_ascii_case("zapret")
                        || (service.running
                            && diagnostics::CONFLICTING_SERVICES
                                .iter()
                                .any(|name| service.name.eq_ignore_ascii_case(name)))
                })
                .map(|service| service.name.clone())
                .collect();
            found.dedup();
            found
        })
        .await
        .unwrap_or_default();
        if !conflicts.is_empty() {
            return Err(self.fail_start(CoreError::Conflict(format!(
                "Мешают службы: {}. Удалите старые установки zapret и другие обходы блокировок \
                 (Диагностика → «Удалить службы zapret / WinDivert»).",
                conflicts.join(", ")
            ))));
        }

        let settings = runner::LaunchSettings {
            game_filter: config.game_filter,
            game_tcp_ports: &config.game_tcp_ports,
            game_udp_ports: &config.game_udp_ports,
            fake_discord_udp: fake_discord.as_deref(),
            fake_game_udp: fake_game.as_deref(),
        };
        let spec = runner::launch_spec(&strategy, &bundle_dir, &runtime_lists, &settings);
        tracing::info!(
            "starting winws: strategy {:?}, args: {}",
            spec.strategy_name,
            spec.args.join(" ")
        );

        let (output_sender, mut output_receiver) = mpsc::unbounded_channel::<String>();
        let tail = self.output_tail.clone();
        tokio::spawn(async move {
            while let Some(line) = output_receiver.recv().await {
                tracing::info!(target: "winws", "{line}");
                let mut tail = lock(&tail);
                if tail.len() == OUTPUT_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });
        let running = match self.driver.start(&spec, output_sender).await {
            Ok(running) => running,
            Err(error) => {
                return Err(self.fail_start(CoreError::Failed(format!(
                    "не удалось запустить winws: {error}"
                ))));
            }
        };
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut exit_watcher = running.exit_watcher();
        let engine = self.clone();
        tokio::spawn(async move {
            let code = match exit_watcher.wait_for(|code| code.is_some()).await {
                Ok(code) => code.unwrap_or(-1),
                Err(_) => return,
            };
            if engine.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            // Let the output pump flush the last lines.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            tracing::warn!("winws exited unexpectedly with code {code}");
            let in_job = lock(&engine.runtime).job.is_some();
            let _ = engine.fail_start(CoreError::Failed(format!(
                "winws неожиданно завершился (код {code})."
            )));
            if !in_job {
                engine.notify(
                    "zapret остановлен",
                    "winws неожиданно завершился. Подробности на главной странице.",
                );
            }
        });
        *slot = Some(running);
        drop(slot);
        self.set_run_state(RunState::Running, None);
        Ok(())
    }

    pub(crate) async fn stop_winws(&self) {
        let mut slot = self.winws.lock().await;
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(running) = slot.take() {
            running.stop().await;
            tracing::info!("winws stopped");
        }
        drop(slot);
        let mut runtime = lock(&self.runtime);
        if runtime.run_state != RunState::Error {
            runtime.run_state = RunState::Stopped;
        }
        drop(runtime);
        self.emit();
    }

    pub(crate) async fn winws_alive(&self) -> bool {
        self.winws
            .lock()
            .await
            .as_ref()
            .is_some_and(|running| !running.has_exited())
    }

    /// Applies changed settings by restarting winws (D27). Skipped during jobs (they restore state afterwards).
    pub(crate) async fn restart_if_running(self: &Arc<Self>) {
        let running = {
            let runtime = lock(&self.runtime);
            runtime.job.is_none() && runtime.run_state == RunState::Running
        };
        if running && let Err(error) = self.start_winws(None, None).await {
            tracing::warn!("restart failed: {error}");
        }
    }

    // ----- upstream ----------------------------------------------------------------------------------------------

    async fn list_releases(&self) -> Result<Vec<Release>, CoreError> {
        if self.fail_downloads {
            return Err(CoreError::Network(
                "имитация: загрузка не удалась (download_fails)".into(),
            ));
        }
        let releases =
            github::list_releases(&self.http, &self.paths.cache_dir().join("releases.json"))
                .await?;
        lock(&self.runtime).releases = releases.clone();
        Ok(releases)
    }

    /// Refreshes the version list without installing anything (dropdown contents).
    pub(crate) async fn refresh_release_list(&self) {
        match self.list_releases().await {
            Ok(_) => self.emit(),
            Err(error) => tracing::warn!("cannot list releases: {error}"),
        }
    }

    async fn install(&self, release: &Release, cancel: &AtomicBool) -> Result<(), CoreError> {
        if self.fail_downloads {
            return Err(CoreError::Network(
                "имитация: загрузка не удалась (download_fails)".into(),
            ));
        }
        tracing::info!("installing upstream {}", release.tag);
        let tag = release.tag.clone();
        let progress = |done: u64, total: Option<u64>| {
            let fraction = total
                .filter(|total| *total > 0)
                .map(|total| done as f32 / total as f32);
            if let Some(job) = lock(&self.runtime).job.as_mut() {
                job.message = format!("Загрузка zapret {tag}: {} КБ", done / 1024);
                job.progress = fraction;
            }
            if done % (256 * 1024) < 16 * 1024 {
                self.emit();
            }
        };
        crate::upstream::install_release(&self.http, &self.paths, release, progress, cancel)
            .await?;
        self.refresh_installed().await;
        tracing::info!("upstream {} installed", release.tag);
        Ok(())
    }

    /// Update check (manual or auto): store the newest release; switch to it unless pinned (D26).
    pub(crate) async fn check_for_updates(
        self: &Arc<Self>,
        manual: bool,
        cancel: &AtomicBool,
    ) -> Result<(), CoreError> {
        let releases = self.list_releases().await?;
        let latest = releases
            .first()
            .cloned()
            .ok_or_else(|| CoreError::Network("на GitHub нет подходящих релизов".into()))?;
        let installed = lock(&self.runtime).installed.contains(&latest.tag);
        if !installed {
            self.install(&latest, cancel).await?;
        }
        let (current, pinned) = {
            let config = lock(&self.config);
            (config.current_version.clone(), config.pinned)
        };
        if current.as_ref() == Some(&latest.tag) {
            lock(&self.runtime).update_available = None;
            if manual {
                self.notify(
                    "Обновление zapret",
                    format!("Установлена последняя версия {}.", latest.tag),
                );
            }
        } else if pinned && current.is_some() {
            lock(&self.runtime).update_available = Some(latest.tag.clone());
            self.notify(
                "Доступна новая версия zapret",
                format!(
                    "Версия {} скачана. Переключиться можно в настройках.",
                    latest.tag
                ),
            );
        } else {
            self.switch_version(&latest.tag, false).await;
            self.notify(
                "Обновление zapret",
                format!("Установлена версия {}.", latest.tag),
            );
        }
        self.emit();
        Ok(())
    }

    async fn select_version(
        self: &Arc<Self>,
        tag: &str,
        cancel: &AtomicBool,
    ) -> Result<(), CoreError> {
        let installed = lock(&self.runtime).installed.contains(&tag.to_string());
        let mut releases = lock(&self.runtime).releases.clone();
        if releases.is_empty() {
            releases = self.list_releases().await.unwrap_or_default();
        }
        if !installed {
            let release = releases
                .iter()
                .find(|release| release.tag == tag)
                .cloned()
                .ok_or_else(|| {
                    CoreError::InvalidInput(format!("Версия {tag} не найдена на GitHub."))
                })?;
            self.install(&release, cancel).await?;
        }
        let is_latest = releases.first().is_some_and(|release| release.tag == tag);
        self.switch_version(tag, !is_latest).await;
        Ok(())
    }

    /// Makes `tag` the current bundle, keeps the active strategy by name, restarts winws if it was running.
    async fn switch_version(self: &Arc<Self>, tag: &str, pinned: bool) {
        let was_running = lock(&self.runtime).run_state == RunState::Running;
        self.edit_config(|config| {
            config.current_version = Some(tag.to_string());
            config.pinned = pinned;
        });
        if !pinned {
            lock(&self.runtime).update_available = None;
        }
        self.reload_bundle().await;
        let active = lock(&self.config).active_strategy.clone();
        let kept = active.as_ref().is_some_and(|name| {
            lock(&self.bundle)
                .strategies
                .iter()
                .any(|strategy| &strategy.name == name && strategy.is_usable())
        });
        if !kept {
            self.edit_config(|config| config.active_strategy = None);
        }
        tracing::info!("switched to upstream {tag} (pinned: {pinned}, strategy kept: {kept})");
        if was_running {
            if kept {
                if let Err(error) = self.start_winws(None, None).await {
                    tracing::warn!("restart after version switch failed: {error}");
                }
            } else {
                self.stop_winws().await;
                self.notify(
                    "Версия zapret",
                    "В новой версии нет выбранной стратегии. Запустите автоподбор.",
                );
            }
        }
        self.emit();
    }

    pub(crate) async fn redownload_current(
        self: &Arc<Self>,
        cancel: &AtomicBool,
    ) -> Result<(), CoreError> {
        let tag = lock(&self.config)
            .current_version
            .clone()
            .ok_or(CoreError::NoBundle)?;
        let release = self
            .list_releases()
            .await?
            .into_iter()
            .find(|release| release.tag == tag)
            .ok_or_else(|| {
                CoreError::InvalidInput(format!("Версия {tag} не найдена на GitHub."))
            })?;
        let was_running = lock(&self.runtime).run_state == RunState::Running;
        self.stop_winws().await;
        self.install(&release, cancel).await?;
        self.reload_bundle().await;
        if was_running {
            self.start_winws(None, None).await?;
        }
        Ok(())
    }

    /// Auto-check: list releases on start, check for updates on start and every 6 h when enabled.
    async fn update_loop(self: Arc<Self>) {
        self.refresh_release_list().await;
        loop {
            let (enabled, first_run_done) = {
                let config = lock(&self.config);
                (config.settings.auto_check_updates, config.first_run_done)
            };
            if enabled
                && first_run_done
                && self.is_elevated()
                && let Ok(cancel) = self.begin_job(JobKind::Download, "Проверка обновлений…")
            {
                if let Err(error) = self.check_for_updates(false, &cancel).await {
                    tracing::warn!("auto update check failed: {error}");
                }
                self.end_job();
            }
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            if self.shutting_down.load(Ordering::Relaxed) {
                return;
            }
        }
    }

    // ----- diagnostics -------------------------------------------------------------------------------------------

    async fn run_diagnostics(&self) {
        let system = self.system.clone();
        let bundle_dir = self.current_bundle_dir();
        let checks = blocking(move || {
            diagnostics::evaluate(&diagnostics::collect(
                system.as_ref(),
                bundle_dir.as_deref(),
            ))
        })
        .await
        .unwrap_or_default();
        for check in &checks {
            if check.status != CheckStatus::Ok {
                tracing::info!(
                    "diagnostics {}. {}: {:?} {}",
                    check.number,
                    check.title,
                    check.status,
                    check.detail
                );
            }
        }
        lock(&self.runtime).checks = checks;
    }

    /// Re-runs the checks after a fix (only if they were shown before).
    async fn refresh_checks(&self) {
        if !lock(&self.runtime).checks.is_empty() {
            self.run_diagnostics().await;
        }
        self.emit();
    }

    async fn apply_fix(self: &Arc<Self>, fix: FixKind) -> Result<Reply, CoreError> {
        self.require_elevated()?;
        let system = self.system.clone();
        let reply = match fix {
            FixKind::EnableTcpTimestamps => {
                blocking(move || system.enable_tcp_timestamps()).await??;
                Reply::Ok
            }
            FixKind::RemoveWinDivert => {
                self.ensure_no_job()?;
                self.stop_winws().await;
                blocking(move || diagnostics::remove_windivert(system.as_ref())).await??;
                Reply::Ok
            }
            FixKind::RemoveConflictingServices => {
                self.ensure_no_job()?;
                self.stop_winws().await;
                blocking(move || diagnostics::remove_conflicting_services(system.as_ref()))
                    .await??;
                Reply::Ok
            }
            FixKind::ClearDiscordCache => {
                let report =
                    blocking(move || diagnostics::clear_discord_cache(system.as_ref())).await?;
                for line in &report {
                    tracing::info!("discord cache: {line}");
                }
                Reply::Lines(report)
            }
            FixKind::RedownloadBundle => {
                let cancel = self.begin_job(JobKind::Download, "Повторная загрузка zapret…")?;
                let engine = self.clone();
                tokio::spawn(async move {
                    if let Err(error) = engine.redownload_current(&cancel).await {
                        engine.notify("Загрузка zapret", error.to_string());
                    }
                    engine.end_job();
                    engine.refresh_checks().await;
                });
                return Ok(Reply::Ok);
            }
        };
        self.refresh_checks().await;
        Ok(reply)
    }

    pub fn diagnostics_report(&self) -> String {
        crate::report::build(self)
    }
}
