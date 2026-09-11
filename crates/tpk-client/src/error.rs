//! Client errors.

use tpk_format::error::ErrorCode;

/// Failures from fetching or downloading.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// Transport failure. Retryable.
    #[error("network: {0}")]
    Network(String),

    /// The server answered with an error status.
    #[error("http {status}: {message}")]
    Http {
        /// The status code.
        status: u16,
        /// Detail, with any query string already stripped.
        message: String,
    },

    /// A download exceeded its declared size.
    #[error("{what} is larger than the {limit} byte limit")]
    TooLarge {
        /// What was being fetched.
        what: String,
        /// The ceiling.
        limit: u64,
    },

    /// The manifest is older than one already seen on this channel.
    #[error("watermark {found} is below the floor {floor} for channel {channel}")]
    Watermark {
        /// The channel.
        channel: String,
        /// What the manifest carried.
        found: u64,
        /// What the device has already seen.
        floor: u64,
    },

    /// A URL template variable is not one of the four allowed names.
    #[error("unsupported template variable {0:?}")]
    Template(String),

    /// The source is not https.
    #[error("insecure url: {0}")]
    InsecureUrl(String),

    /// A format-level failure — signature, schema, or hash.
    #[error(transparent)]
    Format(#[from] tpk_format::error::FormatError),

    /// Filesystem failure while writing a download.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ClientError {
    /// The frozen error code this maps to.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Network(_) | Self::Http { .. } | Self::TooLarge { .. } => ErrorCode::Network,
            Self::Watermark { .. } => ErrorCode::Watermark,
            Self::Template(_) | Self::InsecureUrl(_) => ErrorCode::Spec,
            Self::Format(e) => e.code(),
            Self::Io(_) => ErrorCode::Io,
        }
    }

    /// Whether retrying could plausibly help.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network(_) | Self::Io(_) => true,
            Self::Http { status, .. } => *status >= 500 || *status == 429,
            _ => false,
        }
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, ClientError>;

/// Remove the query string from a URL before it reaches a log.
///
/// Update URLs routinely carry tokens; a log line is not the place for them.
pub fn redact(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?<redacted>"),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_their_codes() {
        assert_eq!(ClientError::Network("x".into()).code(), ErrorCode::Network);
        assert_eq!(
            ClientError::Http {
                status: 404,
                message: "x".into()
            }
            .code(),
            ErrorCode::Network
        );
        assert_eq!(
            ClientError::Watermark {
                channel: "stable".into(),
                found: 1,
                floor: 2
            }
            .code(),
            ErrorCode::Watermark
        );
        assert_eq!(
            ClientError::Template("{{nope}}".into()).code(),
            ErrorCode::Spec
        );
        assert_eq!(
            ClientError::Format(tpk_format::error::FormatError::Signature("x".into())).code(),
            ErrorCode::Signature
        );
    }

    #[test]
    fn only_transient_failures_are_retryable() {
        assert!(ClientError::Network("timeout".into()).is_retryable());
        assert!(ClientError::Http {
            status: 503,
            message: String::new()
        }
        .is_retryable());
        assert!(ClientError::Http {
            status: 429,
            message: String::new()
        }
        .is_retryable());

        // A bad signature will not fix itself.
        assert!(
            !ClientError::Format(tpk_format::error::FormatError::Signature("x".into()))
                .is_retryable()
        );
        assert!(!ClientError::Http {
            status: 404,
            message: String::new()
        }
        .is_retryable());
        assert!(!ClientError::Watermark {
            channel: "stable".into(),
            found: 1,
            floor: 2
        }
        .is_retryable());
    }

    #[test]
    fn redaction_strips_the_query_string() {
        assert_eq!(
            redact("https://cdn.example.com/latest.json?token=secret123"),
            "https://cdn.example.com/latest.json?<redacted>"
        );
        assert_eq!(
            redact("https://cdn.example.com/latest.json"),
            "https://cdn.example.com/latest.json"
        );
    }
}
