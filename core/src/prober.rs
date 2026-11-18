//! The prober: standard checks (HTTP/TLS + ping) and DPI-freeze checks. Shared by scans, tests and the watchdog.

pub mod http;
pub mod targets;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::api::{DpiResult, DpiStatus, ProbeOutcome, TargetResult};
use crate::platform::System;
use targets::{DpiTarget, Target, TargetValue};

pub const DPI_SUITE_URL: &str =
    "https://hyperion-cs.github.io/dpi-checkers/ru/tcp-16-20/suite.v2.json";
const PING_TIMEOUT: Duration = Duration::from_millis(1000);

#[async_trait::async_trait]
pub trait ProbeBackend: Send + Sync {
    /// HTTP/1.1, TLS 1.2, TLS 1.3 HEAD checks of one URL.
    async fn http(&self, url: &str) -> [ProbeOutcome; 3];
    /// DPI-checker POSTs to `https://<host>` in the three variants.
    async fn dpi(&self, host: &str) -> [DpiStatus; 3];
    async fn ping(&self, host: &str) -> Option<u32>;
    /// The DPI suite (fresh, else the last cached copy).
    async fn dpi_suite(&self) -> Option<Vec<DpiTarget>>;
}

/// Upstream's parallelism cap: `min(16, max(8, cores × 2))`.
pub fn concurrency() -> usize {
    let cores = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4);
    (cores * 2).clamp(8, 16)
}

/// Probes all targets in parallel (capped). Results keep the target order.
pub async fn probe_targets(
    backend: &Arc<dyn ProbeBackend>,
    targets: &[Target],
) -> Vec<TargetResult> {
    let permits = Arc::new(Semaphore::new(concurrency()));
    let mut tasks = JoinSet::new();
    for (index, target) in targets.iter().cloned().enumerate() {
        let backend = backend.clone();
        let permits = permits.clone();
        tasks.spawn(async move {
            let _permit = permits.acquire_owned().await;
            let (http, ping_host) = match &target.value {
                TargetValue::Url(url) => {
                    let host = reqwest::Url::parse(url)
                        .ok()
                        .and_then(|parsed| parsed.host_str().map(str::to_string));
                    (Some(backend.http(url).await), host)
                }
                TargetValue::Ping(host) => (None, Some(host.clone())),
            };
            let ping_ms = match ping_host {
                Some(host) => backend.ping(&host).await,
                None => None,
            };
            let result = TargetResult {
                name: target.name.clone(),
                value: target.value_text(),
                critical: target.critical,
                http,
                ping_ms,
            };
            (index, result)
        });
    }
    collect_ordered(tasks).await
}

pub async fn probe_dpi(backend: &Arc<dyn ProbeBackend>, suite: &[DpiTarget]) -> Vec<DpiResult> {
    let permits = Arc::new(Semaphore::new(concurrency()));
    let mut tasks = JoinSet::new();
    for (index, target) in suite.iter().cloned().enumerate() {
        let backend = backend.clone();
        let permits = permits.clone();
        tasks.spawn(async move {
            let _permit = permits.acquire_owned().await;
            let statuses = backend.dpi(&target.host).await;
            (
                index,
                DpiResult {
                    id: target.id,
                    provider: target.provider,
                    host: target.host,
                    statuses,
                },
            )
        });
    }
    collect_ordered(tasks).await
}

async fn collect_ordered<T: 'static>(mut tasks: JoinSet<(usize, T)>) -> Vec<T> {
    let mut results = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(pair) => results.push(pair),
            Err(error) => tracing::warn!("probe task failed: {error}"),
        }
    }
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, value)| value).collect()
}

/// The real backend: reqwest + the platform's ICMP ping.
pub struct RealProbe {
    standard: http::VariantClients,
    dpi: http::VariantClients,
    suite_client: reqwest::Client,
    suite_cache: PathBuf,
    system: Arc<dyn System>,
}

impl RealProbe {
    pub fn new(system: Arc<dyn System>, suite_cache: PathBuf) -> Self {
        Self {
            standard: http::variant_clients(http::STANDARD_TIMEOUT, http::STANDARD_CONNECT_TIMEOUT),
            dpi: http::variant_clients(http::DPI_TIMEOUT, http::DPI_CONNECT_TIMEOUT),
            suite_client: reqwest::Client::builder()
                .timeout(http::DPI_TIMEOUT)
                .build()
                .unwrap_or_default(),
            suite_cache,
            system,
        }
    }

    async fn fetch_suite(&self) -> Option<String> {
        let response = self.suite_client.get(DPI_SUITE_URL).send().await.ok()?;
        response.error_for_status().ok()?.text().await.ok()
    }
}

#[async_trait::async_trait]
impl ProbeBackend for RealProbe {
    async fn http(&self, url: &str) -> [ProbeOutcome; 3] {
        let (first, second, third) = tokio::join!(
            http::head(self.standard[0].as_ref(), url),
            http::head(self.standard[1].as_ref(), url),
            http::head(self.standard[2].as_ref(), url),
        );
        [first, second, third]
    }

    async fn dpi(&self, host: &str) -> [DpiStatus; 3] {
        let url = format!("https://{host}/");
        let (first, second, third) = tokio::join!(
            http::dpi_post(self.dpi[0].as_ref(), &url),
            http::dpi_post(self.dpi[1].as_ref(), &url),
            http::dpi_post(self.dpi[2].as_ref(), &url),
        );
        [first, second, third]
    }

    async fn ping(&self, host: &str) -> Option<u32> {
        let system = self.system.clone();
        let host = host.to_string();
        tokio::task::spawn_blocking(move || system.ping(&host, PING_TIMEOUT))
            .await
            .ok()
            .flatten()
    }

    async fn dpi_suite(&self) -> Option<Vec<DpiTarget>> {
        if let Some(text) = self.fetch_suite().await
            && let Some(suite) = targets::parse_dpi_suite(&text)
        {
            if let Some(parent) = self.suite_cache.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&self.suite_cache, &text);
            return Some(suite);
        }
        tracing::warn!("DPI suite fetch failed; using the cached copy");
        let cached = std::fs::read_to_string(&self.suite_cache).ok()?;
        targets::parse_dpi_suite(&cached)
    }
}
