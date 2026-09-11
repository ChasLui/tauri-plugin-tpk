# Gates: get CI green, and verify what CI cannot

OWNS: crates/**, Cargo.lock, Cargo.toml, deny.toml, .github/workflows/**, GATES.md, scripts/**

Scope: every job in the CI workflow passes on GitHub, and the cross-platform
determinism the format depends on is measured on a second OS rather than
assumed.

- [ ] G1: clippy is clean under the toolchain CI actually uses, not just the
      local one — CI stable is 1.98 and raised `chunks_exact_to_as_chunks`,
      which local 1.95 does not have
  CHECK: node scripts/gate-clippy.mjs
  EXPECT: GATE_CLIPPY_PASS
  EVIDENCE: pending

- [ ] G2: `cargo deny check` passes every check, including `advisories` —
      the local runs only ever passed `licenses bans`, so five RUSTSEC
      vulnerabilities in transitive dependencies were never seen
  CHECK: node scripts/gate-deny.mjs
  EXPECT: GATE_DENY_PASS
  EVIDENCE: pending

- [ ] G3: the workspace test suite still passes after the lockfile bump
  CHECK: node scripts/gate-tests.mjs
  EXPECT: GATE_TESTS_PASS
  EVIDENCE: pending

- [ ] G4: a pack built on Windows 11 is byte-identical to one built on macOS
      from the same input, `--version-code` and `--created-at`. This is the
      determinism the blacklist and the channel manifest digest depend on, and
      no CI job compares artifacts across runners
  CHECK: node scripts/gate-cross-platform-determinism.mjs
  EXPECT: GATE_DETERMINISM_PASS
  EVIDENCE: pending

- [ ] G5: every job of the CI workflow succeeds on the pushed commit
  CHECK: node scripts/gate-ci-green.mjs
  EXPECT: GATE_CI_GREEN_PASS
  EVIDENCE: pending

- [ ] G6: the Deploy Docs workflow succeeds
  EVIDENCE: pending

<!--
G6 is manual because it is blocked on a repository setting (GitHub Pages is not
enabled on the new repo), which is an outward-facing change and the owner's
call, not something a check should make true on its own.

G4's checker must fail honestly. Before trusting a byte-equality pass, it
perturbs one input (--created-at) and asserts the digests then differ, so a
checker that compared nothing cannot report success.
-->
