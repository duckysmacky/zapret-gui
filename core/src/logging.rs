//! `tracing` setup: rotating log files + an in-memory tail that feeds the UI log view and the diagnostics report.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

use crate::api::CoreEvent;

const TAIL_LINES: usize = 1000;

static FILE_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

/// The last log lines, newest last.
#[derive(Default)]
pub struct LogBuffer {
    lines: Mutex<VecDeque<String>>,
    events: OnceLock<broadcast::Sender<CoreEvent>>,
}

impl LogBuffer {
    pub fn push(&self, line: String) {
        if let Some(events) = self.events.get() {
            let _ = events.send(CoreEvent::LogLine(line.clone()));
        }
        let mut lines = self
            .lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if lines.len() == TAIL_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn tail(&self, count: usize) -> Vec<String> {
        let lines = self
            .lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        lines
            .iter()
            .skip(lines.len().saturating_sub(count))
            .cloned()
            .collect()
    }

    /// Live log lines are broadcast as `CoreEvent::LogLine` once this is set.
    pub fn attach_events(&self, events: broadcast::Sender<CoreEvent>) {
        let _ = self.events.set(events);
    }
}

/// Installs the global subscriber. Call once per process.
pub fn init(logs_dir: &Path, buffer: Arc<LogBuffer>) -> std::io::Result<()> {
    std::fs::create_dir_all(logs_dir)?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("zapret-gui")
        .filename_suffix("log")
        .max_log_files(7)
        .build(logs_dir)
        .map_err(std::io::Error::other)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let _ = FILE_GUARD.set(guard);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer)
        .with_filter(LevelFilter::INFO);
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(LevelFilter::INFO);
    let tail_layer = TailLayer { buffer }.with_filter(LevelFilter::INFO);
    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .with(tail_layer)
        .try_init()
        .map_err(std::io::Error::other)
}

struct TailLayer {
    buffer: Arc<LogBuffer>,
}

impl<S: Subscriber> Layer<S> for TailLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let metadata = event.metadata();
        self.buffer.push(format!(
            "{} {} {}: {}",
            utc_clock(crate::paths::unix_now()),
            metadata.level(),
            metadata.target(),
            visitor.text
        ));
    }
}

#[derive(Default)]
struct MessageVisitor {
    text: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.text, "{value:?}");
        } else {
            let _ = write!(self.text, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.text.push_str(value);
        } else {
            let _ = write!(self.text, " {}={value}", field.name());
        }
    }
}

/// `HH:MM:SS` (UTC) of a unix timestamp.
pub fn utc_clock(unix_seconds: u64) -> String {
    let day_seconds = unix_seconds % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        day_seconds / 3600,
        day_seconds % 3600 / 60,
        day_seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_newest() {
        let buffer = LogBuffer::default();
        for index in 0..(TAIL_LINES + 5) {
            buffer.push(index.to_string());
        }
        let tail = buffer.tail(2);
        assert_eq!(
            tail,
            vec![(TAIL_LINES + 3).to_string(), (TAIL_LINES + 4).to_string()]
        );
        assert_eq!(utc_clock(3661), "01:01:01");
    }
}
