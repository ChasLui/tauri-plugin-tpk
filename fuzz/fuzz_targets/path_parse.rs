//! `PackPath::parse` is the zip-slip boundary: every path in a manifest goes
//! through it before anything else touches it. It must never panic, and
//! whatever it accepts must survive a round trip through its own string form.
#![no_main]

use libfuzzer_sys::fuzz_target;
use tpk_format::path::PackPath;

fuzz_target!(|data: &str| {
    let Ok(path) = PackPath::parse(data) else {
        return;
    };

    // An accepted path must not be re-interpretable: reparsing its own string
    // form has to yield the same path, or the value a signature covers is not
    // the value the resolver looks up.
    let round_tripped = PackPath::parse(path.as_str()).expect("accepted path must reparse");
    assert_eq!(path.as_str(), round_tripped.as_str());

    let s = path.as_str();
    assert!(s.starts_with('/'), "accepted a relative path: {s:?}");
    assert!(!s.contains('\\'), "accepted a backslash: {s:?}");
    assert!(!s.contains('\0'), "accepted a NUL: {s:?}");
    assert!(
        !s.split('/').any(|seg| seg == ".." || seg == "."),
        "accepted a traversal segment: {s:?}"
    );
    assert!(!s.contains("//"), "accepted an empty segment: {s:?}");
});
