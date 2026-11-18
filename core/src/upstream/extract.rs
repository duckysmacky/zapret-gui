//! Safe zip extraction (invariant 2): reject traversal, absolute paths, drive letters and links;
//! extract only into a fresh staging directory.

use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::UpstreamError;

/// Upper bound on extracted bytes (upstream releases are ~5 MB unpacked).
const MAX_EXTRACTED_BYTES: u64 = 512 * 1024 * 1024;

/// Validates an archive entry name and returns its relative path.
pub fn safe_entry_path(raw_name: &str) -> Result<PathBuf, String> {
    let reject = |why: &str| Err(format!("недопустимый путь в архиве {raw_name:?}: {why}"));
    if raw_name.is_empty() || raw_name.contains('\0') {
        return reject("пустое имя");
    }
    let normalized = raw_name.replace('\\', "/");
    if normalized.starts_with('/') {
        return reject("абсолютный путь");
    }
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        return reject("буква диска");
    }
    let mut relative = PathBuf::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => return reject("выход за пределы папки"),
            part if part.contains(':') => return reject("недопустимый символ"),
            part => relative.push(part),
        }
    }
    if relative.as_os_str().is_empty() {
        return reject("пустое имя");
    }
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return reject("недопустимый путь");
    }
    Ok(relative)
}

/// Extracts `archive` into `staging`, which must not exist yet.
pub fn extract_zip(archive: &Path, staging: &Path) -> Result<(), UpstreamError> {
    if staging.exists() {
        return Err(UpstreamError::InvalidArchive(
            "папка распаковки уже существует".into(),
        ));
    }
    std::fs::create_dir_all(staging)?;
    let mut zip = zip::ZipArchive::new(File::open(archive)?)
        .map_err(|error| UpstreamError::InvalidArchive(error.to_string()))?;
    let mut total: u64 = 0;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| UpstreamError::InvalidArchive(error.to_string()))?;
        let name = entry
            .name()
            .map_err(|error| UpstreamError::InvalidArchive(error.to_string()))?
            .to_string();
        let relative = safe_entry_path(&name).map_err(UpstreamError::InvalidArchive)?;
        if entry.is_symlink() {
            return Err(UpstreamError::InvalidArchive(format!(
                "ссылка в архиве: {}",
                name
            )));
        }
        if entry.enclosed_name().is_none() {
            return Err(UpstreamError::InvalidArchive(format!(
                "недопустимый путь: {}",
                name
            )));
        }
        let destination = staging.join(&relative);
        if !destination.starts_with(staging) {
            return Err(UpstreamError::InvalidArchive(format!(
                "недопустимый путь: {}",
                name
            )));
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&destination)?;
            continue;
        }
        total += entry.size();
        if total > MAX_EXTRACTED_BYTES {
            return Err(UpstreamError::InvalidArchive(
                "архив слишком большой".into(),
            ));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = File::create_new(&destination)?;
        io::copy(&mut entry, &mut output)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_zip(dir: &Path, entries: &[&str]) -> PathBuf {
        let path = dir.join("test.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        for name in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"data").unwrap();
        }
        writer.finish().unwrap();
        path
    }

    #[test]
    fn rejects_zip_slip() {
        for bad in [
            "../evil.txt",
            "a/../../evil.txt",
            "/abs.txt",
            "\\abs.txt",
            "C:/Windows/evil.txt",
            "C:evil.txt",
            "a\\..\\..\\evil.txt",
            "a/b:stream",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let archive = make_zip(dir.path(), &[bad]);
            let staging = dir.path().join("staging");
            assert!(
                extract_zip(&archive, &staging).is_err(),
                "{bad:?} must be rejected"
            );
            assert!(!dir.path().join("evil.txt").exists());
        }
    }

    #[test]
    fn rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("link.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer
            .add_symlink("bin/link", "/etc/passwd", SimpleFileOptions::default())
            .unwrap();
        writer.finish().unwrap();
        assert!(extract_zip(&path, &dir.path().join("staging")).is_err());
    }

    #[test]
    fn extracts_normal_entries() {
        let dir = tempfile::tempdir().unwrap();
        let archive = make_zip(dir.path(), &["root/bin/winws.exe", "root/./lists/a.txt"]);
        let staging = dir.path().join("staging");
        extract_zip(&archive, &staging).unwrap();
        assert!(staging.join("root/bin/winws.exe").exists());
        assert!(staging.join("root/lists/a.txt").exists());
        // Staging must be fresh.
        assert!(extract_zip(&archive, &staging).is_err());
    }
}
