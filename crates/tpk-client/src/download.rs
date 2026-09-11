//! Fetching pack files, with resume.
//!
//! Partial downloads land in `<cache>/tmp/<sha256>.part` and are only renamed
//! into place once the bytes hash to what the signed manifest said. A `.part`
//! file is therefore always safe to resume or to delete.

use std::path::{Path, PathBuf};

use tpk_format::channel::PackRef;
use tpk_format::sign::sha256_hex;

use crate::error::{redact, ClientError, Result};

/// Progress during a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes fetched so far for the current pack.
    pub downloaded: u64,
    /// Total bytes for the current pack.
    pub total: u64,
    /// Index of the pack being fetched.
    pub pack_index: usize,
    /// How many packs the plan covers.
    pub pack_count: usize,
}

/// A downloaded pack, verified against the manifest.
#[derive(Debug, Clone)]
pub struct DownloadedPack {
    /// The advertised entry.
    pub pack: PackRef,
    /// Where the verified bytes were written.
    pub path: PathBuf,
}

/// The `.part` file for a pack.
pub fn part_path(tmp_dir: &Path, pack: &PackRef) -> PathBuf {
    tmp_dir.join(format!("{}.part", pack.sha256))
}

/// How much of a pack is already on disk.
pub fn resume_offset(tmp_dir: &Path, pack: &PackRef) -> u64 {
    std::fs::metadata(part_path(tmp_dir, pack))
        .map(|m| m.len())
        .unwrap_or(0)
        // A part file longer than the declared size is nonsense; start over.
        .min(pack.size)
}

/// Verify a finished download and move it into place.
///
/// # Errors
///
/// Returns [`ClientError::Format`] when the bytes do not hash to what the
/// manifest advertised, and [`ClientError::Io`] on filesystem failure.
pub fn finalize(tmp_dir: &Path, out_dir: &Path, pack: &PackRef) -> Result<DownloadedPack> {
    let part = part_path(tmp_dir, pack);
    let bytes = std::fs::read(&part)?;

    if bytes.len() as u64 != pack.size {
        // Remove it: a truncated part would otherwise be resumed forever from
        // the wrong offset.
        let _ = std::fs::remove_file(&part);
        return Err(ClientError::Format(tpk_format::error::FormatError::Hash(
            format!(
                "{} is {} bytes, manifest said {}",
                redact(&pack.url),
                bytes.len(),
                pack.size
            ),
        )));
    }
    let actual = sha256_hex(&bytes);
    if actual != pack.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(ClientError::Format(tpk_format::error::FormatError::Hash(
            format!(
                "{} hashed to {actual}, expected {}",
                redact(&pack.url),
                pack.sha256
            ),
        )));
    }

    std::fs::create_dir_all(out_dir)?;
    let final_path = out_dir.join(format!("{}.tpk", pack.sha256));
    std::fs::rename(&part, &final_path)?;
    Ok(DownloadedPack {
        pack: pack.clone(),
        path: final_path,
    })
}

/// Delete every `.part` file that no longer corresponds to a wanted pack.
///
/// # Errors
///
/// Never fails; unreadable entries are skipped.
pub fn prune_parts(tmp_dir: &Path, wanted: &[PackRef]) -> usize {
    let Ok(entries) = std::fs::read_dir(tmp_dir) else {
        return 0;
    };
    let keep: std::collections::HashSet<String> = wanted
        .iter()
        .map(|p| format!("{}.part", p.sha256))
        .collect();

    entries
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".part") && !keep.contains(n))
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// Everything a download needs besides the pack itself.
pub struct DownloadRequest<'a> {
    /// HTTP client to reuse across packs.
    pub client: &'a reqwest::Client,
    /// Where `.part` files live.
    pub tmp_dir: &'a Path,
    /// Where verified packs are moved to.
    pub out_dir: &'a Path,
    /// Headers sent with the request.
    pub headers: &'a std::collections::HashMap<String, String>,
    /// Progress callback.
    pub on_progress: &'a (dyn Fn(Progress) + Send + Sync),
    /// Index of this pack within the plan.
    pub index: usize,
    /// How many packs the plan covers.
    pub count: usize,
}

