//! Manifest parsing runs once per layer during `setup`, before the first window
//! exists, and it holds every cross-field rule in the format. Its cost scales
//! with entry count, which makes it the dominant term in startup time for a
//! large frontend.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpk_format::manifest::PackManifest;

fn manifest_json(entries: usize) -> Vec<u8> {
    let mut items = Vec::with_capacity(entries);
    // Zero-padded: entries must be sorted by path, and `chunk-10` sorts before
    // `chunk-9` lexicographically.
    for i in 0..entries {
        // A distinct, valid digest per entry: the blob name must match it, so a
        // shared constant would be rejected as a duplicate blob.
        let sha = format!("{:064x}", i + 1);
        items.push(format!(
            r#"{{"path":"/assets/chunk-{i:06}.js","op":"full","size":1024,
               "sha256":"{sha}","blob":"blobs/{sha}.zst","blob_sha256":"{sha}",
               "blob_size":512,"encoding":"zstd"}}"#
        ));
    }

    format!(
        r#"{{"spec":"tpk/1","kind":"base","id":"core","version":"1.0.0",
            "version_code":20260911120000,"created_at":"2026-09-11T12:00:00Z",
            "entries":[{}]}}"#,
        items.join(",")
    )
    .into_bytes()
}

fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("manifest_parse");

    for entries in [64usize, 1024, 8192] {
        let json = manifest_json(entries);
        PackManifest::parse(&json).expect("the fixture must be a valid manifest");

        group.throughput(Throughput::Elements(entries as u64));
        group.bench_with_input(BenchmarkId::from_parameter(entries), &json, |b, json| {
            b.iter(|| PackManifest::parse(std::hint::black_box(json)).expect("parse"));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
