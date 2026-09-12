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
  EVIDENCE: 347 tests passed (341 before, plus the G4 regression test and the
    five golden-fixture assertions).

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

- [x] G7: `cargo publish --workspace --dry-run` succeeds on a clean machine.
      It cannot be measured on this host: `~/.cargo/config.toml` replaces
      crates-io with an `rsproxy-sparse` mirror, CARGO_HOME is not honoured
      here, and the replacement cannot be unset through `--config`. So the
      measurement is moved to a CI job on a runner that has no mirror, which
      also makes it a permanent gate rather than a one-off check
  CHECK: node scripts/gate-ci-green.mjs
  EXPECT: GATE_CI_GREEN_PASS
  EVIDENCE: the new Packaging job ran `cargo publish --workspace --dry-run`
    (verbatim as release.yml does, including the gitignored README copy and no
    --allow-dirty) and succeeded. commit 8f11720: 10/10 CI jobs.

- [x] G8: the two open dependabot PRs are resolved by verifying each bump
      rather than by trusting a green test run — the crypto ones sit on the
      signature path, where a self-consistent suite proves nothing
  EVIDENCE: #6 (base64 0.23, sha2 0.11, blake2 0.11, criterion 0.8) applied to
    main directly, since the PR branch predates the `cargo update` and conflicts
    on Cargo.lock. Validated in an isolated worktree first: compiles, 342 tests
    pass, and — the part that actually matters — the committed golden fixture
    built under sha2/blake2 0.10 still verifies under 0.11, with all three of
    its controls still failing as designed. Separately confirmed a 0.10-built
    pack repacks byte-identically under 0.11.
    #1 was taken in half. `@tauri-apps/api` ^2.11.1 is in and builds. TypeScript
    5.9 -> 7.0 is NOT: tsup 8.5.1's DTS step dies on TS 7 with
    `Cannot read properties of undefined (reading 'useCaseSensitiveFileNames')`.
    Isolated against a pnpm-version confound — local corepack pnpm 11.24 blocks
    esbuild's install script and fails the baseline too, so the TS 7 failure was
    reproduced under pnpm 10, which is what CI pins.
    Resolved 2026-09-13: re-reproduced the TS 7.0.2 failure before acting on it
    (ESM builds, DTS dies), and confirmed tsup's latest is still 8.5.1 — there
    is nothing to upgrade to. Both PRs are closed; dependabot had auto-closed
    each once the corresponding dependency landed on main. Added an `ignore`
    rule for typescript majors in .github/dependabot.yml so the unmergeable PR
    stops being recreated weekly, with the removal condition and the
    `tsc --emitDeclarationOnly` alternative recorded in the comment. `gh pr
    list --state open` is now empty and no dependabot branches remain.

- [x] G9: the guest API builds under the toolchain CI actually uses and emits
      exactly the files `package.json` points at. TypeScript 7 is in, via
      tsdown replacing tsup
  CHECK: node scripts/gate-guest-js.mjs
  EXPECT: GATE_GUEST_JS_PASS
  EVIDENCE: tsdown 0.23.0 declares `typescript: ^5 || ^6 || ^7` and generates
    declarations through rolldown-plugin-dts, so it does not touch the compiler
    API TS 7 removed. Equivalence with the tsup output was checked rather than
    assumed: 8/8 runtime exports identical, 18/18 d.ts declarations identical,
    and a strict consumer (`skipLibCheck: false`) type-checks against the new
    d.ts under TS 7.0.2 — control: importing a nonexistent member yields TS2305.
    Known regression: the file-level module JSDoc no longer reaches the d.ts
    (per-declaration JSDoc does, and it survives in the .js).
    Two toolchain traps, both caught only after they had shipped or by this
    gate: tsdown defaults to `platform: node`, which turns on fixedExtension and
    emits .mjs/.d.mts — a "successful" build whose `exports` resolve to nothing;
    fixed with `--platform neutral`, which is also what this package is. And
    tsdown needs Node >= 22.18 while CI pinned Node 20 — pnpm does not enforce a
    transitive tool's engines, so install passed and the build died on
    `Promise.withResolvers is not a function`. Node pins moved to 22 and the
    requirement is declared as devEngines (not engines: the published runtime
    targets a WebView and has no Node requirement to impose on consumers).
    The gate reads the Node and pnpm versions out of ci.yml instead of
    restating them, and refuses to report success under a different Node —
    control: it declines on Node 24 with CI pinned at 22. Second control:
    dropping `--platform neutral` makes it report the declared entry points
    missing.

<!--
G9 exists because two local runs reported success for builds CI rejected. The
clippy gate already refuses to certify below CI's version; this is the same
discipline for the JS toolchain.

G8 is manual: the outcome is a judgement about which bumps to take, and the
evidence is the experiments recorded above rather than one command.

G7 shares gate-ci-green's oracle on purpose: the packaging job is part of the
CI workflow, and that checker already requires every job to have concluded
success. It is a separate gate because it is a separate outcome — a green CI
without a packaging job proved nothing about publishability.

G6 is manual because it is blocked on a repository setting (GitHub Pages is not
enabled on the new repo), which is an outward-facing change and the owner's
call, not something a check should make true on its own.

G4's checker must fail honestly. Before trusting a byte-equality pass, it
perturbs one input (--created-at) and asserts the digests then differ, so a
checker that compared nothing cannot report success.
-->
