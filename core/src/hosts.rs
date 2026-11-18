//! Hosts file update (invariant 6): add only missing upstream lines, always after a backup, never remove anything.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const BLOCK_BEGIN: &str = "# zapret-gui: begin";
pub const BLOCK_END: &str = "# zapret-gui: end";

fn normalize(line: &str) -> String {
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Upstream entry lines (not comments) that the system hosts file doesn't contain yet.
pub fn missing_lines(upstream: &str, system_hosts: &str) -> Vec<String> {
    let present: std::collections::HashSet<String> = system_hosts.lines().map(normalize).collect();
    let mut missing: Vec<String> = Vec::new();
    for line in upstream.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let entry = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if !present.contains(&normalize(trimmed)) && !missing.contains(&entry) {
            missing.push(entry);
        }
    }
    missing
}

/// Copies the hosts file to `hosts-<unix time>.bak` in `backups_dir`.
pub fn backup(hosts_path: &Path, backups_dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(backups_dir)?;
    let mut destination = backups_dir.join(format!("hosts-{}.bak", crate::paths::unix_now()));
    let mut suffix = 1;
    while destination.exists() {
        destination = backups_dir.join(format!("hosts-{}-{suffix}.bak", crate::paths::unix_now()));
        suffix += 1;
    }
    std::fs::copy(hosts_path, &destination)?;
    Ok(destination)
}

/// Appends the lines in a marked block. The caller has made a backup and has the user's confirmation.
pub fn append(hosts_path: &Path, lines: &[String]) -> io::Result<()> {
    let existing = std::fs::read(hosts_path)?;
    let mut file = std::fs::OpenOptions::new().append(true).open(hosts_path)?;
    let mut block = String::new();
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        block.push_str("\r\n");
    }
    block.push_str(BLOCK_BEGIN);
    block.push_str("\r\n");
    for line in lines {
        block.push_str(line);
        block.push_str("\r\n");
    }
    block.push_str(BLOCK_END);
    block.push_str("\r\n");
    file.write_all(block.as_bytes())
}

pub fn latest_backup(backups_dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(backups_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bak"))
        .max_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
        })
}

/// Restores the newest backup and removes it from the backups folder.
pub fn restore_latest(hosts_path: &Path, backups_dir: &Path) -> io::Result<PathBuf> {
    let latest = latest_backup(backups_dir)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "резервных копий нет"))?;
    let content = std::fs::read(&latest)?;
    let temp = hosts_path.with_extension("zapret-gui.tmp");
    std::fs::write(&temp, content)?;
    std::fs::rename(&temp, hosts_path)?;
    std::fs::remove_file(&latest)?;
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_backup_append_restore() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = dir.path().join("hosts");
        let original = "127.0.0.1 localhost\n1.2.3.4   Example.com\n";
        std::fs::write(&hosts, original).unwrap();
        let upstream =
            "# header\n1.2.3.4 example.com\n5.6.7.8 discord.media\n5.6.7.8 discord.media\n\n";
        let missing = missing_lines(upstream, original);
        assert_eq!(missing, vec!["5.6.7.8 discord.media"]);

        let backups = dir.path().join("backups");
        backup(&hosts, &backups).unwrap();
        append(&hosts, &missing).unwrap();
        let updated = std::fs::read_to_string(&hosts).unwrap();
        assert!(updated.starts_with(original), "user entries are kept");
        assert!(updated.contains("5.6.7.8 discord.media"));
        assert!(missing_lines(upstream, &updated).is_empty());

        restore_latest(&hosts, &backups).unwrap();
        assert_eq!(std::fs::read_to_string(&hosts).unwrap(), original);
        assert!(latest_backup(&backups).is_none());
    }
}
