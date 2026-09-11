//! Loading the signing key.
//!
//! The key is read from the environment and never written to disk. The pattern
//! of dumping it to a temp file and `rm`-ing afterwards does not survive a
//! failed job: the cleanup step never runs, and the file is left where the next
//! step — or an artifact upload — can pick it up.

use tpk_format::secret::SecretKey;

use crate::{CliError, CliResult};

/// Environment variable holding the minisign secret key.
pub const KEY_ENV: &str = "TPK_SIGNING_KEY";

/// Load the signing key from [`KEY_ENV`].
///
/// Accepts the two-line `.key` file verbatim or its base64 body on a single
/// line, so the same value works from a file, a CI secret, or a paste.
///
/// # Errors
///
/// Returns [`CliError::usage`] when the variable is unset or unparseable.
pub fn load_signing_key() -> Result<SecretKey, CliError> {
    let raw = std::env::var(KEY_ENV).map_err(|_| {
        CliError::usage(format!(
            "{KEY_ENV} is not set. Generate a key with `tpk keygen`, then:\n  \
             export {KEY_ENV}=\"$(cat signing.key)\"\n\
             In CI, store the base64 body as a secret rather than writing the key to disk."
        ))
    })?;
    SecretKey::parse(&raw).map_err(|e| CliError::usage(format!("{KEY_ENV}: {e}")))
}

/// Write `signature` next to `path` as `<path>.minisig`.
///
/// # Errors
///
/// Returns [`CliError::usage`] if the file cannot be written.
pub fn write_detached_signature(path: &std::path::Path, signature: &str) -> CliResult {
    let sig_path = path.with_file_name(format!(
        "{}.minisig",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&sig_path, signature)
        .map_err(|e| CliError::usage(format!("cannot write {}: {e}", sig_path.display())))?;
    println!("signed   {}", sig_path.display());
    Ok(())
}
