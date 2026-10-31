//! Where channel manifests come from, and how a URL template is expanded.

use std::collections::HashMap;

use crate::error::{ClientError, Result};

/// The four variables a manifest URL may contain.
///
/// Deliberately closed: the URL is native configuration that JavaScript cannot
/// reach, and an open-ended template would reintroduce the ability to point the
/// updater somewhere else.
pub const ALLOWED_VARIABLES: [&str; 4] = ["channel", "arch", "target", "shell"];

/// Values substituted into a manifest URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchContext {
    /// The channel being polled.
    pub channel: String,
    /// Target architecture, e.g. `aarch64`.
    pub arch: String,
    /// Target triple.
    pub target: String,
    /// The running shell version.
    pub shell: String,
}

impl FetchContext {
    /// Build a context for the current build.
    pub fn for_current(channel: impl Into<String>, shell: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            arch: std::env::consts::ARCH.to_string(),
            target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            shell: shell.into(),
        }
    }

    fn value(&self, name: &str) -> Option<&str> {
        match name {
            "channel" => Some(&self.channel),
            "arch" => Some(&self.arch),
            "target" => Some(&self.target),
            "shell" => Some(&self.shell),
            _ => None,
        }
    }
}

/// Expand `{{variable}}` placeholders in a manifest URL.
///
/// # Errors
///
/// Returns [`ClientError::Template`] for an unknown variable and
/// [`ClientError::InsecureUrl`] if the result is not https.
pub fn expand_url(template: &str, ctx: &FetchContext) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(ClientError::Template(rest[start..].to_string()));
        };
        let name = after[..end].trim();
        let value = ctx
            .value(name)
            .ok_or_else(|| ClientError::Template(name.to_string()))?;
        out.push_str(value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);

    if !out.starts_with("https://") {
        return Err(ClientError::InsecureUrl(crate::error::redact(&out)));
    }
    Ok(out)
}

/// A manifest and its detached signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedBytes {
    /// The exact bytes the signature covers.
    pub body: Vec<u8>,
    /// The minisign document.
    pub signature: String,
}

/// Fetches channel manifests.
///
/// A trait so tests can inject one without a network, and so an application with
/// unusual transport needs can supply its own.
pub trait ChannelSource: Send + Sync {
    /// Fetch the manifest and its signature.
    fn fetch(
        &self,
        ctx: &FetchContext,
    ) -> impl std::future::Future<Output = Result<SignedBytes>> + Send;
}

/// Fetches over HTTPS, with the signature at `<url>.minisig`.
#[derive(Debug, Clone)]
pub struct HttpChannelSource {
    template: String,
    headers: HashMap<String, String>,
    max_bytes: u64,
}

/// Ceiling on a channel manifest. Generous for a document listing packs.
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

/// Ceiling on a detached minisign document, independently of the manifest budget.
pub const MAX_SIGNATURE_BYTES: u64 = 64 * 1024;

impl HttpChannelSource {
    /// Point at a manifest URL template.
    pub fn new(template: impl Into<String>) -> Self {
        Self {
            template: template.into(),
            headers: HashMap::new(),
            max_bytes: MAX_MANIFEST_BYTES,
        }
    }

    /// Add headers sent with every request, e.g. an enterprise auth token.
    #[must_use]
    pub fn with_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    /// The configured template.
    pub fn template(&self) -> &str {
        &self.template
    }

    /// The headers sent with each request.
    pub fn headers(&self) -> &HashMap<String, String> {
        &self.headers
    }
}

impl ChannelSource for HttpChannelSource {
    async fn fetch(&self, ctx: &FetchContext) -> Result<SignedBytes> {
        let url = expand_url(&self.template, ctx)?;
        let client = crate::runtime_http_client()?;

        let body = get_bounded(&client, &url, &self.headers, self.max_bytes).await?;
        let signature = get_bounded(
            &client,
            &format!("{url}.minisig"),
            &self.headers,
            MAX_SIGNATURE_BYTES,
        )
        .await?;

        Ok(SignedBytes {
            body,
            signature: String::from_utf8(signature)
                .map_err(|e| ClientError::Network(format!("signature is not UTF-8: {e}")))?,
        })
    }
}

