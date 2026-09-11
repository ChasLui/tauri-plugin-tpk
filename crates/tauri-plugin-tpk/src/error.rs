//! The plugin's error type, and how it reaches JavaScript.
//!
//! Errors serialize as `{"code": "E_HASH", "message": "..."}` so the frontend
//! can branch on a stable code rather than on prose.
//!
//! Note what does *not* come through here: `check` and `download` report
//! up-to-date, shell-required, blacklisted and disabled as **outcomes**, not as
//! errors. An `Err` means something the caller cannot act on.

use serde::{Serialize, Serializer};
use tpk_format::error::ErrorCode;

/// A failure worth surfacing to the frontend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Configuration is missing or invalid.
    #[error("configuration: {0}")]
    Config(String),

    /// The plugin never finished starting up.
    #[error("the plugin is not initialized")]
    NotInitialized,

    /// A format-level failure.
    #[error(transparent)]
    Format(#[from] tpk_format::error::FormatError),

    /// An on-disk failure.
    #[error(transparent)]
    Store(#[from] tpk_store::StoreError),

    /// A network or download failure.
    #[error(transparent)]
    Client(#[from] tpk_client::ClientError),

    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The frozen error code this maps to.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Config(_) => ErrorCode::Spec,
            Self::NotInitialized => ErrorCode::State,
            Self::Format(e) => e.code(),
            Self::Store(e) => e.code(),
            Self::Client(e) => e.code(),
            Self::Io(_) => ErrorCode::Io,
        }
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Error", 2)?;
        s.serialize_field("code", self.code().as_str())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_their_codes() {
        assert_eq!(Error::Config("x".into()).code(), ErrorCode::Spec);
        assert_eq!(Error::NotInitialized.code(), ErrorCode::State);
        assert_eq!(
            Error::Format(tpk_format::error::FormatError::Hash("x".into())).code(),
            ErrorCode::Hash
        );
        assert_eq!(
            Error::Store(tpk_store::StoreError::Delta("x".into())).code(),
            ErrorCode::Delta
        );
        assert_eq!(
            Error::Client(tpk_client::ClientError::Network("x".into())).code(),
            ErrorCode::Network
        );
    }

    #[test]
    fn serializes_as_a_code_and_a_message() {
        let json = serde_json::to_value(Error::NotInitialized).unwrap();
        assert_eq!(json["code"], "E_STATE");
        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("not initialized"));
    }

    #[test]
    fn the_code_survives_being_wrapped() {
        // A store error that itself wraps a format error keeps the innermost
        // code rather than being flattened to E_STATE.
        let inner =
            tpk_store::StoreError::Format(tpk_format::error::FormatError::Signature("bad".into()));
        let json = serde_json::to_value(Error::Store(inner)).unwrap();
        assert_eq!(json["code"], "E_SIGNATURE");
    }
}
