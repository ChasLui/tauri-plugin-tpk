//! `UnverifiedPack::open` is the first thing that touches a downloaded file. It
//! runs *before* any signature is checked, on bytes an attacker fully controls,
//! which makes it the highest-value target here.
#![no_main]

use std::io::Write;

use libfuzzer_sys::fuzz_target;
use tpk_format::container::UnverifiedPack;

fuzz_target!(|data: &[u8]| {
    // `open` takes a path because it needs `Seek` over a real file; there is no
    // reader-based constructor to fuzz against, so pay for a temp file.
    let Ok(mut file) = tempfile::NamedTempFile::new() else {
        return;
    };
    if file.write_all(data).is_err() || file.flush().is_err() {
        return;
    }

    // Only that it does not panic, abort or hang. Everything it accepts here is
    // still unverified — the guarantees start at `verify()`.
    let _ = UnverifiedPack::open(file.path());
});
