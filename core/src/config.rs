//! Persistent settings and state (`config.json` in the config dir, D8).

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::api::{GameFilterMode, IpsetMode, Settings, StrategyResult, UserLists};

pub const CONFIG_VERSION: u32 = 1;
pub const DEFAULT_GAME_PORTS: &str = "1024-65535";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub config_version: u32,
    /// Bundle version in use.
    pub current_version: Option<String>,
    /// The user picked an older version: update checks don't switch away from it (D26).
    pub pinned: bool,
    pub active_strategy: Option<String>,
    pub game_filter: GameFilterMode,
    /// Last non-off mode, restored by the tray toggle.
    pub last_game_filter: GameFilterMode,
    pub game_tcp_ports: String,
    pub game_udp_ports: String,
    pub ipset_mode: IpsetMode,
    /// Chosen fake per slot (file name without `.bin`); `None` = the bundle's own `ACTIVE_*` file.
    pub fake_discord_udp: Option<String>,
    pub fake_game_udp: Option<String>,
    pub user_lists: UserLists,
    pub settings: Settings,
    /// Key: `<bundle version>/<strategy name>`.
    pub strategy_results: BTreeMap<String, StrategyResult>,
    pub first_run_done: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            current_version: None,
            pinned: false,
            active_strategy: None,
            game_filter: GameFilterMode::Off,
            last_game_filter: GameFilterMode::All,
            game_tcp_ports: DEFAULT_GAME_PORTS.into(),
            game_udp_ports: DEFAULT_GAME_PORTS.into(),
            ipset_mode: IpsetMode::Loaded,
            fake_discord_udp: None,
            fake_game_udp: None,
            user_lists: UserLists::default(),
            settings: Settings::default(),
            strategy_results: BTreeMap::new(),
            first_run_done: false,
        }
    }
}

pub fn result_key(bundle_version: &str, strategy_name: &str) -> String {
    format!("{bundle_version}/{strategy_name}")
}

impl Config {
    /// Loads the config. A missing file gives defaults; a corrupt one is moved aside and defaults are used.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                tracing::warn!("cannot read config {}: {error}", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str::<Config>(&text) {
            Ok(config) => config.migrate(),
            Err(error) => {
                tracing::warn!("corrupt config {}: {error}; using defaults", path.display());
                let _ = std::fs::rename(path, path.with_extension("json.corrupt"));
                Self::default()
            }
        }
    }

    fn migrate(mut self) -> Self {
        // No older format exists yet; future migrations go here, keyed on config_version.
        if self.config_version > CONFIG_VERSION {
            tracing::warn!(
                "config version {} is newer than supported {CONFIG_VERSION}",
                self.config_version
            );
        }
        self.config_version = CONFIG_VERSION;
        self
    }

    /// Atomic save (write temp + rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let temp_path = path.with_extension("json.tmp");
        std::fs::write(&temp_path, text)?;
        std::fs::rename(&temp_path, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(Config::load(&path), Config::default());

        let mut config = Config {
            active_strategy: Some("general (ALT)".into()),
            ..Default::default()
        };
        config.user_lists.general.push("example.com".into());
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path), config);
    }

    #[test]
    fn partial_and_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"config_version":1,"pinned":true}"#).unwrap();
        let config = Config::load(&path);
        assert!(config.pinned);
        assert_eq!(config.game_tcp_ports, DEFAULT_GAME_PORTS);

        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Config::load(&path), Config::default());
        assert!(path.with_extension("json.corrupt").exists());
    }
}
