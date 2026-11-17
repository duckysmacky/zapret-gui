//! Auto-strategy (quick / full scan) and strategy tests (PROBING.md).

use std::cmp::Reverse;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::api::*;
use crate::config::result_key;
use crate::engine::{Engine, lock};
use crate::paths::unix_now;
use crate::prober::targets::{DpiTarget, Target, load_targets};
use crate::prober::{probe_dpi, probe_targets};
use crate::upstream::parser::{Strategy, natural_key};

/// Warm-up after winws starts before probing (tune on real machines).
const WARM_UP: Duration = Duration::from_millis(1500);

/// Builds a result from probe outcomes.
pub fn score(
    bundle_version: &str,
    targets: Vec<TargetResult>,
    dpi: Vec<DpiResult>,
    winws_failed: bool,
) -> StrategyResult {
    let critical: Vec<&TargetResult> = targets.iter().filter(|target| target.critical).collect();
    let critical_ok = critical.iter().filter(|target| target.works()).count() as u32;
    let critical_total = critical.len() as u32;
    let dpi_ok: u32 = dpi
        .iter()
        .map(|result| {
            result
                .statuses
                .iter()
                .filter(|status| **status == DpiStatus::Ok)
                .count() as u32
        })
        .sum();
    let pings: Vec<u32> = targets.iter().filter_map(|target| target.ping_ms).collect();
    StrategyResult {
        bundle_version: bundle_version.into(),
        tested_at: unix_now(),
        passed: !winws_failed && critical_total > 0 && critical_ok == critical_total,
        critical_ok,
        critical_total,
        ok_count: targets.iter().map(TargetResult::ok_count).sum::<u32>() + dpi_ok,
        avg_ping_ms: (!pings.is_empty()).then(|| pings.iter().sum::<u32>() / pings.len() as u32),
        winws_failed,
        targets,
        dpi,
    }
}

/// Ranking: passed, then critical OKs, then all OKs, then lower ping.
pub fn rank_key(result: &StrategyResult) -> (bool, u32, u32, Reverse<u32>) {
    (
        result.passed,
        result.critical_ok,
        result.ok_count,
        Reverse(result.avg_ping_ms.unwrap_or(u32::MAX)),
    )
}

/// Candidate order: last known good first, then past success, then natural name order.
pub fn order_candidates(
    strategies: &[Strategy],
    results: &dyn Fn(&str) -> Option<StrategyResult>,
) -> Vec<Strategy> {
    let mut candidates: Vec<Strategy> = strategies
        .iter()
        .filter(|s| s.is_usable())
        .cloned()
        .collect();
    candidates.sort_by_key(|strategy| {
        let result = results(&strategy.name);
        let passed = result.as_ref().is_some_and(|result| result.passed);
        let tested_at = result
            .as_ref()
            .filter(|result| result.passed)
            .map_or(0, |result| result.tested_at);
        let ok = result.as_ref().map_or(0, |result| result.ok_count);
        (
            Reverse(passed),
            Reverse(tested_at),
            Reverse(ok),
            natural_key(&strategy.name),
        )
    });
    candidates
}

struct Previous {
    was_running: bool,
    active: Option<String>,
}

impl Engine {
    fn usable_strategies(&self, names: &[String]) -> Vec<Strategy> {
        lock(&self.bundle)
            .strategies
            .iter()
            .filter(|strategy| {
                strategy.is_usable() && (names.is_empty() || names.contains(&strategy.name))
            })
            .cloned()
            .collect()
    }

    fn previous_state(&self) -> Previous {
        Previous {
            was_running: lock(&self.runtime).run_state == RunState::Running,
            active: lock(&self.config).active_strategy.clone(),
        }
    }

    async fn restore(self: &Arc<Self>, previous: &Previous) {
        self.edit_config(|config| config.active_strategy = previous.active.clone());
        if previous.was_running {
            if let Err(error) = self.start_winws(None, None).await {
                tracing::warn!("cannot restore previous state: {error}");
            }
        } else {
            self.stop_winws().await;
        }
    }

    fn store_result(&self, strategy: &str, result: &StrategyResult) {
        self.edit_config(|config| {
            config
                .strategy_results
                .insert(result_key(&result.bundle_version, strategy), result.clone());
        });
    }

