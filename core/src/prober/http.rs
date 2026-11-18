//! reqwest probes (D23): upstream's standard HEAD checks and DPI-checker POSTs, with comparable semantics.

use std::time::Duration;

use reqwest::{Client, header, redirect, tls};

use crate::api::{DpiStatus, ProbeOutcome};

pub const STANDARD_TIMEOUT: Duration = Duration::from_secs(4);
pub const STANDARD_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub const DPI_TIMEOUT: Duration = Duration::from_secs(5);
pub const DPI_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub const DPI_PAYLOAD_BYTES: usize = 65_536;

/// One client per upstream variant: HTTP/1.1, TLS 1.2 only, TLS 1.3 only. `None` when the TLS backend can't do it.
pub type VariantClients = [Option<Client>; 3];

pub fn variant_clients(timeout: Duration, connect_timeout: Duration) -> VariantClients {
    let base = || {
        Client::builder()
            .timeout(timeout)
            .connect_timeout(connect_timeout)
            .redirect(redirect::Policy::none())
            .http1_only()
            .user_agent(concat!("zapret-gui/", env!("CARGO_PKG_VERSION")))
    };
    let build = |builder: reqwest::ClientBuilder| match builder.build() {
        Ok(client) => Some(client),
        Err(error) => {
            tracing::warn!("probe client unavailable: {error}");
            None
        }
    };
    [
        build(base()),
        build(
            base()
                .min_tls_version(tls::Version::TLS_1_2)
                .max_tls_version(tls::Version::TLS_1_2),
        ),
        build(
            base()
                .min_tls_version(tls::Version::TLS_1_3)
                .max_tls_version(tls::Version::TLS_1_3),
        ),
    ]
}

fn error_chain_text(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(" | ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text.to_lowercase()
}

/// Upstream's classification: certificate/DNS problems = SSL, unsupported protocol = UNSUP, else ERROR.
pub fn classify_error(error: &reqwest::Error) -> ProbeOutcome {
    let text = error_chain_text(error);
    const SSL_MARKERS: &[&str] = &[
        "certificate",
        "dns error",
        "failed to lookup",
        "could not resolve",
        "self signed",
        "self-signed",
        "local issuer",
    ];
    const UNSUPPORTED_MARKERS: &[&str] = &[
        "not supported",
        "unsupported",
        "protocol version",
        "no protocols available",
        "common algorithm",
        "wrong version number",
    ];
    if SSL_MARKERS.iter().any(|marker| text.contains(marker)) {
        ProbeOutcome::Ssl
    } else if UNSUPPORTED_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
    {
        ProbeOutcome::Unsupported
    } else {
        ProbeOutcome::Error
    }
}

/// HEAD request; any HTTP response counts as OK (like `curl -I` without `-f`).
pub async fn head(client: Option<&Client>, url: &str) -> ProbeOutcome {
    let Some(client) = client else {
        return ProbeOutcome::Unsupported;
    };
    match client.head(url).send().await {
        Ok(_) => ProbeOutcome::Ok,
        Err(error) => classify_error(&error),
    }
}

/// Deterministic filler for the 64 KiB upload (content doesn't matter, it just shouldn't compress).
pub fn dpi_payload() -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..DPI_PAYLOAD_BYTES)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// POSTs 64 KiB with `Range: bytes=0-65535` and watches the download.
/// LIKELY_BLOCKED = the request timed out after connecting with nothing downloaded (the 16-20 KB freeze).
/// reqwest can't report uploaded bytes, so "connected and timed out" stands in for upstream's `up > 0`.
pub async fn dpi_post(client: Option<&Client>, url: &str) -> DpiStatus {
    let Some(client) = client else {
        return DpiStatus::Unsupported;
    };
    let request = client
        .post(url)
        .header(header::RANGE, format!("bytes=0-{}", DPI_PAYLOAD_BYTES - 1))
        .body(dpi_payload());
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() && !error.is_connect() => return DpiStatus::LikelyBlocked,
        Err(error) => {
            return match classify_error(&error) {
                ProbeOutcome::Unsupported => DpiStatus::Unsupported,
                _ => DpiStatus::Fail,
            };
        }
    };
    let mut downloaded = 0usize;
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => downloaded += chunk.len(),
            Ok(None) => return DpiStatus::Ok,
            Err(error) if error.is_timeout() && downloaded == 0 => return DpiStatus::LikelyBlocked,
            Err(_) => return DpiStatus::Fail,
        }
    }
}

