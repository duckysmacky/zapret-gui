//! Core API flows on the mock platform: commands produce the expected state.
#![cfg(any(not(windows), feature = "mock-platform"))]

use std::sync::Arc;
use std::time::Duration;

use zapret_core::logging::LogBuffer;
use zapret_core::platform::mock::{Scenario, create_with};
use zapret_core::upstream::bundle::{BundleMeta, META_FILE};
use zapret_core::*;

const STRATEGIES: &[&str] = &[
    "general",
    "general (ALT)",
    "general (ALT2)",
    "general (ALT3)",
    "general (ALT4)",
    "general (ALT5)",
    "general (ALT6)",
    "general (ALT7)",
];

fn fake_bundle(paths: &Paths) {
    let root = paths.bundle_dir("1.0");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lists")).unwrap();
    std::fs::write(root.join("bin/winws.exe"), "").unwrap();
    std::fs::write(root.join("bin/WinDivert64.sys"), "").unwrap();
    std::fs::write(root.join("lists/list-general.txt"), "discord.com\n").unwrap();
    std::fs::write(root.join("lists/ipset-all.txt"), "1.1.1.0/24\n").unwrap();
    for name in STRATEGIES {
        std::fs::write(
            root.join(format!("{name}.bat")),
            "start \"\" \"%BIN%winws.exe\" --wf-tcp=80,%GameFilterTCP% --hostlist=\"%LISTS%list-general.txt\"\r\n",
        )
        .unwrap();
    }
    std::fs::write(
        root.join("broken.bat"),
        "start \"\" \"%BIN%winws.exe\" --x=%NOPE%\r\n",
    )
    .unwrap();
    let meta = BundleMeta {
        version: "1.0".into(),
        sha256: "00".into(),
        installed_at: 0,
    };
    std::fs::write(root.join(META_FILE), serde_json::to_string(&meta).unwrap()).unwrap();
}

async fn engine(dir: &std::path::Path, scenario: Scenario) -> Arc<Engine> {
    let paths = Paths::under(dir);
    fake_bundle(&paths);
    let platform = create_with(
        &paths,
        Scenario {
            download_fails: true,
            ..scenario
        },
    );
    let (events, _) = tokio::sync::broadcast::channel(256);
    let engine = Arc::new(Engine::new(
        paths,
        platform,
        Arc::new(LogBuffer::default()),
        events,
    ));
    engine.startup().await;
    engine
}

