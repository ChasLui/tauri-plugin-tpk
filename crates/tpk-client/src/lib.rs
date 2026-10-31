//! Channel manifest fetching, update planning and resumable downloads.
//!
//! Deliberately independent of `tpk-store`: [`plan::plan`] is a pure function
//! over a verified manifest and what the device already has, so every rule it
//! enforces can be tested without a disk or a network.
//!
//! See `spec/tpk-v1.md` section 4.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod download;
pub mod error;
pub mod plan;
pub mod source;
mod transport;

pub use download::{DownloadRequest, DownloadedPack, Progress};
pub use error::{redact, ClientError, Result};
pub use plan::{in_rollout, plan, Plan, PlanContext, Skipped, SkippedPack};
pub use source::{expand_url, ChannelSource, FetchContext, HttpChannelSource, SignedBytes};
pub use transport::runtime_http_client;

use std::sync::Arc;

use tpk_format::channel::ChannelManifest;
use tpk_format::sign::TrustStore;

/// Verify a fetched channel manifest and check it is fresh enough.
///
/// The order matters: the document is parsed first so a malformed one reports
/// as malformed rather than as a signature failure, and because the epoch the
/// signature must be checked against is inside it.
///
/// # Errors
///
/// Returns [`ClientError::Format`] when the document or its signature is bad,
/// and [`ClientError::Watermark`] when it is older than one already seen on this
/// channel.
pub fn verify_channel(
    signed: &SignedBytes,
    trust: &Arc<TrustStore>,
    min_key_epoch: u32,
    watermark_floor: u64,
) -> Result<ChannelManifest> {
    verify_channel_inner(signed, trust, min_key_epoch, watermark_floor, None)
}

/// Verify signature, expected channel identity, and freshness before persisting a watermark.
///
/// Unlike [`verify_channel`], this binds a document to the configured channel, even
/// when several channels share a signing key. No persistent state is modified.
///
/// # Errors
///
/// Returns `E_SPEC` for a channel mismatch, otherwise the same errors as [`verify_channel`].
pub fn verify_channel_for(
    signed: &SignedBytes,
    trust: &Arc<TrustStore>,
    min_key_epoch: u32,
    watermark_floor: u64,
    expected_channel: &str,
) -> Result<ChannelManifest> {
    verify_channel_inner(
        signed,
        trust,
        min_key_epoch,
        watermark_floor,
        Some(expected_channel),
    )
}

fn verify_channel_inner(
    signed: &SignedBytes,
    trust: &Arc<TrustStore>,
    min_key_epoch: u32,
    watermark_floor: u64,
    expected_channel: Option<&str>,
) -> Result<ChannelManifest> {
    let manifest = ChannelManifest::parse(&signed.body)?;
    trust.verify(
        &signed.body,
        &signed.signature,
        manifest.key_epoch,
        min_key_epoch,
    )?;

    if expected_channel.is_some_and(|expected| manifest.channel != expected) {
        return Err(ClientError::Format(tpk_format::error::FormatError::Spec(
            "manifest channel does not match the configured channel".into(),
        )));
    }
    if !manifest.is_fresh_enough(watermark_floor) {
        return Err(ClientError::Watermark {
            channel: manifest.channel.clone(),
            found: manifest.watermark,
            floor: watermark_floor,
        });
    }
    Ok(manifest)
}

#[cfg(test)]
mod test_http;
