//! Signature verification and content hashing.
//!
//! Signatures are minisign (Ed25519). Trust is rooted in the public keys
//! compiled into the shell; each key carries an `epoch`, and clients keep a
//! monotonic `min_key_epoch` floor so a leaked key can be retired without
//! waiting for every device to drop it from its list.

use sha2::{Digest, Sha256};

use crate::error::{FormatError, Result};
use crate::manifest::Sha256Hex;

/// SHA-256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> Sha256Hex {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Sha256Hex::from_bytes(hasher.finalize().into())
}

/// Check content against an expected digest.
///
/// # Errors
///
/// Returns [`FormatError::Hash`] with both digests when they differ.
pub fn verify_sha256(bytes: &[u8], expected: &Sha256Hex) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual == *expected {
        Ok(())
    } else {
        Err(FormatError::Hash(format!(
            "expected {expected}, got {actual}"
        )))
    }
}

/// One trusted public key and the generation it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey {
    /// The minisign public key, base64 as written in `tauri.conf.json`.
    pub key: String,
    /// Generation. Higher supersedes lower; see [`TrustStore::verify`].
    pub epoch: u32,
}

/// The set of keys a shell trusts.
pub struct TrustStore {
    keys: Vec<(u32, minisign_verify::PublicKey)>,
}