async fn get_bounded(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
    max: u64,
) -> Result<Vec<u8>> {
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let mut response = request
        .send()
        .await
        .map_err(crate::transport::network_error)?;

    let status = response.status();
    if !status.is_success() {
        return Err(ClientError::Http {
            status: status.as_u16(),
            message: crate::error::redact(url),
        });
    }
    if let Some(len) = response.content_length() {
        if len > max {
            return Err(ClientError::TooLarge {
                what: crate::error::redact(url),
                limit: max,
            });
        }
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(crate::transport::network_error)?
    {
        if chunk.len() as u64 > max.saturating_sub(bytes.len() as u64) {
            return Err(ClientError::TooLarge {
                what: crate::error::redact(url),
                limit: max,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_reads_enforce_advertised_and_streaming_budgets() {
        let cases = [
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n123456789".to_vec(),
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n123456789".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n1234\r\n5\r\n56789\r\n0\r\n\r\n".to_vec(),
        ];
        for response in cases {
            let (url, server) = crate::test_http::serve(response);
            let error = get_bounded(&crate::test_http::client(), &url, &HashMap::new(), 8)
                .await
                .unwrap_err();
            assert!(matches!(error, ClientError::TooLarge { limit: 8, .. }));
            assert!(!error.to_string().contains("secret"));
            server.join().unwrap();
        }
        let (url, server) = crate::test_http::serve(
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n12345678".to_vec(),
        );
        assert_eq!(
            get_bounded(&crate::test_http::client(), &url, &HashMap::new(), 8)
                .await
                .unwrap(),
            b"12345678"
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn manifest_budget_is_four_mebibytes_without_content_length() {
        let mut response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        response.resize(response.len() + MAX_MANIFEST_BYTES as usize + 1, b'x');
        let (url, server) = crate::test_http::serve(response);
        assert!(matches!(
            get_bounded(
                &crate::test_http::client(),
                &url,
                &HashMap::new(),
                MAX_MANIFEST_BYTES
            )
            .await,
            Err(ClientError::TooLarge {
                limit: MAX_MANIFEST_BYTES,
                ..
            })
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn signature_has_its_own_small_budget() {
        let mut response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        response.resize(response.len() + MAX_SIGNATURE_BYTES as usize + 1, b'x');
        let (url, server) = crate::test_http::serve(response);
        assert!(matches!(
            get_bounded(
                &crate::test_http::client(),
                &url,
                &HashMap::new(),
                MAX_SIGNATURE_BYTES
            )
            .await,
            Err(ClientError::TooLarge {
                limit: MAX_SIGNATURE_BYTES,
                ..
            })
        ));
        server.join().unwrap();
    }

    fn ctx() -> FetchContext {
        FetchContext {
            channel: "stable".to_string(),
            arch: "aarch64".to_string(),
            target: "aarch64-apple-darwin".to_string(),
            shell: "2.3.0".to_string(),
        }
    }

    #[test]
    fn substitutes_every_allowed_variable() {
        let url = expand_url(
            "https://cdn.example.com/{{channel}}/{{arch}}/{{target}}/{{shell}}/latest.json",
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            url,
            "https://cdn.example.com/stable/aarch64/aarch64-apple-darwin/2.3.0/latest.json"
        );
    }

    #[test]
    fn a_template_without_variables_is_passed_through() {
        let url = "https://cdn.example.com/tpk/stable/latest.json";
        assert_eq!(expand_url(url, &ctx()).unwrap(), url);
    }

    #[test]
    fn an_unknown_variable_is_refused() {
        // The set is closed on purpose: the URL is native configuration that JS
        // cannot reach, and an open template would undo that.
        let err = expand_url("https://x.com/{{endpoint}}/latest.json", &ctx()).unwrap_err();
        assert!(matches!(err, ClientError::Template(_)), "{err}");
        assert_eq!(ALLOWED_VARIABLES.len(), 4);
    }

    #[test]
    fn an_unterminated_placeholder_is_refused() {
        assert!(expand_url("https://x.com/{{channel/latest.json", &ctx()).is_err());
    }

    #[test]
    fn a_plaintext_url_is_refused() {
        let err = expand_url("http://cdn.example.com/latest.json", &ctx()).unwrap_err();
        assert!(matches!(err, ClientError::InsecureUrl(_)), "{err}");
        assert!(expand_url("file:///etc/passwd", &ctx()).is_err());
    }

    #[test]
    fn an_insecure_url_is_redacted_in_the_error() {
        let err = expand_url("http://cdn.example.com/latest.json?token=secret", &ctx())
            .unwrap_err()
            .to_string();
        assert!(!err.contains("secret"), "{err}");
    }

    #[test]
    fn whitespace_inside_a_placeholder_is_tolerated() {
        assert_eq!(
            expand_url("https://x.com/{{ channel }}/latest.json", &ctx()).unwrap(),
            "https://x.com/stable/latest.json"
        );
    }

    #[test]
    fn the_same_variable_may_appear_twice() {
        assert_eq!(
            expand_url("https://x.com/{{channel}}/{{channel}}.json", &ctx()).unwrap(),
            "https://x.com/stable/stable.json"
        );
    }

    #[test]
    fn a_source_keeps_its_headers() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer x".to_string());
        let source = HttpChannelSource::new("https://x.com/latest.json").with_headers(headers);
        assert_eq!(source.headers().len(), 1);
        assert_eq!(source.template(), "https://x.com/latest.json");
    }

    #[test]
    fn the_current_context_is_populated() {
        let c = FetchContext::for_current("beta", "1.2.3");
        assert_eq!(c.channel, "beta");
        assert_eq!(c.shell, "1.2.3");
        assert!(!c.arch.is_empty());
        assert!(c.target.contains(&c.arch));
    }
}
