---
title: Architecture
description: The crates, the startup sequence, and the state machine.
---

# Architecture

## Crates

```
tpk-format   TPK/1 container, manifest, paths, signatures — the single source of truth
tpk-delta    bsdiff apply (runtime) / diff (CLI)
tpk-resolve  layered overlay, tombstones, index, LRU, CSP hashes
tpk-store    layer pool, three-state machine, blacklist, delta materialization
tpk-client   channel manifest fetch, update planning, resumable download
tpk-cli      the `tpk` binary
tauri-plugin-tpk  attach/init, PackAssets, commands, events, permissions
```

Dependencies run one way and must stay that way:

```
tpk-format ◄── tpk-resolve ◄── tpk-store ──► tpk-delta
tpk-format ◄── tpk-client              # client must NOT depend on store
all of the above ◄── tauri-plugin-tpk
```

`tpk-client` deliberately does not depend on `tpk-store`. That keeps
`Client::plan` a pure function — given a channel manifest, the installed layers
and the shell version, it returns what to do — so the whole planning policy
(monotonicity, patch parent exactness, rollout bucketing, shell gating) is
testable without touching disk.

## Startup

Two entry points, and the split between them is the most important thing on this
page.

```rust
fn main() {
    let mut context = tauri::generate_context!();
    let tpk = tauri_plugin_tpk::attach(&mut context);   // 1

    tauri::Builder::default()
        .plugin(tauri_plugin_tpk::init(tpk))            // 2
        .run(context)
        .expect("error running app");
}
```

**1. `attach(&mut context)`** swaps `context.assets` for a `PackAssets` that
keeps the embedded assets as its fallback, and returns a handle. It resolves no
path, opens no file and reads no configuration. It cannot: on Android
`app_local_data_dir()` goes through JNI and is not reachable this early.

**2. `init(handle)`** registers the plugin. Everything stateful happens in its
`setup` hook, which Tauri runs at the end of `Builder::build()` —
`initialize_plugins` is called before the first window is created, so the
resolver is in place before anything can request an asset.

Inside `setup`, in order:

1. Read `plugins.tpk`. Absent or unusable → log and stay inert.
2. Build the `TrustStore` from `pubkeys`.
3. Resolve the two disk roots and ask the platform to exclude the layer pool
   from backup.
4. `Store::open` — read `state.json` and the blacklist.
5. Seed from the bundled pack if configured and nothing is installed yet.
6. `Store::boot()` — run the state machine (below).
7. Build the resolver over the resulting layer set and install it into
   `PackAssets`.
8. Record any layer that failed to load, and `manage` the plugin state.

`setup` **never returns `Err`**. Returning `Err` there aborts `Builder::build`
and the app does not start — so a corrupt `state.json` or a full disk logs and
degrades to the embedded assets instead of turning a content problem into a
launch failure.

## The three-state machine

`state.json` holds three optional revision slots and a pointer:

```
       download()                cold start              notifyReady()
  ─────────────────► staged ──────────────────► booting ──────────────► committed
                                                   │
                                        3 unacknowledged launches
                                                   ▼
                                        rolled back + blacklisted
```

- **`staged`** — downloaded, verified, not yet tried.
- **`booting`** — on trial this launch. `boot_attempts` counts launches without
  an acknowledgement.
- **`committed`** — acknowledged. What a rollback falls back to.

`MAX_BOOT_ATTEMPTS` is 3. Below the threshold the revision is retried, because
an OS kill or the user quitting are ordinary events. At the threshold the
revision is rolled back, the release is blacklisted by both hash and
`(id, version_code)`, and `consecutive_rollbacks` increments.

`MAX_CONSECUTIVE_ROLLBACKS` is also 3. At that point the install is **degraded**:
automatic checking and downloading stop, `check()` returns
`{ status: "degraded" }`, and the app keeps serving whatever last worked. Three
different releases failing in a row is more likely a problem with the device or
the shell than with any one pack, and continuing to download would just burn
bandwidth.

Two independent counters, not one: `boot_attempts` decides whether *this*
revision is bad, `consecutive_rollbacks` decides whether *updating* is working
at all.

## Serving an asset

```
WebView requests tauri://localhost/assets/app.js
  └─ PackAssets::get(AssetKey)
       ├─ normalize the key            (the WebView's, so normalized not rejected)
       ├─ resolver index lookup        (winning layer for this path, precomputed)
       │    ├─ hit  → read blob, verify blob_sha256, decode, verify sha256
       │    ├─ Deleted    → 404, does NOT fall through to embedded
       │    ├─ NotFound   → fall through to embedded
       │    └─ LayerCorrupt → record failure, fall through to embedded
       └─ LRU insert, return bytes
```

The index is built once during `setup` and holds only the *winning* layer per
path, so serving is one map lookup rather than a walk down the stack. A
tombstone that resolved to `Deleted` must not fall through: the whole point of
`op: "delete"` is to hide a file the binary still embeds.

`PackAssets` uses `OnceLock`, not `RwLock`. Layers are frozen for the process
lifetime by design, so a running WebView always sees a consistent index; making
it swappable would be the hot-reload footgun described in
[Philosophy](./philosophy.md).

## Where deltas are applied

In `stage()`, never in `boot()`.

Applying a bsdiff patch at boot means reading the base, allocating the output
and hashing it before the window can be created. On mobile that can trip the iOS
20-second launch watchdog or an Android ANR — which kills the process, which
increments `boot_attempts`, which after three launches blacklists a pack that
was never broken. Materialization at download time costs nothing anyone is
waiting on.

The results land in the cache root, which the OS may purge at any time, so the
lazy re-materialization path in `tpk-store::materialize` must always exist. See
[Disk layout](./disk-layout.md).

## Further reading

- [Overlay resolution](./overlay.md)
- [Disk layout](./disk-layout.md)
- [Server contract](./server-contract.md)
- [API reference](./api-reference.md)
