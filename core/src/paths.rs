//! File locations. Config + logs are per-user; bundles and runtime files are admin-only (invariant 3).

use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "zapret-gui";

#[derive(Debug, Clone)]
pub struct Paths {
    /// `%APPDATA%\zapret-gui` (Linux: `$XDG_CONFIG_HOME/zapret-gui`).
    pub config_dir: PathBuf,
    /// `%ProgramData%\zapret-gui` (Linux: `$XDG_DATA_HOME/zapret-gui`). Admin-only-writable on Windows.
    pub data_dir: PathBuf,
}

impl Paths {
    /// Locations from the environment.
    pub fn from_env() -> Self {
        Self {
            config_dir: config_base().join(APP_NAME),
            data_dir: data_base().join(APP_NAME),
        }
    }

    /// Everything under one root (tests).
    pub fn under(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.config_dir.join("logs")
    }

    pub fn bundles_dir(&self) -> PathBuf {
        self.data_dir.join("bundles")
    }

    pub fn bundle_dir(&self, version_tag: &str) -> PathBuf {
        self.bundles_dir().join(version_tag)
    }

    pub fn runtime_lists_dir(&self) -> PathBuf {
        self.data_dir.join("runtime").join("lists")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache")
    }

    /// Downloaded `ipset-service.txt` ("Update IPSet list"). Used as the base `ipset-all.txt` when present.
    pub fn ipset_override_file(&self) -> PathBuf {
        self.data_dir.join("ipset-service.txt")
    }

    pub fn hosts_backups_dir(&self) -> PathBuf {
        self.data_dir.join("hosts-backups")
    }
}

#[cfg(windows)]
fn config_base() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\Default\AppData\Roaming"))
}

#[cfg(windows)]
fn data_base() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

#[cfg(not(windows))]
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(not(windows))]
fn config_base() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

#[cfg(not(windows))]
fn data_base() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local").join("share"))
}

/// A directory path with a trailing separator, as upstream's `%BIN%` / `%LISTS%` expect.
pub fn with_trailing_separator(dir: &Path) -> String {
    let mut text = dir.display().to_string();
    if !text.ends_with(std::path::MAIN_SEPARATOR) {
        text.push(std::path::MAIN_SEPARATOR);
    }
    text
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