#[cfg(test)]
mod tests {
    //! A local fake-DPI server: deterministic freeze/reset/slow behaviours without real DPI.

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    #[derive(Clone, Copy)]
    enum Behaviour {
        /// Reads the request, answers with 1 MiB.
        Ok,
        /// Reads ~16 KB of the upload, then stalls forever.
        FreezeAfter16k,
        /// Sends headers and 16 KB of body, then stalls.
        FreezeDownload,
        /// Drops the connection after reading a little.
        Reset,
        /// Trickles a small body slowly but finishes before the timeout.
        Slow,
    }

    /// Reads the request headers and up to `body_limit` bytes of its body.
    async fn read_some(stream: &mut TcpStream, body_limit: usize) {
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(count) => data.extend_from_slice(&buffer[..count]),
            }
            let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
                if data.len() >= body_limit {
                    return;
                }
                continue;
            };
            let headers = String::from_utf8_lossy(&data[..end]).to_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            if data.len() - end - 4 >= length.min(body_limit) {
                return;
            }
        }
    }

    async fn serve(behaviour: Behaviour) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    match behaviour {
                        Behaviour::Ok => {
                            read_some(&mut stream, DPI_PAYLOAD_BYTES).await;
                            let body = vec![b'x'; 1 << 20];
                            let head = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(head.as_bytes()).await;
                            let _ = stream.write_all(&body).await;
                        }
                        Behaviour::FreezeAfter16k => {
                            read_some(&mut stream, 16 * 1024).await;
                            tokio::time::sleep(Duration::from_secs(60)).await;
                        }
                        Behaviour::FreezeDownload => {
                            read_some(&mut stream, DPI_PAYLOAD_BYTES).await;
                            let _ = stream
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\n\r\n")
                                .await;
                            let _ = stream.write_all(&vec![b'x'; 16 * 1024]).await;
                            tokio::time::sleep(Duration::from_secs(60)).await;
                        }
                        Behaviour::Reset => {
                            read_some(&mut stream, 100).await;
                            drop(stream);
                        }
                        Behaviour::Slow => {
                            read_some(&mut stream, DPI_PAYLOAD_BYTES).await;
                            let _ = stream
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")
                                .await;
                            for _ in 0..5 {
                                tokio::time::sleep(Duration::from_millis(60)).await;
                                let _ = stream.write_all(b"x").await;
                            }
                        }
                    }
                });
            }
        });
        format!("http://{address}/")
    }

    fn clients() -> VariantClients {
        variant_clients(Duration::from_millis(800), Duration::from_millis(300))
    }

    #[tokio::test]
    async fn dpi_classification() {
        let clients = clients();
        let client = clients[0].as_ref();
        assert_eq!(
            dpi_post(client, &serve(Behaviour::Ok).await).await,
            DpiStatus::Ok
        );
        assert_eq!(
            dpi_post(client, &serve(Behaviour::FreezeAfter16k).await).await,
            DpiStatus::LikelyBlocked
        );
        assert_eq!(
            dpi_post(client, &serve(Behaviour::FreezeDownload).await).await,
            DpiStatus::Fail
        );
        assert_eq!(
            dpi_post(client, &serve(Behaviour::Reset).await).await,
            DpiStatus::Fail
        );
        assert_eq!(
            dpi_post(client, &serve(Behaviour::Slow).await).await,
            DpiStatus::Ok
        );
        assert_eq!(
            dpi_post(None, "http://127.0.0.1:1/").await,
            DpiStatus::Unsupported
        );
    }

    #[tokio::test]
    async fn head_classification() {
        let clients = clients();
        let client = clients[0].as_ref();
        assert_eq!(
            head(client, &serve(Behaviour::Ok).await).await,
            ProbeOutcome::Ok
        );
        assert_eq!(
            head(client, &serve(Behaviour::Reset).await).await,
            ProbeOutcome::Error
        );
        // Nothing listens on port 1: connection refused.
        assert_eq!(
            head(client, "http://127.0.0.1:1/").await,
            ProbeOutcome::Error
        );
        assert_eq!(
            head(None, "http://127.0.0.1:1/").await,
            ProbeOutcome::Unsupported
        );
    }
}