    /// Runs one strategy and probes it. `Err` only for problems that make further candidates pointless.
    async fn test_strategy(
        self: &Arc<Self>,
        strategy: &Strategy,
        targets: &[Target],
        suite: &[DpiTarget],
        ipset_mode: Option<IpsetMode>,
    ) -> Result<StrategyResult, CoreError> {
        let version = lock(&self.config)
            .current_version
            .clone()
            .unwrap_or_default();
        match self.start_winws(Some(&strategy.name), ipset_mode).await {
            Ok(()) => {}
            Err(
                error @ (CoreError::Conflict(_) | CoreError::NotElevated | CoreError::NoBundle),
            ) => return Err(error),
            Err(error) => {
                tracing::warn!("strategy {:?} failed to start: {error}", strategy.name);
                return Ok(score(&version, Vec::new(), Vec::new(), true));
            }
        }
        tokio::time::sleep(WARM_UP).await;
        if !self.winws_alive().await {
            return Ok(score(&version, Vec::new(), Vec::new(), true));
        }
        let target_results = probe_targets(&self.probe, targets).await;
        let dpi_results = if suite.is_empty() {
            Vec::new()
        } else {
            probe_dpi(&self.probe, suite).await
        };
        let failed = !self.winws_alive().await;
        Ok(score(&version, target_results, dpi_results, failed))
    }

    pub(crate) async fn run_scan(self: Arc<Self>, mode: ScanMode, cancel: Arc<AtomicBool>) {
        let previous = self.previous_state();
        let outcome = self.scan(mode, &cancel, &previous).await;
        match outcome {
            Ok(summary) => {
                let body = match (&summary.winner, summary.cancelled) {
                    (_, true) => "Подбор отменён.".to_string(),
                    (Some(winner), _) => format!("Включена стратегия «{winner}»."),
                    (None, _) => "Ни одна стратегия не помогла. Проверьте диагностику, настройте защищённый DNS \
                                  или обновите zapret."
                        .to_string(),
                };
                self.notify("Подбор стратегии завершён", body);
                lock(&self.runtime).last_scan = Some(summary);
            }
            Err(error) => {
                self.restore(&previous).await;
                self.notify("Подбор стратегии", error.to_string());
            }
        }
        self.end_job();
    }

