//! Store errors and their mapping to the frozen codes.

use tpk_format::error::ErrorCode;

/// Failures from the on-disk store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The state file is missing, unreadable or inconsistent.
    #[error("store state: {0}")]
    State(String),

    /// A layer file failed its integrity check.
    #[error("layer integrity: {0}")]
    Integrity(String),

    /// A delta could not be reconstructed.
    #[error("delta materialization: {0}")]
    Delta(String),

    /// A pack is refused because it is blacklisted.
    #[error("blacklisted: {0}")]
    Blacklisted(String),

    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A format-level failure surfaced while handling a layer.
    #[error(transparent)]
    Format(#[from] tpk_format::error::FormatError),
}

impl StoreError {
    /// The frozen error code this maps to.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::State(_) => ErrorCode::State,
            Self::Integrity(_) => ErrorCode::Hash,
            Self::Delta(_) => ErrorCode::Delta,
            Self::Blacklisted(_) => ErrorCode::Blacklist,
            Self::Io(_) => ErrorCode::Io,
            Self::Format(e) => e.code(),
        }
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, StoreError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_their_codes() {
        assert_eq!(StoreError::State("x".into()).code(), ErrorCode::State);
        assert_eq!(StoreError::Integrity("x".into()).code(), ErrorCode::Hash);
        assert_eq!(StoreError::Delta("x".into()).code(), ErrorCode::Delta);
        assert_eq!(
            StoreError::Blacklisted("x".into()).code(),
            ErrorCode::Blacklist
        );
        assert_eq!(
            StoreError::Io(std::io::Error::other("x")).code(),
            ErrorCode::Io
        );
        // Format errors keep their own code rather than being flattened.
        let format = StoreError::Format(tpk_format::error::FormatError::Hash("x".into()));
        assert_eq!(format.code(), ErrorCode::Hash);
    }
}
