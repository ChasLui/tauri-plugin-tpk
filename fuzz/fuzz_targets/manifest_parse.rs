//! `PackManifest::parse` holds every cross-field rule in the format. It runs on
//! bytes that have been signature-checked, but a manifest can also be
//! attacker-chosen in the window before verification, so it must not panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use tpk_format::manifest::PackManifest;

fuzz_target!(|data: &[u8]| {
    let Ok(manifest) = PackManifest::parse(data) else {
        return;
    };

    // Anything that parsed must satisfy the invariants the rest of the stack
    // relies on without re-checking them.
    for entry in &manifest.entries {
        use tpk_format::manifest::Op;
        match entry.op {
            Op::Delete => {
                assert!(entry.blob.is_none(), "a tombstone carries a blob");
                assert!(entry.sha256.is_none(), "a tombstone carries a digest");
            }
            Op::Full | Op::Delta => {
                let blob = entry.blob.as_ref().expect("a content entry needs a blob");
                let blob_sha = entry
                    .blob_sha256
                    .expect("a content entry needs a signature-covered blob digest");
                assert!(entry.sha256.is_some(), "a content entry needs a digest");
                assert!(entry.size.is_some(), "a content entry needs a size");
                assert!(
                    entry.blob_size.is_some(),
                    "a content entry needs a blob size"
                );
                // The blob's name must be its own digest, or the manifest
                // signs one object and the reader opens another.
                assert!(
                    blob.contains(&blob_sha.to_hex()),
                    "blob name {blob:?} does not match its digest"
                );
            }
        }
        if entry.op == Op::Delta {
            assert!(
                entry.delta_base_sha256.is_some(),
                "a delta with no base digest applies to anything"
            );
        }
    }
});