impl std::fmt::Debug for TrustStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // PublicKey has no Debug; print the shape, not the material.
        f.debug_struct("TrustStore")
            .field("key_count", &self.keys.len())
            .field(
                "epochs",
                &self.keys.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl TrustStore {
    /// Build a trust store from configured keys.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Signature`] when the list is empty, a key is not
    /// valid minisign base64, or an epoch is zero.
    pub fn new(keys: &[TrustedKey]) -> Result<Self> {
        if keys.is_empty() {
            return Err(FormatError::Signature("no public keys configured".into()));
        }
        let mut parsed = Vec::with_capacity(keys.len());
        for entry in keys {
            if entry.epoch == 0 {
                return Err(FormatError::Signature("key epoch must be non-zero".into()));
            }
            let key = minisign_verify::PublicKey::from_base64(&entry.key)
                .map_err(|e| FormatError::Signature(format!("invalid public key: {e}")))?;
            parsed.push((entry.epoch, key));
        }
        Ok(Self { keys: parsed })
    }

    /// The highest epoch present in the store.
    pub fn max_epoch(&self) -> u32 {
        self.keys.iter().map(|(e, _)| *e).max().unwrap_or(0)
    }

    /// Verify `payload` against `signature`, requiring the declared epoch.
    ///
    /// Only keys whose epoch equals `declared_epoch` are tried. Accepting any
    /// key regardless of epoch would make the epoch field decorative: a holder
    /// of a leaked epoch-1 key could sign a manifest claiming `key_epoch: 2`
    /// and clients would advance their floor on a signature it never covered.
    ///
    /// `min_epoch` is the client's monotonic floor; anything below it is refused
    /// even if a matching key is still in the list.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Signature`] when the epoch is below the floor, no
    /// key matches the declared epoch, the signature is malformed, or no
    /// matching key verifies it.
    pub fn verify(
        &self,
        payload: &[u8],
        signature: &str,
        declared_epoch: u32,
        min_epoch: u32,
    ) -> Result<()> {
        if declared_epoch < min_epoch {
            return Err(FormatError::Signature(format!(
                "key epoch {declared_epoch} is below the trusted floor {min_epoch}"
            )));
        }
        // Resolve the epoch before touching attacker-controlled signature bytes,
        // so an unknown epoch reports as such instead of as a parse failure.
        let candidates: Vec<_> = self
            .keys
            .iter()
            .filter(|(epoch, _)| *epoch == declared_epoch)
            .map(|(_, key)| key)
            .collect();
        if candidates.is_empty() {
            return Err(FormatError::Signature(format!(
                "no trusted key for epoch {declared_epoch}"
            )));
        }

        let signature = decode_signature(signature)?;
        for key in candidates {
            if key.verify(payload, &signature, false).is_ok() {
                return Ok(());
            }
        }
        Err(FormatError::Signature(
            "no trusted key verified the signature".into(),
        ))
    }
}

/// Accept either a raw minisign `.sig` document or its base64 wrapping.
///
/// Publishers routinely paste signatures into JSON, where the two-line `.sig`
/// format is awkward; both spellings describe the same bytes.
fn decode_signature(raw: &str) -> Result<minisign_verify::Signature> {
    let text = if raw.trim_start().starts_with("untrusted comment:") {
        raw.to_string()
    } else {
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(raw.trim())
            .map_err(|e| FormatError::Signature(format!("signature is not base64: {e}")))?;
        String::from_utf8(decoded)
            .map_err(|e| FormatError::Signature(format!("signature is not UTF-8: {e}")))?
    };
    minisign_verify::Signature::decode(&text)
        .map_err(|e| FormatError::Signature(format!("malformed signature: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated with `minisign -G`; the matching secret key is not in the repo.
    const PUBKEY: &str = "RWSMvzB0jPz2SCJl9hDwJnzBrCE3xAFkUQPGcAlnsIKbLAvBxUxJoAVQ";

    fn store(pairs: &[(&str, u32)]) -> TrustStore {
        TrustStore::new(
            &pairs
                .iter()
                .map(|(k, e)| TrustedKey {
                    key: (*k).to_string(),
                    epoch: *e,
                })
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn sha256_matches_known_vector() {
        // The canonical empty-input digest.
        assert_eq!(
            sha256_hex(b"").to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_sha256_reports_both_digests() {
        let expected = sha256_hex(b"abc");
        assert!(verify_sha256(b"abc", &expected).is_ok());
        let err = verify_sha256(b"abd", &expected).unwrap_err().to_string();
        assert!(err.contains("expected"), "{err}");
        assert!(err.contains("got"), "{err}");
    }

    #[test]
    fn rejects_empty_key_list() {
        assert!(TrustStore::new(&[]).is_err());
    }

    #[test]
    fn rejects_zero_epoch() {
        assert!(TrustStore::new(&[TrustedKey {
            key: PUBKEY.to_string(),
            epoch: 0
        }])
        .is_err());
    }

    #[test]
    fn rejects_malformed_public_key() {
        assert!(TrustStore::new(&[TrustedKey {
            key: "not-a-key".to_string(),
            epoch: 1
        }])
        .is_err());
    }

    #[test]
    fn max_epoch_reports_the_newest_generation() {
        assert_eq!(store(&[(PUBKEY, 1), (PUBKEY, 3)]).max_epoch(), 3);
    }

    #[test]
    fn epoch_below_the_floor_is_refused() {
        let s = store(&[(PUBKEY, 1)]);
        let err = s
            .verify(b"payload", "ignored", 1, 2)
            .unwrap_err()
            .to_string();
        assert!(err.contains("below the trusted floor"), "{err}");
    }

    #[test]
    fn unknown_epoch_is_refused_before_looking_at_the_signature() {
        let s = store(&[(PUBKEY, 1)]);
        // Epoch 2 has no key, so a holder of the epoch-1 key cannot get a
        // manifest claiming epoch 2 accepted.
        let err = s
            .verify(b"payload", "garbage", 2, 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no trusted key for epoch 2"), "{err}");
    }

    #[test]
    fn malformed_signature_is_rejected() {
        let s = store(&[(PUBKEY, 1)]);
        let err = s.verify(b"payload", "!!!not base64!!!", 1, 1).unwrap_err();
        assert!(matches!(err, FormatError::Signature(_)));
    }

    #[test]
    fn wrong_signature_does_not_verify() {
        let s = store(&[(PUBKEY, 1)]);
        // Well-formed minisign document, but not a signature over this payload.
        let sig = "untrusted comment: signature from minisign secret key\n\
                   RWQf6LRCGA9i53mlYecO4IzK/hR9Y8tOxQ1JNS9r0DJ0oGnVBCBcyb5c\n\
                   trusted comment: timestamp:0\n\
                   AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n";
        assert!(s.verify(b"payload", sig, 1, 1).is_err());
    }
}
