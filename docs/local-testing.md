---
title: Local testing
description: Serving a channel from your laptop and exercising the paths that matter.
---

# Local testing

## Prepared desktop smoke session

The standalone example includes `examples/shell-app/smoke.mjs`. It prepares a
fresh app identifier, an embedded baseline, signed base/patch/broken packs, and
localhost TLS certificates. It does not install certificate trust, launch apps,
or modify existing app data. Signing and TLS private keys stay in a private
OS temporary directory outside Git; only public fixtures/configuration live in
the ignored `test-server/` directory.

From the repository root:

```sh
CARGO_TARGET_DIR="$PWD/target" cargo build -p tpk-cli --locked
pnpm -C examples/shell-app install --frozen-lockfile
node examples/shell-app/smoke.mjs prepare \
  --tpk ./target/debug/tpk \
  --dir examples/shell-app/test-server/local-smoke
```

The destination must not already exist. Preparation verifies every pack and
channel signature and requires the patch to contain both a delta and a deletion
tombstone. If a runner removes its `TMPDIR` between commands, add
`--private-root /private/tmp` on macOS (or another persistent OS temporary root).
The helper refuses a private-key root inside a Git worktree. Certificates expire
after two days; prepare a new session if they expire or its private directory is
removed.

Preparation prints the app build command, bundle identifier, CA fingerprint,
server command, and macOS trust/removal commands. Review the certificate before
adding trust: the printed macOS command restricts trust to the SSL policy for
`localhost` in the user keychain. This changes OS state and needs explicit
approval. Do not disable TLS verification. iOS simulators/devices need their own
certificate trust setup; desktop keychain trust does not transfer to them.

In one terminal, keep the printed `serve` command running. In another, build the
app with the printed `--config` override. Stop only this session's server with
`node examples/shell-app/smoke.mjs stop --dir SESSION_DIRECTORY`; the control
token stays in its private temporary directory and shutdown requires it. Use
these scenario switches:

```sh
node examples/shell-app/smoke.mjs publish \
  --dir examples/shell-app/test-server/local-smoke --scenario patch
node examples/shell-app/smoke.mjs publish \
  --dir examples/shell-app/test-server/local-smoke --scenario broken
```

Use a CLI binary in your own stable target directory when preparing the session.
If its original location is removed, `publish --tpk /new/path/to/tpk` overrides the
saved executable path without changing the session's signing key or certificate.

Every publish gets a new watermark and keeps numbered manifests and packs
immutable. An exclusive `.publish.lock` prevents concurrent publishers. If a
process dies while publishing, inspect that lock before removing it; partial
numbered manifests are skipped on the next publish. Publishing during a
check can briefly mismatch the fetched manifest/signature pair; retry Check
once publishing has finished. A verification failure never condemns a pack.

The actual GUI sequence is:

1. Launch the built app. Its heading says `TPK Smoke embedded 0`. Press Check and
   Download, then cold-start the app: the heading becomes `TPK Smoke base 1` and
   the log records `notifyReady` committing the revision.
2. Publish `patch`, press Check/Download, and cold-start again. Confirm the patch
   heading, `Read smoke resource` returns `PATCH DATA`, and `Check tombstone`
   does not return the embedded or base `removed.txt` content.
3. Press `Reset for next launch`, then read the smoke resource again before
   closing the window. It must still return patch data. The smoke configuration
   disables the resolver LRU to make this an uncached disk read. After cold
   start the heading returns to embedded; publish `patch` to reinstall the chain
   at the retained version floor.
4. Publish `broken`, check/download, and cold-start three times. This fixture
   deliberately skips ready acknowledgement. The third start must restore the
   previous committed heading, report `rolled_back`, and refuse the broken pack
   on the next Check.

For cache-purge recovery, locate the unique identifier printed by preparation
in the cache root, stop only that smoke app, remove its `materialized` results,
and relaunch. Read the smoke resource until the background rebuild returns patch
data. Never remove another application's data. Revoke the temporary certificate
trust after finishing. These steps are real device acceptance; building the
bundle and the following tests do not establish that acceptance:

