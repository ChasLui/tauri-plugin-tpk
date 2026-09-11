//! Minisign secret keys and signature production (`pack` feature).
//!
//! Build-machine only. The runtime never links this: verification needs a
//! public key and nothing else.
//!
//! Only unencrypted secret keys are supported — generate one with
//! `minisign -G -W`. A CI secret store already encrypts what it holds, so
//! wrapping the key in a second passphrase that has to live in the same store
//! buys nothing, and skipping scrypt keeps a KDF out of the dependency graph.

use base64::Engine as _;
use blake2::digest::consts::U32;
use blake2::{Blake2b, Blake2b512, Digest};

use crate::error::{FormatError, Result};

type Blake2b256 = Blake2b<U32>;

/// Tag stored in a secret key file.
const SIG_ALG: [u8; 2] = *b"Ed";
/// Tag written into signatures. Minisign's default is the prehashed variant,
/// which signs BLAKE2b-512 of the payload rather than the payload itself;
/// verifiers reject the legacy `Ed` form unless explicitly opted in.
const SIG_ALG_PREHASHED: [u8; 2] = *b"ED";
const KDF_ALG_NONE: [u8; 2] = [0, 0];
const KDF_ALG_SCRYPT: [u8; 2] = *b"Sc";
const CKSUM_ALG: [u8; 2] = *b"B2";
const SECRET_KEY_LEN: usize = 158;

/// A minisign secret key, held in memory only for the duration of a signing run.
pub struct SecretKey {
    key_id: [u8; 8],
    keypair: ed25519_compact::KeyPair,
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material, not even truncated.
        f.debug_struct("SecretKey")
            .field("key_id", &hex8(&self.key_id))
            .finish()
    }
}

impl SecretKey {
    /// Generate a fresh key pair.
    ///
    /// The result is unencrypted; store the serialized form in a secret manager
    /// rather than on disk.
    pub fn generate() -> Self {
        let keypair = ed25519_compact::KeyPair::generate();
        let mut key_id = [0u8; 8];
        // The key id only has to distinguish keys, not be unpredictable;
        // deriving it from the public key keeps generation dependency-free.
        key_id.copy_from_slice(&keypair.pk.as_ref()[..8]);
        Self { key_id, keypair }
    }