    async fn scan(
        self: &Arc<Self>,
        mode: ScanMode,
        cancel: &AtomicBool,
        previous: &Previous,
    ) -> Result<ScanSummary, CoreError> {
        let version = lock(&self.config)
            .current_version
            .clone()
            .ok_or(CoreError::NoBundle)?;
        let candidates = {
            let config = lock(&self.config).clone();
            order_candidates(&self.usable_strategies(&[]), &|name| {
                config
                    .strategy_results
                    .get(&result_key(&version, name))
                    .cloned()
            })
        };
        let mut targets = load_targets(self.current_bundle_dir().as_deref());
        if mode == ScanMode::Quick {
            targets.retain(|target| target.critical);
        }
        let suite = match mode {
            ScanMode::Full => self.probe.dpi_suite().await.unwrap_or_default(),
            ScanMode::Quick => Vec::new(),
        };
        // The DPI suite hosts are only covered with ipset `any` (upstream does the same, runtime lists only).
        let ipset = (mode == ScanMode::Full).then_some(IpsetMode::Any);
        tracing::info!(
            "scan {mode:?}: {} candidates, {} targets",
            candidates.len(),
            targets.len()
        );

        let mut tested: Vec<(String, StrategyResult)> = Vec::new();
        let mut winner = None;
        let total = candidates.len().max(1);
        for (index, strategy) in candidates.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            self.job_progress(
                format!("Проверка «{}» ({} из {})", strategy.name, index + 1, total),
                Some(index as f32 / total as f32),
            );
            let result = self
                .test_strategy(strategy, &targets, &suite, ipset)
                .await?;
            tracing::info!(
                "strategy {:?}: passed {}, critical {}/{}, ok {}",
                strategy.name,
                result.passed,
                result.critical_ok,
                result.critical_total,
                result.ok_count
            );
            self.store_result(&strategy.name, &result);
            let passed = result.passed;
            tested.push((strategy.name.clone(), result));
            self.emit();
            if mode == ScanMode::Quick && passed {
                winner = Some(strategy.name.clone());
                break;
            }
        }
        let cancelled = cancel.load(Ordering::Relaxed);
        tested.sort_by_key(|(_, result)| Reverse(rank_key(result)));
        if mode == ScanMode::Full && !cancelled {
            winner = tested
                .iter()
                .find(|(_, result)| result.passed)
                .map(|(name, _)| name.clone());
        }
        let ranking: Vec<String> = tested.into_iter().map(|(name, _)| name).collect();
        match (&winner, cancelled) {
            (Some(name), false) => {
                self.edit_config(|config| config.active_strategy = Some(name.clone()));
                self.start_winws(None, None).await?;
            }
            _ => self.restore(previous).await,
        }
        Ok(ScanSummary {
            mode,
            ranking,
            winner: winner.filter(|_| !cancelled),
            cancelled,
        })
    }

    pub(crate) async fn run_tests(
        self: Arc<Self>,
        names: Vec<String>,
        mode: TestMode,
        cancel: Arc<AtomicBool>,
    ) {
        let previous = self.previous_state();
        let strategies = self.usable_strategies(&names);
        let targets = match mode {
            TestMode::Standard => load_targets(self.current_bundle_dir().as_deref()),
            TestMode::Dpi => Vec::new(),
        };
        let suite = match mode {
            TestMode::Dpi => self.probe.dpi_suite().await.unwrap_or_default(),
            TestMode::Standard => Vec::new(),
        };
        if mode == TestMode::Dpi && suite.is_empty() {
            self.notify("Проверка", "Не удалось загрузить список DPI-проверок.");
            self.end_job();
            return;
        }
        let ipset = (mode == TestMode::Dpi).then_some(IpsetMode::Any);
        let mut entries = Vec::new();
        let total = strategies.len().max(1);
        let mut aborted = None;
        for (index, strategy) in strategies.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            self.job_progress(
                format!("Проверка «{}» ({} из {})", strategy.name, index + 1, total),
                Some(index as f32 / total as f32),
            );
            match self.test_strategy(strategy, &targets, &suite, ipset).await {
                Ok(result) => {
                    if mode == TestMode::Standard {
                        self.store_result(&strategy.name, &result);
                    }
                    entries.push(TestEntry {
                        strategy: strategy.name.clone(),
                        result,
                    });
                    lock(&self.runtime).last_test = Some(TestReport {
                        mode,
                        entries: entries.clone(),
                        cancelled: false,
                    });
                }
                Err(error) => {
                    aborted = Some(error);
                    break;
                }
            }
        }
        self.restore(&previous).await;
        lock(&self.runtime).last_test = Some(TestReport {
            mode,
            entries,
            cancelled: cancel.load(Ordering::Relaxed),
        });
        match aborted {
            Some(error) => self.notify("Проверка", error.to_string()),
            None => self.notify(
                "Проверка завершена",
                "Результаты на странице «Диагностика».",
            ),
        }
        self.end_job();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(critical: bool, ok: bool, ping: Option<u32>) -> TargetResult {
        let outcome = if ok {
            ProbeOutcome::Ok
        } else {
            ProbeOutcome::Error
        };
        TargetResult {
            name: "t".into(),
            value: "https://t".into(),
            critical,
            http: Some([outcome, outcome, ProbeOutcome::Unsupported]),
            ping_ms: ping,
        }
    }

    #[test]
    fn scoring_and_ranking() {
        let good = score(
            "1",
            vec![target(true, true, Some(30)), target(false, true, None)],
            vec![],
            false,
        );
        assert!(good.passed);
        assert_eq!(good.critical_ok, 1);
        assert_eq!(good.ok_count, 5);
        let partial = score(
            "1",
            vec![target(true, false, Some(10)), target(false, true, None)],
            vec![],
            false,
        );
        assert!(!partial.passed);
        let crashed = score("1", vec![], vec![], true);
        assert!(!crashed.passed);
        let mut results = vec![partial.clone(), crashed.clone(), good.clone()];
        results.sort_by_key(|result| Reverse(rank_key(result)));
        assert_eq!(results, vec![good, partial, crashed]);
    }

    #[test]
    fn candidate_order() {
        let strategy = |name: &str| Strategy {
            name: name.into(),
            args: vec!["--x".into()],
            parse_error: None,
        };
        let broken = Strategy {
            parse_error: Some("bad".into()),
            ..strategy("broken")
        };
        let strategies = vec![
            strategy("b (ALT10)"),
            strategy("b (ALT2)"),
            strategy("a"),
            broken,
        ];
        let good = score("1", vec![target(true, true, None)], vec![], false);
        let lookup = move |name: &str| (name == "b (ALT10)").then(|| good.clone());
        let order: Vec<String> = order_candidates(&strategies, &lookup)
            .into_iter()
            .map(|strategy| strategy.name)
            .collect();
        assert_eq!(order, vec!["b (ALT10)", "a", "b (ALT2)"]);
    }
}