async fn wait_until(
    engine: &Engine,
    what: &str,
    condition: impl Fn(&AppState) -> bool,
) -> AppState {
    for _ in 0..600 {
        let state = engine.snapshot();
        if condition(&state) {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}: {:#?}", engine.snapshot().job);
}

#[tokio::test(flavor = "multi_thread")]
async fn enable_scan_and_settings() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), Scenario::default()).await;
    let state = engine.snapshot();
    assert_eq!(state.current_version.as_deref(), Some("1.0"));
    assert_eq!(state.strategies.len(), STRATEGIES.len() + 1);
    assert!(
        state
            .strategies
            .iter()
            .any(|s| s.name == "broken" && s.parse_error.is_some())
    );
    assert!(state.first_run);

    assert_eq!(
        engine.handle(Command::Enable).await,
        Err(CoreError::NoStrategy)
    );
    assert!(
        engine
            .handle(Command::SetActiveStrategy {
                strategy_name: "broken".into()
            })
            .await
            .is_err()
    );

    engine
        .handle(Command::RunScan {
            scan_mode: ScanMode::Quick,
        })
        .await
        .unwrap();
    assert_eq!(engine.handle(Command::Enable).await, Err(CoreError::Busy));
    let state = wait_until(&engine, "scan end", |state| state.job.is_none()).await;
    let scan = state.last_scan.expect("scan summary");
    let winner = scan.winner.expect("a mock strategy works");
    assert_eq!(state.active_strategy.as_ref(), Some(&winner));
    assert_eq!(state.run_state, RunState::Running);
    let result = state
        .strategies
        .iter()
        .find(|s| s.name == winner)
        .unwrap()
        .result
        .clone()
        .unwrap();
    assert!(result.passed);

    let reply = engine
        .handle(Command::UpdateUserList {
            list_kind: UserListKind::General,
            entries: vec!["example.com".into(), "bad entry".into()],
        })
        .await
        .unwrap();
    assert_eq!(reply, Reply::Rejected(vec!["bad entry".into()]));
    engine.handle(Command::ToggleGameFilter).await.unwrap();
    let state = engine.snapshot();
    assert_eq!(state.game_filter, GameFilterMode::All);
    assert_eq!(state.user_lists.general, vec!["example.com"]);
    assert_eq!(state.run_state, RunState::Running);
    let lists = engine.snapshot();
    assert!(
        lists
            .upstream_list_files
            .contains(&"list-general.txt".to_string())
    );
    let runtime_user_list = dir.path().join("data/runtime/lists/list-general-user.txt");
    assert_eq!(
        std::fs::read_to_string(runtime_user_list).unwrap(),
        "example.com\n"
    );

    assert!(
        engine
            .handle(Command::SetGameFilterPorts {
                tcp_ports: "70000".into(),
                udp_ports: "1".into()
            })
            .await
            .is_err()
    );

    engine.handle(Command::Disable).await.unwrap();
    assert_eq!(engine.snapshot().run_state, RunState::Stopped);

    engine.handle(Command::RunDiagnostics).await.unwrap();
    let state = wait_until(&engine, "diagnostics", |state| state.job.is_none()).await;
    assert_eq!(state.checks.len(), 17);
    let report = match engine.handle(Command::GetDiagnosticsReport).await.unwrap() {
        Reply::Text(text) => text,
        other => panic!("{other:?}"),
    };
    assert!(report.contains("Проверки"));
    assert!(
        !report.contains("example.com"),
        "no list contents in the report"
    );

    engine.handle(Command::Shutdown).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_works_restores_previous_state() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(
        dir.path(),
        Scenario {
            nothing_works: true,
            ..Default::default()
        },
    )
    .await;
    engine
        .handle(Command::SetActiveStrategy {
            strategy_name: "general".into(),
        })
        .await
        .unwrap();
    engine
        .handle(Command::RunScan {
            scan_mode: ScanMode::Quick,
        })
        .await
        .unwrap();
    let state = wait_until(&engine, "scan end", |state| state.job.is_none()).await;
    assert_eq!(state.last_scan.unwrap().winner, None);
    assert_eq!(state.active_strategy.as_deref(), Some("general"));
    assert_eq!(state.run_state, RunState::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_restores_previous_state() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(
        dir.path(),
        Scenario {
            nothing_works: true,
            ..Default::default()
        },
    )
    .await;
    engine
        .handle(Command::SetActiveStrategy {
            strategy_name: "general (ALT)".into(),
        })
        .await
        .unwrap();
    engine.handle(Command::Enable).await.unwrap();
    engine
        .handle(Command::RunScan {
            scan_mode: ScanMode::Full,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    engine.handle(Command::CancelJob).await.unwrap();
    let state = wait_until(&engine, "scan end", |state| state.job.is_none()).await;
    assert!(state.last_scan.unwrap().cancelled);
    assert_eq!(state.active_strategy.as_deref(), Some("general (ALT)"));
    assert_eq!(state.run_state, RunState::Running);
}

#[tokio::test(flavor = "multi_thread")]
async fn winws_exit_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(
        dir.path(),
        Scenario {
            winws_exits: true,
            ..Default::default()
        },
    )
    .await;
    engine
        .handle(Command::SetActiveStrategy {
            strategy_name: "general".into(),
        })
        .await
        .unwrap();
    engine.handle(Command::Enable).await.unwrap();
    assert_eq!(engine.snapshot().run_state, RunState::Running);
    let state = wait_until(&engine, "error state", |state| {
        state.run_state == RunState::Error
    })
    .await;
    let error = state.error.unwrap();
    assert!(error.message.contains("winws"));
    assert!(
        error
            .output_tail
            .iter()
            .any(|line| line.contains("simulated failure"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn not_elevated_blocks_admin_actions() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(
        dir.path(),
        Scenario {
            not_elevated: true,
            ..Default::default()
        },
    )
    .await;
    assert!(!engine.snapshot().elevated);
    engine
        .handle(Command::SetActiveStrategy {
            strategy_name: "general".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        engine.handle(Command::Enable).await,
        Err(CoreError::NotElevated)
    );
    assert_eq!(
        engine
            .handle(Command::RunScan {
                scan_mode: ScanMode::Quick
            })
            .await,
        Err(CoreError::NotElevated)
    );
}