    /// Serialize as a minisign `.key` file (unencrypted).
    pub fn to_key_file(&self, comment: &str) -> String {
        let secret: [u8; 64] = self
            .keypair
            .sk
            .as_ref()
            .try_into()
            .expect("Ed25519 secret keys are 64 bytes");
        let mut bytes = Vec::with_capacity(SECRET_KEY_LEN);
        bytes.extend_from_slice(&SIG_ALG);
        bytes.extend_from_slice(&KDF_ALG_NONE);
        bytes.extend_from_slice(&CKSUM_ALG);
        bytes.extend_from_slice(&[0u8; 48]); // salt + opslimit + memlimit, unused
        bytes.extend_from_slice(&self.key_id);
        bytes.extend_from_slice(&secret);
        bytes.extend_from_slice(&keynum_checksum(&SIG_ALG, &self.key_id, &secret));
        debug_assert_eq!(bytes.len(), SECRET_KEY_LEN);
        format!(
            "untrusted comment: {comment}\n{}\n",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    /// Parse a `.key` file's contents.
    ///
    /// Accepts either the two-line minisign file or the bare base64 body, so
    /// the same value works from a file, a single-line CI secret, or a paste.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Signature`] for malformed input, an unsupported
    /// algorithm, a failed checksum, or a passphrase-encrypted key.
    pub fn parse(raw: &str) -> Result<Self> {
        let body = raw
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
            .ok_or_else(|| FormatError::Signature("secret key is empty".into()))?;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|e| FormatError::Signature(format!("secret key is not base64: {e}")))?;
        if bytes.len() != SECRET_KEY_LEN {
            return Err(FormatError::Signature(format!(
                "secret key is {} bytes, expected {SECRET_KEY_LEN}",
                bytes.len()
            )));
        }

        let sig_alg: [u8; 2] = bytes[0..2].try_into().expect("length checked");
        let kdf_alg: [u8; 2] = bytes[2..4].try_into().expect("length checked");
        let cksum_alg: [u8; 2] = bytes[4..6].try_into().expect("length checked");

        if sig_alg != SIG_ALG {
            return Err(FormatError::Signature(format!(
                "unsupported signature algorithm {sig_alg:?}"
            )));
        }
        if cksum_alg != CKSUM_ALG {
            return Err(FormatError::Signature(format!(
                "unsupported checksum algorithm {cksum_alg:?}"
            )));
        }
        if kdf_alg == KDF_ALG_SCRYPT {
            return Err(FormatError::Signature(
                "this key is passphrase-encrypted; export an unencrypted one with `minisign -G -W`"
                    .into(),
            ));
        }
        if kdf_alg != KDF_ALG_NONE {
            return Err(FormatError::Signature(format!(
                "unsupported key derivation algorithm {kdf_alg:?}"
            )));
        }

        // Layout after the three algorithm tags: salt[32] || opslimit[8] ||
        // memlimit[8] || key_id[8] || secret[64] || checksum[32]. All the KDF
        // fields are zero for an unencrypted key and unused here.
        let key_id: [u8; 8] = bytes[54..62].try_into().expect("length checked");
        let secret: [u8; 64] = bytes[62..126].try_into().expect("length checked");
        let checksum: [u8; 32] = bytes[126..158].try_into().expect("length checked");

        let expected = keynum_checksum(&sig_alg, &key_id, &secret);
        if checksum != expected {
            return Err(FormatError::Signature(
                "secret key checksum mismatch; the key file is corrupt".into(),
            ));
        }

        let keypair = ed25519_compact::KeyPair::from_slice(&secret)
            .map_err(|e| FormatError::Signature(format!("invalid Ed25519 key: {e}")))?;

        Ok(Self { key_id, keypair })
    }

    /// The matching public key, base64 as it appears in `tauri.conf.json`.
    pub fn public_key_base64(&self) -> String {
        let mut out = Vec::with_capacity(42);
        out.extend_from_slice(&SIG_ALG);
        out.extend_from_slice(&self.key_id);
        out.extend_from_slice(self.keypair.pk.as_ref());
        base64::engine::general_purpose::STANDARD.encode(out)
    }

    /// Sign `payload`, producing a minisign `.sig` document.
    ///
    /// `trusted_comment` is covered by the global signature, so it cannot be
    /// altered without detection; `untrusted_comment` is not.
    pub fn sign(&self, payload: &[u8], trusted_comment: &str, untrusted_comment: &str) -> String {
        let prehashed = Blake2b512::digest(payload);
        let signature = self.keypair.sk.sign(prehashed, None);

        let mut global_payload = Vec::with_capacity(64 + trusted_comment.len());
        global_payload.extend_from_slice(signature.as_ref());
        global_payload.extend_from_slice(trusted_comment.as_bytes());
        let global_signature = self.keypair.sk.sign(&global_payload, None);

        let mut sig_blob = Vec::with_capacity(74);
        sig_blob.extend_from_slice(&SIG_ALG_PREHASHED);
        sig_blob.extend_from_slice(&self.key_id);
        sig_blob.extend_from_slice(signature.as_ref());

        let b64 = base64::engine::general_purpose::STANDARD;
        format!(
            "untrusted comment: {untrusted_comment}\n{}\ntrusted comment: {trusted_comment}\n{}\n",
            b64.encode(sig_blob),
            b64.encode(global_signature.as_ref()),
        )
    }
}

fn keynum_checksum(sig_alg: &[u8; 2], key_id: &[u8; 8], secret: &[u8; 64]) -> [u8; 32] {
    let mut hasher = Blake2b256::new();
    hasher.update(sig_alg);
    hasher.update(key_id);
    hasher.update(secret);
    hasher.finalize().into()
}

fn hex8(bytes: &[u8; 8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign::{TrustStore, TrustedKey};

    /// A deterministic secret key file, so failures are reproducible.
    fn make_key(seed: [u8; 32]) -> String {
        let keypair = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new(seed));
        let key_id = [1u8, 2, 3, 4, 5, 6, 7, 8];
        SecretKey { key_id, keypair }.to_key_file("minisign encrypted secret key")
    }

    #[test]
    fn signs_what_minisign_verify_accepts() {
        let sk = SecretKey::parse(&make_key([42u8; 32])).unwrap();
        let payload = b"the exact manifest bytes";
        let sig = sk.sign(payload, "timestamp:1757600000", "signature from tpk");

        let trust = TrustStore::new(&[TrustedKey {
            key: sk.public_key_base64(),
            epoch: 1,
        }])
        .unwrap();
        trust
            .verify(payload, &sig, 1, 1)
            .expect("our own signature must verify");
    }

    #[test]
    fn a_tampered_payload_does_not_verify() {
        let sk = SecretKey::parse(&make_key([7u8; 32])).unwrap();
        let sig = sk.sign(b"original", "timestamp:0", "c");
        let trust = TrustStore::new(&[TrustedKey {
            key: sk.public_key_base64(),
            epoch: 1,
        }])
        .unwrap();
        assert!(trust.verify(b"tampered", &sig, 1, 1).is_err());
    }

    #[test]
    fn a_different_key_does_not_verify() {
        let signer = SecretKey::parse(&make_key([1u8; 32])).unwrap();
        let other = SecretKey::parse(&make_key([2u8; 32])).unwrap();
        let sig = signer.sign(b"payload", "timestamp:0", "c");
        let trust = TrustStore::new(&[TrustedKey {
            key: other.public_key_base64(),
            epoch: 1,
        }])
        .unwrap();
        assert!(trust.verify(b"payload", &sig, 1, 1).is_err());
    }

    #[test]
    fn key_rotation_accepts_the_declared_epoch_only() {
        let k1 = SecretKey::parse(&make_key([11u8; 32])).unwrap();
        let k2 = SecretKey::parse(&make_key([22u8; 32])).unwrap();
        let trust = TrustStore::new(&[
            TrustedKey {
                key: k1.public_key_base64(),
                epoch: 1,
            },
            TrustedKey {
                key: k2.public_key_base64(),
                epoch: 2,
            },
        ])
        .unwrap();

        let sig1 = k1.sign(b"payload", "timestamp:0", "c");
        let sig2 = k2.sign(b"payload", "timestamp:0", "c");

        assert!(trust.verify(b"payload", &sig1, 1, 1).is_ok());
        assert!(trust.verify(b"payload", &sig2, 2, 1).is_ok());

        // The whole point of the epoch: a holder of the retired key cannot get
        // a document claiming the new epoch accepted.
        assert!(trust.verify(b"payload", &sig1, 2, 1).is_err());

        // And once the floor moves up, the old key is dead even though it is
        // still listed.
        assert!(trust.verify(b"payload", &sig1, 1, 2).is_err());
    }

    #[test]
    fn accepts_a_bare_base64_body() {
        let full = make_key([5u8; 32]);
        let bare = full.lines().nth(1).unwrap();
        let from_full = SecretKey::parse(&full).unwrap();
        let from_bare = SecretKey::parse(bare).unwrap();
        assert_eq!(from_full.public_key_base64(), from_bare.public_key_base64());
    }

    #[test]
    fn rejects_a_passphrase_encrypted_key() {
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(make_key([3u8; 32]).lines().nth(1).unwrap())
            .unwrap();
        bytes[2..4].copy_from_slice(&KDF_ALG_SCRYPT);
        let doc = base64::engine::general_purpose::STANDARD.encode(bytes);
        let err = SecretKey::parse(&doc).unwrap_err().to_string();
        assert!(err.contains("minisign -G -W"), "{err}");
    }

    #[test]
    fn rejects_a_corrupt_checksum() {
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(make_key([9u8; 32]).lines().nth(1).unwrap())
            .unwrap();
        bytes[130] ^= 0xff;
        let doc = base64::engine::general_purpose::STANDARD.encode(bytes);
        let err = SecretKey::parse(&doc).unwrap_err().to_string();
        assert!(err.contains("checksum"), "{err}");
    }

    #[test]
    fn rejects_wrong_length_and_garbage() {
        assert!(SecretKey::parse("").is_err());
        assert!(SecretKey::parse("not base64!!!").is_err());
        assert!(SecretKey::parse("QUJD").is_err(), "too short");
    }

    #[test]
    fn generated_keys_round_trip_through_the_key_file() {
        let sk = SecretKey::generate();
        let reloaded = SecretKey::parse(&sk.to_key_file("test")).unwrap();
        assert_eq!(sk.public_key_base64(), reloaded.public_key_base64());

        let sig = reloaded.sign(b"payload", "timestamp:0", "c");
        let trust = TrustStore::new(&[TrustedKey {
            key: sk.public_key_base64(),
            epoch: 1,
        }])
        .unwrap();
        assert!(trust.verify(b"payload", &sig, 1, 1).is_ok());
    }

    #[test]
    fn two_generated_keys_differ() {
        assert_ne!(
            SecretKey::generate().public_key_base64(),
            SecretKey::generate().public_key_base64()
        );
    }

    #[test]
    fn debug_never_prints_key_material() {
        let sk = SecretKey::parse(&make_key([13u8; 32])).unwrap();
        let shown = format!("{sk:?}");
        assert!(shown.contains("0102030405060708"));
        assert!(!shown.contains("keypair"), "{shown}");
    }
}
