//! The core API (`CoreClient`): the only interface the ui crate uses.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::api::{Command, CoreError, CoreEvent, Reply};
use crate::engine::Engine;

#[async_trait::async_trait]
pub trait CoreClient: Send + Sync {
    async fn request(&self, command: Command) -> Result<Reply, CoreError>;
    fn subscribe(&self) -> broadcast::Receiver<CoreEvent>;
}

/// v1's single implementation: calls straight into the engine (D3).
pub struct InProcessClient {
    engine: Arc<Engine>,
}

impl InProcessClient {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }
}

#[async_trait::async_trait]
impl CoreClient for InProcessClient {
    async fn request(&self, command: Command) -> Result<Reply, CoreError> {
        let name = format!("{command:?}");
        let result = self.engine.handle(command).await;
        if let Err(error) = &result {
            tracing::warn!(
                "command {} failed: {error}",
                name.chars().take(120).collect::<String>()
            );
        }
        result
    }

    fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.engine.subscribe()
    }
}
