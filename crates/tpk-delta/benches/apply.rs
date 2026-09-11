//! Delta application is on the staging path, not the boot path — but the whole
//! reason it was moved off the boot path is that it is slow enough to trip a
//! launch watchdog. This measures how slow.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

/// Pseudo-random but deterministic, so runs are comparable.
fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

/// A base with a small edit in the middle: what a real frontend hotfix looks
/// like, as opposed to two unrelated files.
fn edited(base: &[u8]) -> Vec<u8> {
    let mut out = base.to_vec();
    let mid = out.len() / 2;
    out.splice(mid..mid + 16, b"/* patched */   ".iter().copied());
    out
}

fn bench_apply(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_apply");

    for size in [64 * 1024usize, 512 * 1024, 2 * 1024 * 1024] {
        let base = pseudo_random(size, 0x5eed);
        let target = edited(&base);

        let mut patch = Vec::new();
        bsdiff::diff(&base, &target, &mut patch).expect("diff");

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                tpk_delta::apply(
                    std::hint::black_box(&base),
                    std::hint::black_box(&patch),
                    target.len(),
                    8 * 1024 * 1024,
                )
                .expect("apply")
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_apply);
criterion_main!(benches);