```sh
node --test examples/shell-app/smoke.test.mjs
TPK_SMOKE_SESSION="$PWD/examples/shell-app/test-server/local-smoke" \
  node --test examples/shell-app/smoke.test.mjs
```

The first command tests readiness ordering and error/retry handling. The second
also starts/stops a local HTTPS server and verifies signed scenario switching,
Range responses and private-file isolation. Its test client trusts the CA only
within that process, without changing system trust.

## A channel on localhost

`manifest_url` must be https, so a plain `python3 -m http.server` will not do.
Use a local TLS proxy, or point at a tunnel:

```bash
# a throwaway cert with mkcert
mkcert -install && mkcert localhost
npx http-server ./cdn -S -C localhost.pem -K localhost-key.pem -p 8443
```

```json
"manifest_url": "https://localhost:8443/{{channel}}/latest.json"
```

Directory layout:

```
cdn/stable/
├── latest.json
├── latest.json.minisig
└── core-2.1.0.tpk
```

## Build a pack and a channel

```bash
tpk keygen --out /tmp/signing.key      # prints the public key
export TPK_SIGNING_KEY=$(cat /tmp/signing.key)

pnpm build

tpk pack --kind base --id core \
  --version 2.1.0 --version-code "$(date -u +%Y%m%d%H%M%S)" \
  --created-at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --dist dist/ --out cdn/stable/core-2.1.0.tpk

tpk channel --channel stable --pack cdn/stable/core-2.1.0.tpk \
  --url-base https://localhost:8443/stable/ \
  --watermark auto --out cdn/stable/latest.json

tpk verify --pubkey "$TPK_PUBKEY" \
  --file cdn/stable/latest.json --file cdn/stable/core-2.1.0.tpk
```

Put the printed public key into `plugins.tpk.pubkeys`.

## Exercise the flow

```ts
const c = await check();
console.log(c);                    // { status: "available", ... }
console.log(await download());     // { status: "staged", rev: "..." }
console.log(await status());       // pending: true
// quit and relaunch — updates apply on cold start
console.log(await status());       // pointer: "booting"
await notifyReady();
console.log(await status());       // pointer: "committed"
```

## Test the rollback

This is the path worth testing, and almost nobody does.

```bash
# a pack whose frontend never calls notifyReady()
tpk pack --kind base --id core \
  --version 9.9.9 --version-code "$(date -u +%Y%m%d%H%M%S)" \
  --created-at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --dist broken-dist/ --out cdn/stable/core-broken.tpk
```

Stage it, then cold start three times. On the third the revision is rolled back
and blacklisted, `status().rolled_back` names it (no `onState` event fires — the
rollback happens before any window exists), and the app is back on the previous
content.

Then confirm the blacklist holds: republish the same `version_code` and watch
`check()` decline it. Bumping `version_code` is the only way past — deliberately,
so a CI rerun cannot push a condemned release back onto devices.

## Test a patch chain

```bash
tpk pack --kind patch --id core \
  --version 2.1.1 --version-code "$(date -u +%Y%m%d%H%M%S)" \
  --created-at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --parent cdn/stable/core-2.1.0.tpk \
  --dist dist/ --out cdn/stable/core-2.1.1.tpk

tpk inspect cdn/stable/core-2.1.1.tpk --json \
  | jq '[.entries[] | .op] | group_by(.) | map({op: .[0], n: length})'
```

You should see a mix of `full`, `delta` and `delete`. If everything is `full`,
your build is not reproducible — a changed bundle hash on every file usually
means a timestamp or a content hash in the output names.

## Test the tombstone

