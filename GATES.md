# Gates: get CI green, and verify what CI cannot

OWNS: crates/**, Cargo.lock, Cargo.toml, deny.toml, .github/workflows/**, GATES.md, scripts/**

Scope: every job in the CI workflow passes on GitHub, and the cross-platform
determinism the format depends on is measured on a second OS rather than
assumed.

- [x] G1: clippy is clean under the toolchain CI actually uses, not just the
      local one — CI stable is 1.98 and raised `chunks_exact_to_as_chunks`,
      which local 1.95 does not have
  CHECK: node scripts/gate-clippy.mjs
  EXPECT: GATE_CLIPPY_PASS
  EVIDENCE: gate-clippy under rustup stable 1.98 (CI's toolchain), workspace clean; control: same gate refused to certify under 1.94.1. Fix was Sha256Hex::parse -> as_chunks::<2>(), still compiles under MSRV 1.88.

- [x] G2: `cargo deny check` passes every check, including `advisories` —
      the local runs only ever passed `licenses bans`, so five RUSTSEC
      vulnerabilities in transitive dependencies were never seen
  CHECK: node scripts/gate-deny.mjs
  EXPECT: GATE_DENY_PASS
  EVIDENCE: `cargo deny check` -> advisories ok, bans ok, licenses ok, sources ok, after `cargo update`. Cleared RUSTSEC-2026-0258 (h2), -0194 and -0195 (quick-xml), -0098, -0099 and -0104 (rustls-webpki).

- [x] G3: the workspace test suite still passes after the lockfile bump
  CHECK: node scripts/gate-tests.mjs
  EXPECT: GATE_TESTS_PASS
  EVIDENCE: 342 tests passed (341 before, plus the G4 regression test).

- [x] G4: a pack built on Windows 11 is byte-identical to one built on macOS
      from the same input, `--version-code` and `--created-at`. This is the
      determinism the blacklist and the channel manifest digest depend on, and
      no CI job compares artifacts across runners
  CHECK: node scripts/gate-cross-platform-determinism.mjs
  EXPECT: GATE_DETERMINISM_PASS
  EVIDENCE: failed first, which is the point. host de57d9b4... vs vm 8cd8e666... for identical input; the diff was exactly 7 bytes, one per central directory header, at the 'version made by' host byte (0x03 Unix on macOS, 0x00 FAT on Windows). Fixed by pinning `.system(zip::System::Unix)` and `.unix_permissions(0o644)`. Re-run: host and vm both de57d9b4..., and both controls (created_at +1s) moved to 44040a6f... A CI-runnable regression test was added in pack.rs; it asserts the host byte and so bites on the windows-latest runner.

- [x] G5: every job of the CI workflow succeeds on the pushed commit
  CHECK: node scripts/gate-ci-green.mjs
  EXPECT: GATE_CI_GREEN_PASS
  EVIDENCE: commit 013d45d, 9/9 CI jobs succeeded — includes windows-latest
    and macos-latest, and the MSRV 1.88 job.

- [x] G6: the Deploy Docs workflow succeeds
  EVIDENCE: the repository had GitHub Pages disabled, so every Deploy Docs run
    failed with "404 ... Ensure GitHub Pages has been enabled". Enabled by the
    owner's instruction through the Settings UI (source: GitHub Actions);
    `gh api repos/ChasLui/tauri-plugin-tpk/pages` now reports
    build_type=workflow. Run 34588298125 succeeded, and the published site
    answers 200 on /, /overlay/ and /app-review-checklist/ — so the deploy is
    confirmed from the outside, not just from a green job.

<!--
G6 is manual because it is blocked on a repository setting (GitHub Pages is not
enabled on the new repo), which is an outward-facing change and the owner's
call, not something a check should make true on its own.

G4's checker must fail honestly. Before trusting a byte-equality pass, it
perturbs one input (--created-at) and asserts the digests then differ, so a
checker that compared nothing cannot report success.
-->
