//! Mock platform: the whole app is usable on Linux with simulated, deterministic behaviour.
//! Scenarios via `ZAPRET_GUI_MOCK_SCENARIO`: `winws_exits`, `nothing_works`, `download_fails`, `not_elevated`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use super::{
    Platform, PlatformError, RunningWinws, ServiceInfo, System, SystemFacts, WinwsDriver,
    WinwsLaunchSpec,
};
use crate::api::{DpiStatus, ProbeOutcome};
use crate::paths::Paths;
use crate::prober::ProbeBackend;
use crate::prober::targets::DpiTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scenario {
    pub winws_exits: bool,
    pub nothing_works: bool,
    pub download_fails: bool,
    pub not_elevated: bool,
}

impl Scenario {
    /// Comma-separated scenario names.
    pub fn parse(text: &str) -> Self {
        let mut scenario = Self::default();
        for name in text.split(',').map(str::trim) {
            match name {
                "winws_exits" => scenario.winws_exits = true,
                "nothing_works" => scenario.nothing_works = true,
                "download_fails" => scenario.download_fails = true,
                "not_elevated" => scenario.not_elevated = true,
                "" => {}
                other => tracing::warn!("unknown mock scenario {other:?}"),
            }
        }
        scenario
    }
}

pub fn create(paths: &Paths) -> Platform {
    let scenario = Scenario::parse(&std::env::var("ZAPRET_GUI_MOCK_SCENARIO").unwrap_or_default());
    tracing::info!("mock platform, scenario {scenario:?}");
    create_with(paths, scenario)
}

pub fn create_with(paths: &Paths, scenario: Scenario) -> Platform {
    let running = Arc::new(Mutex::new(None));
    let system = Arc::new(MockSystem::new(
        paths.data_dir.join("mock"),
        !scenario.not_elevated,
    ));
    Platform {
        system: system.clone(),
        driver: Arc::new(MockDriver {
            running: running.clone(),
            exits: scenario.winws_exits,
        }),
        probe: Arc::new(MockProbe {
            running,
            nothing_works: scenario.nothing_works,
            system,
        }),
        is_mock: true,
        fail_downloads: scenario.download_fails,
    }
}

/// FNV-1a, for stable pseudo-random choices.
fn stable_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

struct MockState {
    elevated: bool,
    autostart: bool,
    services: Vec<ServiceInfo>,
    processes: Vec<String>,
    tcp_timestamps: bool,
}

pub struct MockSystem {
    state: Mutex<MockState>,
    root: PathBuf,
}

fn service(name: &str, display_name: &str, running: bool) -> ServiceInfo {
    ServiceInfo {
        name: name.into(),
        display_name: display_name.into(),
        running,
        stop_pending: false,
        is_driver: name == "WinDivert",
    }
}