/// Download one pack, resuming if a `.part` file is present.
///
/// # Errors
///
/// Returns [`ClientError::Network`] or [`ClientError::Http`] on transport
/// failure, [`ClientError::TooLarge`] if the response exceeds the declared
/// size, and [`ClientError::Format`] if the finished bytes do not verify.
pub async fn download_pack(req: &DownloadRequest<'_>, pack: &PackRef) -> Result<DownloadedPack> {
    use std::io::Write as _;

    let DownloadRequest {
        client,
        tmp_dir,
        out_dir,
        headers,
        on_progress,
        index,
        count,
    } = *req;

    std::fs::create_dir_all(tmp_dir)?;
    let part = part_path(tmp_dir, pack);
    let mut offset = resume_offset(tmp_dir, pack);

    if offset < pack.size {
        let mut request = client.get(&pack.url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if offset > 0 {
            request = request.header("Range", format!("bytes={offset}-"));
        }

        let response = request
            .send()
            .await
            .map_err(|e| ClientError::Network(format!("{}: {e}", redact(&pack.url))))?;

        let status = response.status();
        if !status.is_success() {
            return Err(ClientError::Http {
                status: status.as_u16(),
                message: redact(&pack.url),
            });
        }
        // A server that ignores Range answers 200 with the whole file; starting
        // over is correct, appending would corrupt it.
        let append = status.as_u16() == 206 && offset > 0;
        if !append {
            offset = 0;
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&part)?;

        let mut stream = response;
        let mut downloaded = offset;
        loop {
            let chunk = stream
                .chunk()
                .await
                .map_err(|e| ClientError::Network(e.to_string()))?;
            let Some(chunk) = chunk else { break };

            downloaded += chunk.len() as u64;
            if downloaded > pack.size {
                let _ = std::fs::remove_file(&part);
                return Err(ClientError::TooLarge {
                    what: redact(&pack.url),
                    limit: pack.size,
                });
            }
            file.write_all(&chunk)?;
            on_progress(Progress {
                downloaded,
                total: pack.size,
                pack_index: index,
                pack_count: count,
            });
        }
        file.sync_all()?;
    }

    finalize(tmp_dir, out_dir, pack)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpk_format::manifest::{PackId, PackKind};

    fn pack(content: &[u8]) -> PackRef {
        PackRef {
            id: PackId::parse("core").unwrap(),
            kind: PackKind::Base,
            version: "1.0.0".parse().unwrap(),
            version_code: 100,
            parent_version_code: None,
            url: "https://cdn.example.com/tpk/core/base.tpk?token=secret".to_string(),
            size: content.len() as u64,
            sha256: sha256_hex(content),
            optional: false,
            rollout: 100,
        }
    }

    #[test]
    fn a_complete_part_file_is_moved_into_place() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        let out = dir.path().join("layers");
        std::fs::create_dir_all(&tmp).unwrap();

        let content = b"the pack bytes";
        let p = pack(content);
        std::fs::write(part_path(&tmp, &p), content).unwrap();

        let done = finalize(&tmp, &out, &p).unwrap();
        assert!(done.path.exists());
        assert_eq!(std::fs::read(&done.path).unwrap(), content);
        assert!(!part_path(&tmp, &p).exists(), "the part file was consumed");
    }

    #[test]
    fn a_hash_mismatch_is_refused_and_the_part_removed() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();

        let p = pack(b"the real content");
        // Same length, different bytes.
        std::fs::write(part_path(&tmp, &p), b"the fake content").unwrap();

        let err = finalize(&tmp, &dir.path().join("layers"), &p).unwrap_err();
        assert!(matches!(err, ClientError::Format(_)), "{err}");
        assert!(
            !part_path(&tmp, &p).exists(),
            "a bad part must not be resumed forever"
        );
    }

    #[test]
    fn a_truncated_part_is_refused_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();

        let p = pack(b"the full content");
        std::fs::write(part_path(&tmp, &p), b"the full").unwrap();

        assert!(finalize(&tmp, &dir.path().join("layers"), &p).is_err());
        assert!(!part_path(&tmp, &p).exists());
    }

    #[test]
    fn the_url_token_never_reaches_an_error_message() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();

        let p = pack(b"the real content");
        std::fs::write(part_path(&tmp, &p), b"the fake content").unwrap();
        let err = finalize(&tmp, &dir.path().join("layers"), &p)
            .unwrap_err()
            .to_string();
        assert!(!err.contains("secret"), "{err}");
        assert!(err.contains("<redacted>"), "{err}");
    }

    #[test]
    fn resume_reports_what_is_already_there() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().to_path_buf();
        let p = pack(b"0123456789");

        assert_eq!(resume_offset(&tmp, &p), 0, "nothing yet");
        std::fs::write(part_path(&tmp, &p), b"01234").unwrap();
        assert_eq!(resume_offset(&tmp, &p), 5);
    }

    #[test]
    fn an_overlong_part_reports_a_full_offset_rather_than_more() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().to_path_buf();
        let p = pack(b"0123456789");
        std::fs::write(part_path(&tmp, &p), b"0123456789EXTRA").unwrap();

        // Never report past the declared size: a Range request from there would
        // be nonsense, and `finalize` will reject the file anyway.
        assert_eq!(resume_offset(&tmp, &p), 10);
    }

    #[test]
    fn pruning_removes_only_unwanted_parts() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().to_path_buf();

        let wanted = pack(b"wanted");
        let stale = pack(b"stale from an abandoned plan");
        std::fs::write(part_path(&tmp, &wanted), b"partial").unwrap();
        std::fs::write(part_path(&tmp, &stale), b"partial").unwrap();

        assert_eq!(prune_parts(&tmp, std::slice::from_ref(&wanted)), 1);
        assert!(part_path(&tmp, &wanted).exists());
        assert!(!part_path(&tmp, &stale).exists());
    }

    #[test]
    fn pruning_a_missing_directory_is_harmless() {
        assert_eq!(prune_parts(Path::new("/nonexistent/tmp"), &[]), 0);
    }
}
