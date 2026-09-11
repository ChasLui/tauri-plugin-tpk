---
title: Local testing
description: Serving a channel from your laptop and exercising the paths that matter.
---

# Local testing

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
tpk keygen --out /tmp/tpk-secret.key      # prints the public key
export TPK_SECRET_KEY=$(cat /tmp/tpk-secret.key)

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
and blacklisted, `onState` fires with `rolled_back`, and the app is back on the
previous content.

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
path. It must 404 — **not** fall through to the embedded asset. That fall-through
is the classic overlay bug and it is the reason `ResolveMiss` distinguishes
`Deleted` from `NotFound`.

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