impl MockSystem {
    fn new(root: PathBuf, elevated: bool) -> Self {
        let _ = std::fs::create_dir_all(root.join("appdata").join("discord").join("Cache"));
        let hosts = root.join("hosts");
        if !hosts.exists() {
            let _ = std::fs::write(&hosts, "127.0.0.1 localhost\n");
        }
        Self {
            state: Mutex::new(MockState {
                elevated,
                autostart: false,
                services: vec![
                    service("BFE", "Base Filtering Engine", true),
                    service("ExampleVpnSvc", "Example VPN Service", true),
                    service("GoodbyeDPI", "GoodbyeDPI", false),
                    service("WinDivert", "WinDivert", false),
                ],
                processes: vec!["explorer.exe".into()],
                tcp_timestamps: false,
            }),
            root,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl System for MockSystem {
    fn is_elevated(&self) -> bool {
        self.state().elevated
    }

    fn relaunch_elevated(&self) -> Result<(), PlatformError> {
        Err(PlatformError::Failed(
            "В режиме имитации перезапуск недоступен (уберите not_elevated из ZAPRET_GUI_MOCK_SCENARIO).".into(),
        ))
    }

    fn set_autostart(&self, enabled: bool) -> Result<(), PlatformError> {
        self.state().autostart = enabled;
        tracing::info!(
            "mock: autostart task {}",
            if enabled { "created" } else { "deleted" }
        );
        Ok(())
    }

    fn services(&self) -> Result<Vec<ServiceInfo>, PlatformError> {
        Ok(self.state().services.clone())
    }

    fn remove_service(&self, name: &str) -> Result<(), PlatformError> {
        self.state()
            .services
            .retain(|service| !service.name.eq_ignore_ascii_case(name));
        tracing::info!("mock: service {name} removed");
        Ok(())
    }

    fn process_names(&self) -> Result<Vec<String>, PlatformError> {
        Ok(self.state().processes.clone())
    }

    fn kill_processes(&self, image_name: &str) -> Result<usize, PlatformError> {
        let mut state = self.state();
        let before = state.processes.len();
        state
            .processes
            .retain(|process| !process.eq_ignore_ascii_case(image_name));
        Ok(before - state.processes.len())
    }

    fn system_facts(&self) -> SystemFacts {
        SystemFacts {
            proxy_enabled: false,
            proxy_server: String::new(),
            doh_configured: false,
            tcp_timestamps: Some(self.state().tcp_timestamps),
        }
    }

    fn enable_tcp_timestamps(&self) -> Result<(), PlatformError> {
        self.state().tcp_timestamps = true;
        Ok(())
    }

    fn hosts_path(&self) -> PathBuf {
        self.root.join("hosts")
    }

    fn appdata_dir(&self) -> Option<PathBuf> {
        Some(self.root.join("appdata"))
    }

    fn secure_data_dir(&self, _dir: &Path) -> Result<(), PlatformError> {
        Ok(())
    }

    fn ping(&self, host: &str, _timeout: Duration) -> Option<u32> {
        std::thread::sleep(Duration::from_millis(20));
        Some(8 + (stable_hash(host) % 40) as u32)
    }

    fn wait_network_change(&self) {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
}

type RunningStrategy = Arc<Mutex<Option<(u64, String)>>>;

pub struct MockDriver {
    running: RunningStrategy,
    exits: bool,
}

#[async_trait::async_trait]
impl WinwsDriver for MockDriver {
    async fn start(
        &self,
        spec: &WinwsLaunchSpec,
        output: mpsc::UnboundedSender<String>,
    ) -> Result<RunningWinws, PlatformError> {
        let (handle, control) = RunningWinws::new();
        let instance =
            crate::paths::unix_now() ^ stable_hash(&format!("{:?}", std::time::Instant::now()));
        *self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((instance, spec.strategy_name.clone()));
        let _ = output.send(format!(
            "mock winws: strategy {:?}, {} arguments",
            spec.strategy_name,
            spec.args.len()
        ));
        let running = self.running.clone();
        let exits = self.exits;
        tokio::spawn(async move {
            let crash = async {
                if exits {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            let code = tokio::select! {
                _ = control.kill_requested => 1,
                _ = crash => {
                    let _ = output.send("mock winws: error: simulated failure (winws_exits)".into());
                    2
                }
            };
            let mut current = running
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if current.as_ref().is_some_and(|(id, _)| *id == instance) {
                *current = None;
            }
            drop(current);
            let _ = control.exit.send(Some(code));
        });
        Ok(handle)
    }

    async fn kill_leftovers(&self) -> Result<usize, PlatformError> {
        Ok(0)
    }
}

pub struct MockProbe {
    running: RunningStrategy,
    nothing_works: bool,
    system: Arc<MockSystem>,
}

/// 0 = works fully, 1 = only Discord works, else nothing behind DPI works.
fn strategy_quality(name: &str) -> u64 {
    stable_hash(name) % 4
}

fn behind_dpi(url_or_host: &str) -> bool {
    ["discord", "youtu", "ytimg", "googlevideo"]
        .iter()
        .any(|marker| url_or_host.contains(marker))
}

impl MockProbe {
    fn reaches(&self, url_or_host: &str) -> bool {
        if !behind_dpi(url_or_host) {
            return true;
        }
        if self.nothing_works {
            return false;
        }
        let running = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match running.as_ref().map(|(_, name)| strategy_quality(name)) {
            Some(0) => true,
            Some(1) => url_or_host.contains("discord"),
            _ => false,
        }
    }
}

#[async_trait::async_trait]
impl ProbeBackend for MockProbe {
    async fn http(&self, url: &str) -> [ProbeOutcome; 3] {
        tokio::time::sleep(Duration::from_millis(50 + stable_hash(url) % 150)).await;
        if self.reaches(url) {
            [ProbeOutcome::Ok, ProbeOutcome::Ok, ProbeOutcome::Ok]
        } else {
            [
                ProbeOutcome::Error,
                ProbeOutcome::Error,
                ProbeOutcome::Error,
            ]
        }
    }

    async fn dpi(&self, host: &str) -> [DpiStatus; 3] {
        tokio::time::sleep(Duration::from_millis(100 + stable_hash(host) % 200)).await;
        let running_quality = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|(_, name)| strategy_quality(name));
        if !self.nothing_works && running_quality == Some(0) {
            [DpiStatus::Ok; 3]
        } else {
            [
                DpiStatus::LikelyBlocked,
                DpiStatus::LikelyBlocked,
                DpiStatus::Ok,
            ]
        }
    }

    async fn ping(&self, host: &str) -> Option<u32> {
        let system = self.system.clone();
        let host = host.to_string();
        tokio::task::spawn_blocking(move || system.ping(&host, Duration::from_secs(1)))
            .await
            .ok()
            .flatten()
    }

    async fn dpi_suite(&self) -> Option<Vec<DpiTarget>> {
        Some(
            [
                ("US.GH-HPRN", "Self check", "hyperion-cs.github.io"),
                ("DE.AWS-01", "AWS", "amplifon.com"),
            ]
            .iter()
            .map(|(id, provider, host)| DpiTarget {
                id: (*id).into(),
                provider: (*provider).into(),
                host: (*host).into(),
            })
            .collect(),
        )
    }
}
