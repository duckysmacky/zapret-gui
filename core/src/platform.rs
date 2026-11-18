//! Platform seams: everything Windows-specific sits behind these traits (invariant 7).
//! Windows gets the real implementations; other platforms (and `mock-platform`) get mocks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use crate::paths::Paths;
use crate::prober::ProbeBackend;

#[cfg(any(not(windows), feature = "mock-platform"))]
pub mod mock;
#[cfg(windows)]
pub mod windows;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    pub name: String,
    pub display_name: String,
    pub running: bool,
    pub stop_pending: bool,
    /// Kernel driver (e.g. WinDivert) rather than a Win32 service.
    pub is_driver: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemFacts {
    pub proxy_enabled: bool,
    pub proxy_server: String,
    /// Any interface has `DohFlags > 0` (Secure DNS configured).
    pub doh_configured: bool,
    /// `None` when it couldn't be determined.
    pub tcp_timestamps: Option<bool>,
}

/// OS facts and actions. All methods block; call them through `spawn_blocking`.
pub trait System: Send + Sync {
    fn is_elevated(&self) -> bool;
    /// Starts a new elevated instance (UAC). The caller then quits.
    fn relaunch_elevated(&self) -> Result<(), PlatformError>;
    /// Creates or deletes the elevated logon task.
    fn set_autostart(&self, enabled: bool) -> Result<(), PlatformError>;
    /// All services and drivers, any state.
    fn services(&self) -> Result<Vec<ServiceInfo>, PlatformError>;
    /// Stops and deletes a service. Missing services are not an error.
    fn remove_service(&self, name: &str) -> Result<(), PlatformError>;
    /// Image names of running processes (e.g. `winws.exe`).
    fn process_names(&self) -> Result<Vec<String>, PlatformError>;
    /// Kills every process with this image name; returns how many were killed.
    fn kill_processes(&self, image_name: &str) -> Result<usize, PlatformError>;
    fn system_facts(&self) -> SystemFacts;
    fn enable_tcp_timestamps(&self) -> Result<(), PlatformError>;
    fn hosts_path(&self) -> PathBuf;
    /// The user's roaming app data (`%APPDATA%`), for Discord caches.
    fn appdata_dir(&self) -> Option<PathBuf>;
    /// Makes the data dir admin-only-writable (invariant 3).
    fn secure_data_dir(&self, dir: &Path) -> Result<(), PlatformError>;
    /// One ICMP echo; latency in ms, `None` on timeout/failure.
    fn ping(&self, host: &str, timeout: Duration) -> Option<u32>;
    /// Returns when the network configuration changes.
    fn wait_network_change(&self);
}

#[derive(Debug, Clone)]
pub struct WinwsLaunchSpec {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub strategy_name: String,
}

/// A started winws. Dropping it does not kill the process; call `stop`.
pub struct RunningWinws {
    kill: Option<oneshot::Sender<()>>,
    exited: watch::Receiver<Option<i32>>,
}

/// The driver side of a `RunningWinws`: wait for the kill request, report the exit code.
pub struct WinwsControl {
    pub kill_requested: oneshot::Receiver<()>,
    pub exit: watch::Sender<Option<i32>>,
}

impl RunningWinws {
    pub fn new() -> (Self, WinwsControl) {
        let (kill_sender, kill_receiver) = oneshot::channel();
        let (exit_sender, exit_receiver) = watch::channel(None);
        (
            Self {
                kill: Some(kill_sender),
                exited: exit_receiver,
            },
            WinwsControl {
                kill_requested: kill_receiver,
                exit: exit_sender,
            },
        )
    }

    /// Resolves with the exit code once the process is gone.
    pub fn exit_watcher(&self) -> watch::Receiver<Option<i32>> {
        self.exited.clone()
    }

    pub fn has_exited(&self) -> bool {
        self.exited.borrow().is_some()
    }

    /// Kills winws and waits for it to exit.
    pub async fn stop(mut self) {
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
        let wait = self.exited.wait_for(|code| code.is_some());
        if tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .is_err()
        {
            tracing::warn!("winws did not report exit within 10 s");
        }
    }
}

#[async_trait::async_trait]
pub trait WinwsDriver: Send + Sync {
    /// Spawns winws (hidden). Output lines go to `output`.
    async fn start(
        &self,
        spec: &WinwsLaunchSpec,
        output: mpsc::UnboundedSender<String>,
    ) -> Result<RunningWinws, PlatformError>;
    /// Kills any `winws.exe` not started by us (D22).
    async fn kill_leftovers(&self) -> Result<usize, PlatformError>;
}

pub struct Platform {
    pub system: Arc<dyn System>,
    pub driver: Arc<dyn WinwsDriver>,
    pub probe: Arc<dyn ProbeBackend>,
    pub is_mock: bool,
    /// Mock scenario: upstream downloads fail.
    pub fail_downloads: bool,
}

/// The real platform on Windows, the mock elsewhere (or with `mock-platform`).
pub fn create(paths: &Paths) -> Platform {
    #[cfg(any(not(windows), feature = "mock-platform"))]
    {
        mock::create(paths)
    }
    #[cfg(all(windows, not(feature = "mock-platform")))]
    {
        windows::create(paths)
    }
}
