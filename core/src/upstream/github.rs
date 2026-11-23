//! GitHub access: release listing (cached, ETag), downloads with progress/cancel, raw files.
//! HTTPS to GitHub hosts only (SECURITY.md T6).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use reqwest::{Client, StatusCode, header, redirect};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::UpstreamError;

pub const REPOSITORY: &str = "Flowseal/zapret-discord-youtube";
pub const RELEASES_URL: &str =
    "https://api.github.com/repos/Flowseal/zapret-discord-youtube/releases?per_page=30";
pub const IPSET_URL: &str = "https://raw.githubusercontent.com/Flowseal/zapret-discord-youtube/refs/heads/main/.service/ipset-service.txt";
pub const HOSTS_URL: &str = "https://raw.githubusercontent.com/Flowseal/zapret-discord-youtube/refs/heads/main/.service/hosts";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub zip_url: String,
}

pub fn is_github_host(host: &str) -> bool {
    host == "github.com"
        || host == "api.github.com"
        || host == "codeload.github.com"
        || host.ends_with(".githubusercontent.com")
}

/// The HTTP client for upstream data: HTTPS only, redirects only to GitHub hosts.
pub fn client() -> Client {
    Client::builder()
        .https_only(true)
        .redirect(redirect::Policy::custom(|attempt| {
            let allowed = attempt.url().scheme() == "https"
                && attempt.url().host_str().is_some_and(is_github_host);
            if attempt.previous().len() > 10 || !allowed {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("zapret-gui/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

/// Version tags end up in paths: allow only `[A-Za-z0-9._-]`, not starting with a dot.
pub fn is_valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && !tag.starts_with('.')
        && tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

/// Releases newest first; drafts, prereleases and releases without a `.zip` asset are skipped.
pub fn parse_releases(json: &str) -> Result<Vec<Release>, UpstreamError> {
    let releases: Vec<ApiRelease> = serde_json::from_str(json)
        .map_err(|error| UpstreamError::Network(format!("ответ GitHub: {error}")))?;
    Ok(releases
        .into_iter()
        .filter(|release| !release.draft && !release.prerelease && is_valid_tag(&release.tag_name))
        .filter_map(|release| {
            let asset = release
                .assets
                .into_iter()
                .find(|asset| asset.name.to_lowercase().ends_with(".zip"))?;
            let url = reqwest::Url::parse(&asset.browser_download_url).ok()?;
            (url.scheme() == "https" && url.host_str().is_some_and(is_github_host)).then_some(
                Release {
                    tag: release.tag_name,
                    zip_url: asset.browser_download_url,
                },
            )
        })
        .collect())
}

#[derive(Serialize, Deserialize)]
struct ReleasesCache {
    etag: Option<String>,
    body: String,
}

/// Lists releases with a conditional request. Falls back to the cache when offline.
pub async fn list_releases(
    http: &Client,
    cache_file: &Path,
) -> Result<Vec<Release>, UpstreamError> {
    let cached: Option<ReleasesCache> = std::fs::read_to_string(cache_file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let mut request = http
        .get(RELEASES_URL)
        .header(header::ACCEPT, "application/vnd.github+json")
        .timeout(Duration::from_secs(20));
    if let Some(etag) = cached.as_ref().and_then(|cache| cache.etag.as_ref()) {
        request = request.header(header::IF_NONE_MATCH, etag);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            return match cached {
                Some(cache) => {
                    tracing::warn!("release list offline ({error}); using cache");
                    parse_releases(&cache.body)
                }
                None => Err(UpstreamError::Network(error.to_string())),
            };
        }
    };
    if response.status() == StatusCode::NOT_MODIFIED
        && let Some(cache) = cached
    {
        return parse_releases(&cache.body);
    }
    if !response.status().is_success() {
        let status = response.status();
        return match cached {
            Some(cache) => parse_releases(&cache.body),
            None => Err(UpstreamError::Network(format!("GitHub ответил {status}"))),
        };
    }
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response
        .text()
        .await
        .map_err(|error| UpstreamError::Network(error.to_string()))?;
    let releases = parse_releases(&body)?;
    if let Some(parent) = cache_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(&ReleasesCache { etag, body }) {
        let _ = std::fs::write(cache_file, text);
    }
    Ok(releases)
}

/// Downloads to `destination`, reporting (downloaded, total). Returns the SHA256 (hex).
pub async fn download(
    http: &Client,
    url: &str,
    destination: &Path,
    progress: impl Fn(u64, Option<u64>),
    cancel: &AtomicBool,
) -> Result<String, UpstreamError> {
    let mut response = http
        .get(url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|error| UpstreamError::Network(error.to_string()))?;
    let total = response.content_length();
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::File::create(destination).await?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| UpstreamError::Network(error.to_string()))?
    {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = tokio::fs::remove_file(destination).await;
            return Err(UpstreamError::Cancelled);
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        progress(downloaded, total);
    }
    file.flush().await?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Small text files from raw.githubusercontent.com (cache-busted, like upstream).
pub async fn fetch_text(http: &Client, url: &str) -> Result<String, UpstreamError> {
    http.get(format!("{url}?t={}", crate::paths::unix_now()))
        .header(header::CACHE_CONTROL, "no-cache")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|error| UpstreamError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| UpstreamError::Network(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_parsing() {
        let json = r#"[
          {"tag_name":"1.10.4","assets":[
             {"name":"zapret-discord-youtube-1.10.4.rar","browser_download_url":"https://github.com/x/1.rar"},
             {"name":"zapret-discord-youtube-1.10.4.zip","browser_download_url":"https://github.com/x/1.zip"}]},
          {"tag_name":"1.10.3","prerelease":true,"assets":[{"name":"a.zip","browser_download_url":"https://github.com/a.zip"}]},
          {"tag_name":"1.10.2","assets":[{"name":"a.rar","browser_download_url":"https://github.com/a.rar"}]},
          {"tag_name":"../evil","assets":[{"name":"a.zip","browser_download_url":"https://github.com/a.zip"}]},
          {"tag_name":"1.10.1","assets":[{"name":"a.zip","browser_download_url":"https://evil.example/a.zip"}]}
        ]"#;
        let releases = parse_releases(json).unwrap();
        assert_eq!(
            releases,
            vec![Release {
                tag: "1.10.4".into(),
                zip_url: "https://github.com/x/1.zip".into()
            }]
        );
    }

    #[test]
    fn tags_and_hosts() {
        assert!(is_valid_tag("1.9.9b"));
        assert!(!is_valid_tag("../x"));
        assert!(!is_valid_tag("a/b"));
        assert!(!is_valid_tag(".hidden"));
        assert!(is_github_host("release-assets.githubusercontent.com"));
        assert!(!is_github_host("github.com.evil.example"));
    }
}
