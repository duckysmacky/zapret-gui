//! Installed bundles: `<data>/bundles/<version>/` with `meta.json`. Never modified after install.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::parser::{Strategy, natural_key, parse_strategy};

pub const META_FILE: &str = "zapret-gui-meta.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleMeta {
    pub version: String,
    pub sha256: String,
    pub installed_at: u64,
}

/// The folder holding `bin/winws.exe`: the staging dir itself or its single top-level folder
/// (1.10.x archives have a `zapret-discord-youtube-<version>/` folder, 1.9.x ones are flat).
pub fn find_root(staging: &Path) -> Option<PathBuf> {
    if staging.join("bin").join("winws.exe").is_file() {
        return Some(staging.to_path_buf());
    }
    let subdirs: Vec<PathBuf> = std::fs::read_dir(staging)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    match subdirs.as_slice() {
        [single] if single.join("bin").join("winws.exe").is_file() => Some(single.clone()),
        _ => None,
    }
}

fn files_with_extension(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
        })
        .collect()
}

pub fn has_windivert_driver(root: &Path) -> bool {
    !files_with_extension(&root.join("bin"), "sys").is_empty()
}

/// Strategy files: every `*.bat` in the root not starting with `service`, natural order.
pub fn strategy_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = files_with_extension(root, "bat")
        .into_iter()
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().to_string();
            (!name.to_lowercase().starts_with("service")).then_some((name, path))
        })
        .collect();
    files.sort_by_key(|(name, _)| natural_key(name));
    files
}

pub fn load_strategies(root: &Path) -> Vec<Strategy> {
    strategy_files(root)
        .into_iter()
        .map(|(name, path)| match std::fs::read(&path) {
            Ok(bytes) => parse_strategy(&name, &String::from_utf8_lossy(&bytes)),
            Err(error) => Strategy {
                name,
                args: Vec::new(),
                parse_error: Some(format!("не удалось прочитать файл: {error}")),
            },
        })
        .collect()
}

/// Structure check after extraction (UPSTREAM.md "Verify").
pub fn verify_structure(root: &Path) -> Result<(), String> {
    if !root.join("bin").join("winws.exe").is_file() {
        return Err("нет bin\\winws.exe".into());
    }
    if !has_windivert_driver(root) {
        return Err("нет драйвера WinDivert (.sys) в bin".into());
    }
    if !root.join("lists").is_dir() {
        return Err("нет папки lists".into());
    }
    if strategy_files(root).is_empty() {
        return Err("нет ни одной стратегии (.bat)".into());
    }
    Ok(())
}

pub fn read_meta(bundle_dir: &Path) -> Option<BundleMeta> {
    serde_json::from_str(&std::fs::read_to_string(bundle_dir.join(META_FILE)).ok()?).ok()
}

/// Installed versions (folders with valid metadata).
pub fn installed_versions(bundles_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(bundles_dir) else {
        return Vec::new();
    };
    let mut versions: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| read_meta(&entry.path()).map(|meta| meta.version))
        .collect();
    versions.sort_by_key(|version| std::cmp::Reverse(natural_key(version)));
    versions
}

/// Fake payload names (without `.bin`), or `None` if this bundle has no `ACTIVE_*` fakes.
pub fn fakes(root: &Path) -> Option<Vec<String>> {
    let bin = root.join("bin");
    if !bin.join("ACTIVE_DISCORD_UDP.bin").is_file() && !bin.join("ACTIVE_GAME_UDP.bin").is_file() {
        return None;
    }
    let mut names: Vec<String> = files_with_extension(&bin, "bin")
        .iter()
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .filter(|name| !name.starts_with("ACTIVE_"))
        .collect();
    names.sort();
    Some(names)
}

/// `lists/*.txt` of the bundle.
pub fn list_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = files_with_extension(&root.join("lists"), "txt")
        .iter()
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn fake_bundle(root: &Path) {
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("lists")).unwrap();
        std::fs::write(root.join("bin/winws.exe"), "").unwrap();
        std::fs::write(root.join("bin/WinDivert64.sys"), "").unwrap();
        std::fs::write(root.join("service.bat"), "").unwrap();
        std::fs::write(
            root.join("general (ALT2).bat"),
            "start \"\" \"%BIN%winws.exe\" --a\r\n",
        )
        .unwrap();
        std::fs::write(
            root.join("general.bat"),
            "start \"\" \"%BIN%winws.exe\" --b\r\n",
        )
        .unwrap();
    }

    #[test]
    fn root_detection_and_verification() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("zapret-discord-youtube-1.0");
        fake_bundle(&nested);
        assert_eq!(find_root(dir.path()), Some(nested.clone()));
        assert_eq!(verify_structure(&nested), Ok(()));
        let names: Vec<String> = strategy_files(&nested)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, vec!["general", "general (ALT2)"]);
        assert_eq!(fakes(&nested), None);
        std::fs::write(nested.join("bin/ACTIVE_GAME_UDP.bin"), "").unwrap();
        std::fs::write(nested.join("bin/stun.bin"), "").unwrap();
        assert_eq!(fakes(&nested), Some(vec!["stun".to_string()]));
    }
}
