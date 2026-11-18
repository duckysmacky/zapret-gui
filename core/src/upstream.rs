//! Upstream sync: download, verify, safely extract and install release bundles.

pub mod bundle;
pub mod extract;
pub mod github;
pub mod parser;

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::paths::{Paths, unix_now};
use bundle::BundleMeta;
use github::Release;

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("{0}")]
    Network(String),
    #[error("архив повреждён или подозрителен: {0}")]
    InvalidArchive(String),
    #[error("ошибка файловой системы: {0}")]
    Io(#[from] std::io::Error),
    #[error("отменено")]
    Cancelled,
}

/// Download → verify → extract into fresh staging → parse → move into the bundle store.
/// An existing bundle of the same version is replaced (used by "re-download").
pub async fn install_release(
    http: &reqwest::Client,
    paths: &Paths,
    release: &Release,
    progress: impl Fn(u64, Option<u64>),
    cancel: &AtomicBool,
) -> Result<(), UpstreamError> {
    if !github::is_valid_tag(&release.tag) {
        return Err(UpstreamError::InvalidArchive(format!(
            "недопустимая версия {:?}",
            release.tag
        )));
    }
    let archive = paths
        .cache_dir()
        .join(format!("download-{}.zip", release.tag));
    let sha256 = github::download(http, &release.zip_url, &archive, progress, cancel).await?;
    let paths = paths.clone();
    let version = release.tag.clone();
    tokio::task::spawn_blocking(move || install_archive(&paths, &version, &archive, sha256))
        .await
        .map_err(|error| UpstreamError::Io(std::io::Error::other(error)))?
}

/// The blocking part of the install; also used by tests with local archives.
pub fn install_archive(
    paths: &Paths,
    version: &str,
    archive: &Path,
    sha256: String,
) -> Result<(), UpstreamError> {
    let bundles_dir = paths.bundles_dir();
    std::fs::create_dir_all(&bundles_dir)?;
    let staging = bundles_dir.join(format!(".staging-{version}"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    let outcome = (|| {
        extract::extract_zip(archive, &staging)?;
        let root = bundle::find_root(&staging).ok_or_else(|| {
            UpstreamError::InvalidArchive("не найдена папка с bin\\winws.exe".into())
        })?;
        bundle::verify_structure(&root).map_err(UpstreamError::InvalidArchive)?;
        if !bundle::load_strategies(&root)
            .iter()
            .any(|strategy| strategy.is_usable())
        {
            return Err(UpstreamError::InvalidArchive(
                "ни одна стратегия не разобрана".into(),
            ));
        }
        let meta = BundleMeta {
            version: version.to_string(),
            sha256,
            installed_at: unix_now(),
        };
        std::fs::write(
            root.join(bundle::META_FILE),
            serde_json::to_string_pretty(&meta).map_err(std::io::Error::other)?,
        )?;
        let final_dir = paths.bundle_dir(version);
        if final_dir.exists() {
            std::fs::remove_dir_all(&final_dir)?;
        }
        std::fs::rename(&root, &final_dir)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(archive);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn installs_nested_archive() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let archive = dir.path().join("release.zip");
        let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
        for (name, data) in [
            ("zapret-1.0/bin/winws.exe", ""),
            ("zapret-1.0/bin/WinDivert64.sys", ""),
            ("zapret-1.0/lists/list-general.txt", "discord.com\n"),
            (
                "zapret-1.0/general.bat",
                "start \"\" \"%BIN%winws.exe\" --x\r\n",
            ),
        ] {
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        install_archive(&paths, "1.0", &archive, "abc".into()).unwrap();
        assert_eq!(
            bundle::installed_versions(&paths.bundles_dir()),
            vec!["1.0".to_string()]
        );
        assert!(
            paths
                .bundle_dir("1.0")
                .join("lists/list-general.txt")
                .exists()
        );
        assert!(!archive.exists());
    }
}
