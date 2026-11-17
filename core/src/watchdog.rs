//! Watchdog (D19): periodic light probe of the critical targets while the bypass runs.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::Notify;

use crate::api::{JobKind, RunState, ScanMode, WatchdogStatus};
use crate::engine::{Engine, lock};
use crate::paths::unix_now;
use crate::prober::probe_targets;
use crate::prober::targets::load_targets;

const TICK: Duration = Duration::from_secs(30);
const RETRY_DELAY: Duration = Duration::from_secs(10);
const NETWORK_SETTLE: Duration = Duration::from_secs(5);

impl Engine {
    /// All critical targets work with the current winws.
    async fn light_probe(&self) -> bool {
        let mut targets = load_targets(self.current_bundle_dir().as_deref());
        targets.retain(|target| target.critical);
        probe_targets(&self.probe, &targets)
            .await
            .iter()
            .all(|result| result.works())
    }

    pub(crate) async fn watchdog_loop(self: Arc<Self>) {
        let network_changed = Arc::new(Notify::new());
        {
            let system = self.system.clone();
            let network_changed = network_changed.clone();
            // A detached thread: a blocking task that never ends would hold up runtime shutdown.
            let spawned = std::thread::Builder::new()
                .name("network-watch".into())
                .spawn(move || {
                    loop {
                        system.wait_network_change();
                        network_changed.notify_one();
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!("network change watcher unavailable: {error}");
            }
        }
        let mut last_check = Instant::now();
        let mut last_wall = SystemTime::now();
        let mut last_tick = Instant::now();
        loop {
            let mut triggered = tokio::select! {
                _ = tokio::time::sleep(TICK) => false,
                _ = self.watchdog_kick.notified() => false,
                _ = network_changed.notified() => {
                    tokio::time::sleep(NETWORK_SETTLE).await;
                    tracing::info!("watchdog: network changed");
                    true
                }
            };
            if self.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            // Resume from sleep: wall time jumped further than monotonic time.
            let wall_elapsed = last_wall.elapsed().unwrap_or_default();
            if wall_elapsed > last_tick.elapsed() + Duration::from_secs(60) {
                tracing::info!("watchdog: resume from sleep detected");
                triggered = true;
            }
            last_wall = SystemTime::now();
            last_tick = Instant::now();

            let (enabled, period, auto_rescan) = {
                let settings = &lock(&self.config).settings;
                (
                    settings.watchdog_enabled,
                    Duration::from_secs(u64::from(settings.watchdog_period_minutes) * 60),
                    settings.auto_rescan,
                )
            };
            let idle_and_running = {
                let runtime = lock(&self.runtime);
                runtime.job.is_none() && runtime.run_state == RunState::Running
            };
            if !enabled || !idle_and_running {
                last_check = Instant::now();
                continue;
            }
            if !triggered && last_check.elapsed() < period {
                continue;
            }
            last_check = Instant::now();
            let mut ok = self.light_probe().await;
            if !ok {
                tokio::time::sleep(RETRY_DELAY).await;
                ok = self.light_probe().await;
            }
            tracing::info!("watchdog check: {}", if ok { "ok" } else { "FAILED" });
            lock(&self.runtime).watchdog = WatchdogStatus {
                last_check: Some(unix_now()),
                last_ok: Some(ok),
            };
            self.emit();
            if !ok {
                self.notify(
                    "Обход перестал работать",
                    if auto_rescan {
                        "Запускаю автоподбор стратегии."
                    } else {
                        "Запустите автоподбор стратегии на главной странице."
                    },
                );
                if auto_rescan
                    && let Ok(cancel) = self.begin_job(JobKind::Scan, "Подбор стратегии…")
                {
                    self.clone().run_scan(ScanMode::Quick, cancel).await;
                }
            }
        }
    }
}
