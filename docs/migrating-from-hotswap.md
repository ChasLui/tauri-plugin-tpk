---
title: Migrating from tauri-plugin-hotswap
description: What changed, why, and how to move an existing app across.
---

# Migrating from `tauri-plugin-hotswap`

`tauri-plugin-tpk` is a full rewrite around the frozen TPK/1 format. It is not
source-compatible and the bundle format is different — old bundles cannot be
loaded. Plan a shell release, not a drop-in swap.

## Package names

| Old | New |
|---|---|
| `tauri-plugin-hotswap` (crates.io) | `tauri-plugin-tpk` |
| `tauri-plugin-hotswap-api` (npm) | `tauri-plugin-tpk-api` |
| `plugins.hotswap` in `tauri.conf.json` | `plugins.tpk` |
| `hotswap:` ACL namespace | `tpk:` |
| `hotswap://` events | `tpk://` |

MSRV moved from 1.77.2 to 1.88.

## Rust

```rust
// before
tauri::Builder::default()
    .plugin(tauri_plugin_hotswap::init())
    .run(tauri::generate_context!())

// after
let mut context = tauri::generate_context!();
let tpk = tauri_plugin_tpk::attach(&mut context);

tauri::Builder::default()
    .plugin(tauri_plugin_tpk::init(tpk))
    .run(context)
```

The two-phase `attach` / `init` split is load-bearing: `attach` swaps the asset
provider before the app is built without resolving any path, because on Android
the data directory is not reachable that early. See
[Architecture](./architecture.md#startup).

`HotswapAssets` and `HotswapResolver` are gone. Asset serving is
`PackAssets` plus a resolver built from the layer stack, and there is no
pluggable-source trait — the update source is native configuration with no
runtime setter, on purpose.

## TypeScript

| Old | New |
|---|---|
| `checkUpdate()` | `check()` |
| `downloadUpdate()` | `download()` |
| `applyUpdate()` / `activateUpdate()` | — gone, updates apply on cold start |
| `rollback()` | — gone, rollback is automatic |
| `getVersionInfo()` | `status()` |
| `configure()` / `getConfig()` | — gone, configuration is native only |
| `notifyReady()` | `notifyReady()` |
| `onLifecycle()` | `onState()` |
| `onDownloadProgress()` | `onDownloadProgress()` |
| `HotswapCheckResult` | `CheckOutcome` |
| `HotswapVersionInfo` | `Status` |

### Things that were removed and why

**`configure()` / `getConfig()`** — a runtime setter for the update URL means a
scripting bug in the frontend can repoint the updater. `manifest_url` and
`pubkeys` are now build-time configuration with no command that changes them.

**`applyUpdate()` / `activateUpdate()`** — there is no supported way to apply
content to a running process. A WebView that has already imported half a bundle
would end up mixing modules from two revisions. `download()` returns `staged`
and the revision applies on the next cold start.

**`rollback()`** — a manual rollback is only reachable from a frontend that
still works, which is the case where you do not need one. Rollback is now
automatic after three unacknowledged launches.

### Outcomes instead of exceptions

The biggest behavioural change. `check()` and `download()` no longer throw for
ordinary answers:

```ts
// before
try {
  const r = await checkUpdate();
  if (r.available) await downloadUpdate();
} catch (e) {
  // "up to date" arrived here, alongside real failures
}

// after
const r = await check();
switch (r.status) {
  case "available":       await download(); break;
  case "up_to_date":      break;
  case "shell_required":  promptStoreUpdate(r.min_shell); break;
  case "degraded":        telemetry.warn(r.consecutive_rollbacks); break;
  case "disabled":        break;
}
```

A rejected promise now means something you cannot act on, and carries one of
fourteen frozen [error codes](./error-codes.md).

## Configuration

```json
// before
{ "plugins": { "hotswap": {
    "endpoint": "https://cdn.example.com/hotswap.json",
    "publicKey": "..."
} } }

// after
{ "plugins": { "tpk": {
    "manifest_url": "https://cdn.example.com/tpk/{{channel}}/latest.json",
    "pubkeys": [{ "key": "RWT...", "epoch": 1 }]
} } }
```

`pubkeys` is a list with epochs so keys can be rotated; the URL template accepts
`{{channel}}`, `{{arch}}`, `{{target}}` and `{{shell}}` and nothing else. Unknown
keys in the section are now a hard error rather than a silent default — a
misspelling that used to leave you convinced you had configured something now
fails at startup. Full list in [Configuration](./configuration.md).

## Capabilities

```json
// before
"permissions": ["hotswap:default"]
// after
"permissions": ["tpk:default"]
```

`tpk:default` covers `check`, `download`, `notify_ready` and `status`. `reset`
needs `tpk:allow-reset` and is not in the default set.

## Bundles

Old bundles do not load. Repack from source:

```bash
tpk keygen --out secret.key
tpk pack --kind base --id core \
  --version 2.0.0 --version-code "$(date -u +%Y%m%d%H%M%S)" \
  --created-at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --dist dist/ --out core-2.0.0.tpk
tpk channel --channel stable --pack core-2.0.0.tpk \
  --url-base https://cdn.example.com/tpk/core/ --out latest.json
```

Generate a new key pair. The old format's signatures are not TPK signatures, and
reusing a key across two formats is a bad habit with no upside.

The server contract changed too — a channel manifest, a detached `.minisig`, and
a monotonic per-channel watermark. See [Server contract](./server-contract.md).

New in the format, and worth knowing about before you design your publishing
pipeline: layered packs with `base` / `patch` / `dlc` / `mod`, bsdiff deltas and
deletion tombstones, staged rollout by percentage, and key epochs. See
[Overlay resolution](./overlay.md).

## Rollout plan

1. Ship a shell release carrying `tauri-plugin-tpk` and the new public key,
   with no channel published yet. Every device serves its embedded assets.
2. Wait for adoption. Devices still on the old shell keep using the old
   mechanism until they update — the two do not interfere, they read different
   config sections and different directories.
3. Publish a `base` pack at `--rollout 10`, watch telemetry, widen.
4. Once adoption is high enough, remove the old plugin in a later shell release.

Do not try to serve both formats from one endpoint. Publish the TPK channel at a
new path.

## Cleanup

The old plugin's data directory is not read or removed by this one. If you want
it gone, delete it from your own `setup` — it is your app's data, and silently
deleting a user's files is not a plugin's call.
