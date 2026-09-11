//! `tpk_delta::apply` runs a third-party bsdiff decoder over a patch stream
//! taken from a layer on disk. A corrupt layer must degrade to "skip this
//! layer" — never to a process abort, and never to an allocation past `limit`.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
struct Input<'a> {
    base: &'a [u8],
    patch: &'a [u8],
    expect_out_len: u16,
}

fuzz_target!(|input: Input<'_>| {
    // A real limit, so an over-large `expect_out_len` is rejected rather than
    // letting the fuzzer OOM on a value the signed manifest would never carry.
    const LIMIT: usize = 1 << 20;

    let expect = usize::from(input.expect_out_len);
    if let Ok(out) = tpk_delta::apply(input.base, input.patch, expect, LIMIT) {
        assert_eq!(
            out.len(),
            expect,
            "apply succeeded with a length other than the one it was promised"
        );
    }
});
