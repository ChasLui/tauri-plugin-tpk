//! Shared runtime HTTP policy.

use std::sync::OnceLock;
use std::time::Duration;

use crate::error::{ClientError, Result};

/// Build a reusable HTTPS-only client for manifests, signatures and packs.
///
/// Connections time out after 10 seconds; each read times out after 30 seconds.
/// There is no total deadline, so large packs can keep making progress. Redirects
/// cannot downgrade to HTTP. Applications should reuse this client across packs.
///
/// # Errors
///
/// Returns [`ClientError::Network`] if the HTTP client cannot be constructed.
pub fn runtime_http_client() -> Result<reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(network_error)?;
    let _ = CLIENT.set(client.clone());
    Ok(CLIENT.get().cloned().unwrap_or(client))
}

pub(crate) fn network_error(error: reqwest::Error) -> ClientError {
    // Do not render the source chain: redirect errors can contain another URL.
    let detail = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_body() || error.is_decode() {
        "response body failed"
    } else if error.is_redirect() {
        "redirect failed"
    } else {
        "HTTP request failed"
    };
    ClientError::Network(detail.into())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn runtime_rejects_http_and_errors_do_not_disclose_request_urls() {
        let error = super::runtime_http_client()
            .unwrap()
            .get("http://user:password@127.0.0.1:1/pack?token=secret#fragment")
            .send()
            .await
            .unwrap_err();
        let message = super::network_error(error).to_string();
        for secret in ["user", "password", "secret", "fragment"] {
            assert!(!message.contains(secret), "{message}");
        }
    }

    #[tokio::test]
    async fn send_and_body_failures_are_sanitized() {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        drop(socket);
        let error = crate::test_http::client()
            .get(format!("http://{address}/?token=secret"))
            .send()
            .await
            .unwrap_err();
        assert!(!super::network_error(error).to_string().contains("secret"));
        let (url, server) = crate::test_http::serve(
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nx".to_vec(),
        );
        let error = crate::test_http::client()
            .get(url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap_err();
        assert!(!super::network_error(error).to_string().contains("secret"));
        server.join().unwrap();
    }
}