Delete a file from `dist/`, build a patch, install it, and request the deleted
path. With `TpkHandle::filter_web_resource_response` wired into the webview's
response hook, it must return an empty 404. The example wires this automatically
when explicitly creating its configured windows. Without that hook, Tauri can
return a SPA homepage with 200 after `Assets::get()` correctly hides the deleted
payload. Never accept the old embedded file's content as a passing result. See
[the response guard](./api-reference.md#tombstone-http-response-guard) for host
integration and its core-resolution limitation.

## Inspecting state

```bash
# macOS
python3 -m json.tool ~/Library/Application\ Support/com.example.app/tpk/state.json
```

Paths for every platform are in [Disk layout](./disk-layout.md).

Start over:

```ts
await reset({ clearBlacklist: true });
```

or delete the `tpk` directory in both roots.

## Mobile

**`cargo tauri ios dev` and `android dev` prove nothing.** They proxy every asset
request to the dev server, so `Assets::get()` is never called and no part of the
overlay runs.

```bash
cargo tauri ios build --debug
cargo tauri android build --debug
```

```bash
# Android logs
adb logcat | grep -i tpk
# Android state
adb shell run-as com.example.app ls -la files/tpk/

# iOS simulator container
xcrun simctl get_app_container booted com.example.app data
```

Remember that `auto_check_on_launch` and `auto_download` default to `false` on
mobile — call `check()` and `download()` explicitly, or you will conclude the
plugin is broken.

On a physical device, `localhost` refers to the device, not your laptop. Use a
reachable HTTPS endpoint. Android's native updater uses rustls' bundled roots;
installing a desktop or Android user CA does not make the prepared localhost
certificate trusted by that updater. Use a publicly trusted HTTPS certificate
for Android device acceptance. A simulator run is separate evidence from a
physical-device run.

### Isolated Apple Silicon simulator build

From the repository root, use the example's entry point to keep Rust 1.88 and
its simulator artifacts under `target/`, without changing the global toolchain:

```sh
# One-time toolchain preparation; use the actual rustup executable.
RUSTUP_HOME="$PWD/target/ios-rustup" "$HOME/.cargo/bin/rustup" \
  toolchain install 1.88.0 --profile minimal --no-self-update
RUSTUP_HOME="$PWD/target/ios-rustup" "$HOME/.cargo/bin/rustup" \
  target add --toolchain 1.88.0 aarch64-apple-ios-sim

sh examples/shell-app/ios-simulator.sh check
sh examples/shell-app/ios-simulator.sh init
sh examples/shell-app/ios-simulator.sh build
# A prepared smoke session may be selected with build --config /absolute/path/to/config.json.
```

Set `TPK_RUSTUP_BIN` if rustup is installed elsewhere. Install the example's
locked JavaScript dependencies with `pnpm -C examples/shell-app install --frozen-lockfile`
before initialization or bundling. Xcode and its iOS simulator SDK must already
be available.

`check` runs Cargo with `--target aarch64-apple-ios-sim --locked`; it checks Rust
compilation without generating or linking a simulator `.app`. The script adds a
command-local Swift wrapper selecting `--build-system native` for `swift build`
and disabling keychain/netrc lookup. This avoids the macOS SDK flags observed
with Xcode 27's default SwiftPM backend. The wrapper and build outputs are ignored
under `target/`; system Swift and global Cargo settings are unchanged. The script
also places `TMPDIR` under `target/ios-smoke/tmp`, so a detached build can continue
after its launching terminal's temporary directory is removed.

`init` checks for `xcodegen`, `idevicesyslog` (libimobiledevice), and `pod`
(CocoaPods) before invoking Tauri. Tauri's `--skip-targets-install` only skips Rust
target installation; missing native dependencies still trigger Homebrew/gem
installation. The script stops if one is missing so that installing those tools
can be an explicit machine setup decision. Generated files live in the ignored
`src-tauri/gen/apple/` directory and must be regenerated in each checkout.

`build` requires that generated project and runs `tauri ios build --debug
--target aarch64-sim --no-sign --ci`. It does not launch Xcode or a simulator.
A successful `.app` build is separate from simulator OTA acceptance, which still
requires installing the app, configuring HTTPS trust, and exercising cold starts.
Physical-device acceptance remains a separate check.

If Tauri reports `Xcode Simulator SDK <version> is not installed` even though
`xcodebuild -showsdks` lists that SDK, compare it with
`xcrun simctl list runtimes`. The mobile build dependency also requires an
available simulator runtime at least as new as the default simulator SDK.
For example, SDK 27.1 with only iOS 26.5 and 27.0 runtimes fails this preflight.
Installing the matching runtime is a separate Xcode machine setup step:

```sh
# Installs a system simulator runtime; run after choosing to change Xcode's setup.
xcodebuild -downloadPlatform iOS -buildVersion 27.1 -architectureVariant arm64
```

Use the version matching your installed SDK; downloading a runtime is not needed
for the Rust-only `check` command.

Runtime availability also does not prove application compatibility. In a local
run with Tauri 2.10.3 / tao 0.34.8, the SDK 27.1 simulator build installed and
started on iOS 27.1 (24A94425), then UIKit raised `SIGTRAP` at
`UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption` before OTA actions
could be exercised. Treat this as a host lifecycle compatibility blocker; do not
count a successful `simctl launch` as proof that the WebView stayed alive. Check
the crash report and verify the actual page and durable state before accepting a
mobile run. This observation does not establish the behavior on a physical device
or a different runtime/framework version.

The upstream [scene-adoption issue](https://github.com/tauri-apps/tauri/issues/15719)
requires a nonempty static `UIApplicationSceneManifest` naming
`TaoSceneDelegate`, together with a runtime that implements that delegate. Adding
that plist to tao 0.34.8 alone does not supply the missing scene implementation.
The [tao release notes](https://v2.tauri.app/release/tao/#0360) also place the
scene-configuration use-after-free fix in 0.36.0; upgrading only to the 0.35
series does not include it.

A separate diagnostic host built with Rust 1.90.0, Tauri 2.12.1,
tauri-runtime-wry 2.12.3 and tao 0.37.2, plus the static scene manifest, remained
alive on the same iOS 27.1 runtime. Its initial `Loading…` screen was traced to
the diagnostic configuration dropping `app.withGlobalTauri: true` when replacing
the example's `app` object. Restoring that setting enabled the smoke page's
`window.__TAURI__` calls and IPC readiness.

A subsequent dedicated simulator run verified the actual WebView and durable
state through embedded → signed base 1 → patch 2, with cold starts committing
each revision. The delta resource returned `200 PATCH DATA`; the deleted resource
returned an empty `404`. A deliberately unacknowledged version 3 remained
`booting` for two launches, then the third launch restored committed patch 2.
The blacklist recorded `not_acknowledged`, the version floor remained 2, and a
new check of the same channel returned `up_to_date`. The dedicated simulator and
its test CA were deleted, and the local HTTPS server was stopped afterward.

A follow-up dedicated simulator run also verified cache purge recovery and
reset. With the app stopped, both materialized delta files were removed from its
own cache; the next cold start rebuilt both with identical SHA-256 hashes, kept
the committed patch, and did not roll back. The WebView again returned
`200 PATCH DATA`. Reset cleared persisted revisions while keeping version floor 2
and the running window's resources: a fresh fetch in that same window still
returned patch data. The next cold start displayed embedded assets. Checking and
downloading again selected the complete base 1 + patch 2 chain despite the retained
floor, and another cold start committed patch 2. That simulator, its test CA, and
the local services were also cleaned up.

These results accept the described OTA scenarios on the newer diagnostic host.
Physical iOS devices and Android remain untested. They do not resolve the official
Rust 1.88 example's iOS 27 lifecycle blocker.

Tauri 2.12.1 requires Rust 1.90. Keep that diagnostic upgrade separate from this
project's Rust 1.88 baseline until the host/toolchain policy is explicitly
changed. The diagnostic run did not update the example or workspace lockfiles.

## In CI

```bash
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo deny check licenses bans
cargo check --manifest-path examples/shell-app/src-tauri/Cargo.toml
```

`examples/shell-app/src-tauri` is excluded from the workspace and carries its own
lockfile, so `--workspace` never covers it. Check it separately.

`cargo deny`'s `bans.wrappers` pins the zstd boundary: `ruzstd` (pure Rust)
decodes at runtime, the C `zstd` encoder is CLI-only. If a change makes the
runtime pull `zstd-sys`, that check fails — and it is meant to.
