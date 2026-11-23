//! zapret-gui core: all logic behind the `CoreClient` API. No UI code here.

pub mod api;
pub mod client;
pub mod config;
pub mod diagnostics;
mod engine;
pub mod hosts;
pub mod lists;
pub mod logging;
pub mod paths;
pub mod platform;
pub mod prober;
pub mod report;
pub mod runner;
mod scanner;
pub mod uliana;
pub mod upstream;
mod watchdog;

use std::sync::Arc;

pub use api::*;
pub use client::{CoreClient, InProcessClient};
pub use engine::Engine;
pub use paths::Paths;

/// A running core: its tokio runtime and the client the UI talks to.
pub struct CoreHandle {
    pub client: Arc<dyn CoreClient>,
    runtime: tokio::runtime::Runtime,
}

impl CoreHandle {
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Runs `Shutdown` (kills winws) and waits for it, then drops the runtime.
    pub fn shutdown(self) {
        let client = self.client.clone();
        let _ = self.runtime.block_on(client.request(Command::Shutdown));
        self.runtime
            .shutdown_timeout(std::time::Duration::from_secs(2));
    }
}

/// Starts logging, the platform and the engine on a new multi-threaded runtime.
pub fn start(paths: Paths) -> std::io::Result<CoreHandle> {
    let log = Arc::new(logging::LogBuffer::default());
    if let Err(error) = logging::init(&paths.logs_dir(), log.clone()) {
        eprintln!("logging unavailable: {error}");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("zapret-core")
        .build()?;
    let (events, _) = tokio::sync::broadcast::channel(1024);
    log.attach_events(events.clone());
    let engine = runtime.block_on(async {
        let platform = platform::create(&paths);
        let engine = Arc::new(Engine::new(paths, platform, log, events));
        engine.startup().await;
        engine
    });
    Ok(CoreHandle {
        client: Arc::new(InProcessClient::new(engine)),
        runtime,
    })
}
