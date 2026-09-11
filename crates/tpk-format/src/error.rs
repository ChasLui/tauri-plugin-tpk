//! Error codes and the format-layer error type.

use std::fmt;

/// The frozen error codes from the TPK/1 specification (section 15).
///
/// Every error raised anywhere in the TPK stack maps to exactly one of these.
/// The plugin layer serializes them as `{"code": "E_HASH", "message": "..."}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The plugin is disabled by configuration.
    Disabled,
    /// Network transport failure.
    Network,
    /// A signature did not verify against any trusted key.
    Signature,
    /// Content did not match its declared hash.
    Hash,
    /// The document violates the TPK/1 schema.
    Spec,
    /// A path violates the path rules (section 3.2).
    Path,
    /// A parent link is missing, malformed or unsatisfiable.
    Parent,
    /// The current shell version is outside the pack's supported range.
    Shell,
    /// The channel manifest is older than one already seen.
    Watermark,
    /// The pack is blacklisted.
    Blacklist,
    /// Filesystem failure.
    Io,
    /// A delta could not be applied.
    Delta,
    /// The on-disk state is missing or inconsistent.
    State,
    /// A policy (override globs, platform availability, packaging rules) was violated.
    Policy,
}

impl ErrorCode {
    /// The wire representation, e.g. `"E_HASH"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "E_DISABLED",
            Self::Network => "E_NETWORK",
            Self::Signature => "E_SIGNATURE",
            Self::Hash => "E_HASH",
            Self::Spec => "E_SPEC",
            Self::Path => "E_PATH",
            Self::Parent => "E_PARENT",
            Self::Shell => "E_SHELL",
            Self::Watermark => "E_WATERMARK",
            Self::Blacklist => "E_BLACKLIST",
            Self::Io => "E_IO",
            Self::Delta => "E_DELTA",
            Self::State => "E_STATE",
            Self::Policy => "E_POLICY",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors raised while parsing or verifying TPK documents and containers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FormatError {
    /// A path violates the rules in specification section 3.2.
    #[error("invalid path {path:?}: {reason}")]
    Path {
        /// The offending path, as written in the document.
        path: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// A pack id violates `[a-z0-9][a-z0-9-]{0,62}`.
    #[error("invalid pack id {0:?}")]
    PackId(String),

    /// The document is not TPK/1 (or not a channel manifest of the expected version).
    #[error("unsupported spec tag {found:?}, expected {expected:?}")]
    SpecTag {
        /// The tag found in the document.
        found: String,
        /// The tag this parser accepts.
        expected: &'static str,
    },

    /// The document is structurally invalid.
    #[error("invalid document: {0}")]
    Spec(String),

    /// A parent link is missing, present when forbidden, or unsatisfiable.
    #[error("invalid parent link: {0}")]
    Parent(String),

    /// A hex digest was malformed or did not match the content.
    #[error("hash mismatch: {0}")]
    Hash(String),

    /// No trusted key verified the signature, or the signature was malformed.
    #[error("signature verification failed: {0}")]
    Signature(String),

    /// A packaging or override policy was violated.
    #[error("policy violation: {0}")]
    Policy(String),

    /// Filesystem or container read failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl FormatError {
    /// The frozen error code this failure maps to.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Path { .. } => ErrorCode::Path,
            Self::PackId(_) | Self::SpecTag { .. } | Self::Spec(_) => ErrorCode::Spec,
            Self::Parent(_) => ErrorCode::Parent,
            Self::Hash(_) => ErrorCode::Hash,
            Self::Signature(_) => ErrorCode::Signature,
            Self::Policy(_) => ErrorCode::Policy,
            Self::Io(_) => ErrorCode::Io,
        }
    }
}

/// Convenience alias for fallible format operations.
pub type Result<T> = std::result::Result<T, FormatError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_the_frozen_wire_name() {
        // Spec section 15 freezes these 14 strings; a typo here is a silent
        // protocol break, so pin them explicitly rather than deriving them.
        let pairs = [
            (ErrorCode::Disabled, "E_DISABLED"),
            (ErrorCode::Network, "E_NETWORK"),
            (ErrorCode::Signature, "E_SIGNATURE"),
            (ErrorCode::Hash, "E_HASH"),
            (ErrorCode::Spec, "E_SPEC"),
            (ErrorCode::Path, "E_PATH"),
            (ErrorCode::Parent, "E_PARENT"),
            (ErrorCode::Shell, "E_SHELL"),
            (ErrorCode::Watermark, "E_WATERMARK"),
            (ErrorCode::Blacklist, "E_BLACKLIST"),
            (ErrorCode::Io, "E_IO"),
            (ErrorCode::Delta, "E_DELTA"),
            (ErrorCode::State, "E_STATE"),
            (ErrorCode::Policy, "E_POLICY"),
        ];
        assert_eq!(pairs.len(), 14);
        for (code, wire) in pairs {
            assert_eq!(code.as_str(), wire);
            assert_eq!(code.to_string(), wire);
        }
    }

    #[test]
    fn errors_map_to_their_codes() {
        assert_eq!(
            FormatError::Path {
                path: "/x".into(),
                reason: "test"
            }
            .code(),
            ErrorCode::Path
        );
        assert_eq!(FormatError::PackId("X".into()).code(), ErrorCode::Spec);
        assert_eq!(
            FormatError::SpecTag {
                found: "tpk/2".into(),
                expected: "tpk/1"
            }
            .code(),
            ErrorCode::Spec
        );
        assert_eq!(FormatError::Parent("x".into()).code(), ErrorCode::Parent);
        assert_eq!(FormatError::Hash("x".into()).code(), ErrorCode::Hash);
        assert_eq!(
            FormatError::Signature("x".into()).code(),
            ErrorCode::Signature
        );
        assert_eq!(FormatError::Policy("x".into()).code(), ErrorCode::Policy);
        let io = FormatError::Io(std::io::Error::other("x"));
        assert_eq!(io.code(), ErrorCode::Io);
    }
}
