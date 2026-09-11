# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — unreleased

### This is a different package

`tauri-plugin-hotswap` / `tauri-plugin-hotswap-api` are no longer maintained.
This project continues as `tauri-plugin-tpk` / `tauri-plugin-tpk-api`, implementing
the TPK/1 specification. **There is no upgrade path**: the on-disk layout, the
manifest format, the command names and the JS API are all incompatible.

### Added

- **TPK/1 pack format** (`tpk-format`) — ZIP container plus a signed
  `tpk-manifest.json`. Per-entry `full` / `delta` / `delete` operations, so a
  release can be a full base, a file-level patch, or a set of tombstones.
- **Signature handling** — minisign (Ed25519) verification and production, with a
  `key_epoch` on every trusted key. Clients keep a monotonic floor, which is what
  lets a leaked key be retired without waiting for every device to drop it from
  its list.
- **`tpk` CLI** — `keygen`, `inspect` and `verify`, with the specification's exit
  codes (0 / 2 verification failure / 3 usage).
- **JSON Schemas** under `spec/json-schema/`, checked against the Rust parsers by
  a test so the two cannot drift apart.
- **Workspace layout** — seven crates with a strictly one-way dependency graph;
  `tpk-client` deliberately cannot reach `tpk-store`.
- **MSRV 1.88**, enforced by a CI job rather than left to drift.

### Changed

- Runtime zstd decoding uses pure-Rust `ruzstd`; `zip` is pinned to
  `default-features = false` so its default feature set cannot drag `zstd-sys`,
  `bzip2`, `lzma-rust2` and `ppmd-rust` into the runtime. `deny.toml` pins that
  boundary with a `wrappers` allowlist.
- The README no longer describes the plugin as a way to skip app store review.
  Content updates are governed by Apple DPLA §3.3.1(B) and Google Play's Device
  and Network Abuse policy; see `docs/security.md`.

### Removed

- Everything from the hotswap implementation: the `seq-N` directory layout, the
  `current` pointer file, tar.gz bundles, the `apply` / `activate` split, and the
  runtime `configure` command.

## [0.0.4] — 2026-04-09

### Added

- **Cross-platform CI** — `cargo check` and `cargo test` now run on macOS and Windows in addition to Linux
- **Rustdoc CI step** — docs are built with `-D warnings` to catch broken links and missing docs
- **`documentation` field in `Cargo.toml`** — links to docs.rs from the crates.io page
- **`#[non_exhaustive]` on public types** — `Error`, `HotswapConfig`, `HotswapManifest`, `HotswapMeta`, `HotswapCheckResult`, `HotswapVersionInfo`, `DownloadProgress`, `LifecycleEvent`, `ConfirmationDecision` are now non-exhaustive, preventing new fields/variants from being semver-breaking

### Fixed

- **Yanked dependency** — bumped `fastrand` 2.4.0 → 2.4.1
- **README version drift** — quickstart now shows `0.0.4` instead of `0.0.1`
- **Split doc comment on `DiscardOnUpgrade`** — doc block was interrupted by `#[default]` attribute

### Changed

- **Crate tarball trimmed** — added `exclude` to `Cargo.toml`, reducing package from 81 files to ~32
- **CI consolidated** — merged separate Linux and cross-platform Rust jobs into a single matrix job

## [0.0.3] — 2026-04-06

### Fixed

- **Startup crash when `plugins.hotswap` is absent from `tauri.conf.json`**: Apps using `init_with_config()` or `HotswapBuilder` (without a `plugins.hotswap` JSON section) crashed on startup. Switched plugin builder config type from `HotswapConfig` to `serde_json::Value` so Tauri accepts both `null` and JSON objects during `Builder::run()`.

### Changed

- `init()`, `init_with_config()`, and `HotswapBuilder::build()` now return `HotswapPlugin<R>` (alias for `TauriPlugin<R, serde_json::Value>`)

## [0.0.2] — 2026-04-06

### Added

- **iOS platform support** — full OTA flow tested on simulator
- **Configurable OTA policy traits** — four traits replace hardcoded behavior:
  - `BinaryCachePolicy` — controls cache retention on binary upgrades (`keep_compatible`, `discard_on_upgrade`, `never_discard`)
  - `ConfirmationPolicy` — configurable grace period for `notifyReady()` (`single_launch`, `grace_period { max_unconfirmed_launches }`)
  - `RollbackPolicy` — configurable rollback target (`latest_confirmed`, `immediate_previous_confirmed`, `embedded_only`)
  - `RetentionPolicy` — configurable version retention count (`max_retained_versions`, default 2)
