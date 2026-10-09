# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- An explicit `app-store` build profile that excludes DLC and mod pack kinds,
  with separate CI compilation, lint and test coverage.
- Apple backup exclusion for the persistent layer pool through `tpk-backup`.
- Startup rollback reporting through `status().rolled_back` in Rust and the
  TypeScript guest API.

### Fixed

- Embed the Common Controls activation manifest so Windows plugin tests start
  instead of exiting with `STATUS_ENTRYPOINT_NOT_FOUND` before running.
- Validate patch ancestry, shell compatibility and persisted revision state;
  rebuild missing or corrupt delta caches with bounded reads and atomic writes.
- Keep launch checks and automatic downloads out of degraded or blacklisted
  update paths, and preserve complete parent chains when planning updates.
- Enforce container and signature bounds, validate local executable sources in
  packed HTML and JavaScript, and distinguish CLI usage errors from verification
  failures.
- Preserve published channel chains and refuse immutable-object overwrites or
  unverifiable S3 existence checks before uploading content releases.
- Update vulnerable dependencies in both workspace and example lockfiles, and
  audit the example's independent lockfile in CI.

### Changed

- Update compatible Rust dependencies while retaining `tauri-utils 2.9.3` and
  `tauri-plugin 2.6.3`: newer helpers require Rust 1.90 and fail to compile with
  Tauri 2.11's test feature; the project keeps its declared Rust 1.88 minimum.
- Align the TPK/1 specification, JSON Schemas, API documentation and website with
  the implemented runtime and App Store behavior; remove the obsolete hotswap
  migration guide.

## [0.1.0] — unreleased

### Added

- **TPK/1 pack format** (`tpk-format`) — ZIP container plus a signed
  `tpk-manifest.json`. Per-entry `full` / `delta` / `delete` operations, so a
  release can be a full base, a file-level patch, or a set of tombstones.
- **Signature handling** — minisign (Ed25519) verification and production, with a
  `key_epoch` on every trusted key. Clients keep a monotonic floor, which is what
  lets a leaked key be retired without waiting for every device to drop it from
  its list.
- **`tpk` CLI** — `keygen`, `pack`, `sign`, `channel`, `inspect` and `verify`,
  with the specification's exit codes (0 / 2 verification failure / 3 usage).
- **JSON Schemas** under `spec/json-schema/`, checked against the Rust parsers by
  a test so the two cannot drift apart.
- **Workspace layout** — eight crates with a strictly one-way dependency graph;
  `tpk-client` deliberately cannot reach `tpk-store`.
- **MSRV 1.88**, enforced by a CI job rather than left to drift.
- Runtime zstd decoding is pure-Rust `ruzstd`; `zip` is pinned to
  `default-features = false` so its default feature set cannot drag `zstd-sys`,
  `bzip2`, `lzma-rust2` and `ppmd-rust` into the runtime. `deny.toml` pins that
  boundary with a `wrappers` allowlist.