- **Custom policy injection** — `HotswapBuilder` setters accept `impl Policy` for all four traits, enabling custom implementations beyond the built-in enums
- New config knobs: `binary_cache_policy`, `confirmation_policy`, `rollback_policy`, `max_retained_versions`
- `HotswapMeta` gains `unconfirmed_launch_count` field (backward compatible via serde default)
- Debug logging in `HotswapAssets::get()` for diagnosing asset resolution issues
- Local testing guide (`docs/local-testing.md`) with example test server
- App Store / Google Play compliance disclaimer in README
- Mobile-compatible example app (`lib.rs` + `main.rs` split for iOS/Android)
- README included in npm package (`tauri-plugin-hotswap-api`)
- 66 new unit tests (105 total, up from 39)

### Fixed

- **Mobile crash on startup**: Plugin builder now declares `HotswapConfig` as its config type (`Builder::<R, HotswapConfig>::new("hotswap")`). Without this, Tauri's plugin system failed to deserialize `plugins.hotswap` from the config on iOS and Android, causing a crash during app initialization.

### Changed

- Return types of `init()`, `init_with_config()`, and `HotswapBuilder::build()` changed from `TauriPlugin<R>` to `TauriPlugin<R, HotswapConfig>` (required for the mobile fix; transparent to most users since the type is passed directly to `.plugin()`)
- `check_compatibility()`, `rollback()`, `cleanup_old_versions()` now accept policy trait references instead of booleans/hardcoded logic
- `HotswapBuilder` gains `binary_cache_policy()`, `confirmation_policy()`, `rollback_policy()`, `retention_policy()`, `max_retained_versions()` setters — all accept custom `impl Policy` types

### Breaking

- **Removed `discard_on_binary_upgrade`** — the config field, builder method, and legacy mapping logic are removed entirely. Migrate as follows:
  - `discard_on_binary_upgrade: true` → `binary_cache_policy: "discard_on_upgrade"` (or omit — this is the default)
  - `discard_on_binary_upgrade: false` → `binary_cache_policy: "keep_compatible"`
  - `HotswapBuilder::discard_on_binary_upgrade(true)` → `.binary_cache_policy(BinaryCachePolicyKind::DiscardOnUpgrade)`
  - If you never set `discard_on_binary_upgrade`, no action needed — default behavior is unchanged

## [0.0.1] — 2026-04-05

Initial release. Open-source OTA frontend updates for Tauri v2.

### Added

#### Core

- Hot-swap frontend assets at runtime — no binary rebuild, no app store review
- Minisign signature verification on every downloaded bundle
- Automatic rollback if `notifyReady()` is not called after update
- Binary compatibility gating via `min_binary_version`
- Sequence-based update ordering (monotonic integers, not semver)

#### Update Flow

- `checkUpdate()` → `applyUpdate()` one-liner for simple integrations
- Split `downloadUpdate()` + `activateUpdate()` for download-now-apply-later workflows
- Download progress events (`hotswap://download-progress`)
- Lifecycle events (`hotswap://lifecycle`) for telemetry (Sentry, PostHog, etc.)
- Download retry with exponential backoff (1s → 2s → 4s → 8s, configurable)
- `mandatory` and `bundle_size` fields in manifest for UI decisions

#### Configuration

- Configure via `tauri.conf.json`, programmatic `HotswapConfig`, or `HotswapBuilder`
- Runtime configuration via `configure()` / `getConfig()` — change channel, endpoint, and headers without restart
- Update channels (`production`, `staging`, `beta`, etc.) switchable at runtime
- Custom HTTP headers on check and download requests (auth tokens, API keys)
- Platform and architecture sent automatically on every check request

#### Extensibility

- `HotswapResolver` trait — bring your own update source
- Built-in `HttpResolver` for dynamic API endpoints
- Built-in `StaticFileResolver` for static manifest files
- Zip bundle support via `features = ["zip"]`

#### Security

- HTTPS enforced by default (configurable)
- Configurable maximum bundle size (default 512 MB)
- Path traversal protection in archive extraction (`..` and absolute paths rejected)
- Atomic extraction via temp directory + rename
- Atomic pointer updates via temp file + rename
- Restrictive file permissions on metadata (`0o600` on Unix)
- Pointer file validation (`seq-N` format enforced)
- Stale cache discarded on binary upgrade (`discard_on_binary_upgrade`)

#### Platforms

- macOS, Windows, Linux, Android

#### Guest JS (`tauri-plugin-hotswap-api`)

- `checkUpdate()`, `applyUpdate()`, `downloadUpdate()`, `activateUpdate()`
- `rollback()`, `getVersionInfo()`, `notifyReady()`
- `configure()`, `getConfig()`
- `onDownloadProgress()`, `onLifecycle()`

[Unreleased]: https://github.com/denniskribl/tauri-plugin-hotswap/compare/v0.0.4...HEAD
[0.0.4]: https://github.com/denniskribl/tauri-plugin-hotswap/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/denniskribl/tauri-plugin-hotswap/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/denniskribl/tauri-plugin-hotswap/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/denniskribl/tauri-plugin-hotswap/releases/tag/v0.0.1
