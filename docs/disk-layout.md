---
title: Disk layout
description: What lives where, which root the OS may purge, and what to exclude from backup.
---

# Disk layout

Two roots, and the split matters.

```
$APPLOCALDATA/tpk/            durable — survives, must be backed up selectively
├── state.json                the three-state pointer, watermarks, version and key epoch floors
├── blacklist.json            condemned releases
└── layers/
    └── <file_sha256>.tpk     the content-addressed layer pool

$APPCACHE/tpk/                purgeable — the OS may delete this at any moment
├── materialized/
│   └── <sha256>              delta results, named by the hash of their content
└── tmp/
    ├── *.part                in-flight downloads
    └── <sha256>.tpk          finished downloads, deleted once download() returns
```

## Why a content-addressed pool

Every pack lives once in `layers/`, keyed by the file's own SHA-256; the staged,
booting and committed revisions in `state.json` refer into that pool.

That turns promotion into a single atomic `state.json` write. Moving directories
around is not atomic, and a crash halfway through leaves a pointer to a revision
whose files are partly in two places — which is exactly the situation the state
machine exists to prevent.

It also deduplicates: a `base` shared by the committed and staged revisions is
stored once.

## Atomicity

`state.json` is written through an exclusively created temp file, `fsync`,
`rename`, then a parent-directory `fsync` on Unix. Existing temp paths are never
followed or truncated. Unix directory synchronization errors are returned to the
caller. Other platforms sync the file and atomically rename it; the standard
library does not provide a portable directory flush there, so directory-entry
survival after power loss is not guaranteed.

State transitions publish their candidate in memory after the state write
succeeds. A failed write leaves the trial retryable. A later blacklist write
failure preserves the committed state and is retried by subsequent operations.
Rollback saves new blacklist condemnations before discarding the booting revision,
so a restart between the two writes cannot lose the failed pack's identity.
Staging flushes the layer and materialized directories before publishing state,
including when it reuses resources left by a previous failed write.

Downloads land as `.part` in the cache root and are renamed to `<sha256>.tpk`
there once size and hash match. They are never renamed across roots into
`layers/` — the two roots may be on different filesystems. Staging writes the
pool copy itself with the same temp file, `fsync`, `rename` sequence, verifies
that copy, and the files in `tmp/` are deleted when `download()` returns,
whether it succeeded or not. A crash skips that cleanup, so when a download has
something to fetch it first removes every `.part` and `<sha256>.tpk` in `tmp/`
that is not part of the current plan.

## What the OS may delete

`$APPCACHE` is purgeable on every platform this plugin targets. iOS will clear it
under storage pressure without telling you; Android's cache directory is the
first thing the system reclaims.

So a missing delta result is ordinary. When `setup` finds one, a background
thread rebuilds it from the layer pool, and until then that path falls back to
the embedded assets. Recovery continues after a layer fails so later independent
packs can still recover; staging requires the results it builds on to be
materialized successfully before publishing a revision. Nothing in
the durable root is ever assumed to be reconstructible from the cache root, and
nothing in the cache root is ever assumed to still be there.

## Backup

Exclude `layers/` — it is large and entirely re-downloadable. On Apple platforms
this is `NSURLIsExcludedFromBackupKey`. The call lives in the `tpk-backup`
crate: it is `unsafe` on Apple platforms and `tauri-plugin-tpk` is
`#![forbid(unsafe_code)]`, so the one unsafe line sits alone in an Apple-only
leaf crate; everywhere else it is a no-op with no dependencies.

Two details decide when it runs. The flag cannot be set on a directory that does
not exist, so it goes on after `Store::open` has created the tree, not before —
setting it first left a fresh install's layer pool in iCloud until the second
launch. And Apple does not document whether a file created inside an excluded
directory inherits the flag; reports disagree. So it is re-applied after every
successful stage, which is the only thing that adds files to the pool. One
syscall, and setting it twice is harmless.

A failure is logged and startup continues; the cost is backup quota, not a
broken app.

On Android it belongs in the host app's data extraction rules, because a plugin
cannot merge into that manifest:

```xml
<data-extraction-rules>
  <cloud-backup>
    <exclude domain="file" path="tpk/layers/" />
  </cloud-backup>
</data-extraction-rules>
```

Android's Auto Backup has a 25 MB ceiling, and exceeding it causes the **whole
app's** backup to be skipped — silently. A layer pool will exceed it.

Do **not** exclude `state.json`. Carrying the blacklist and the watermark floor
across a device migration is the behaviour you want: a user restoring onto a new
phone should not be handed a release already known to be broken.

## Garbage collection

`Store::gc()` retains files referenced by staged, booting and committed
revisions, plus layer files and delta results pinned by the current process's
frozen resolver. It runs after a successful `notifyReady()` and after `reset()`.
Reset clears the persistent revisions but keeps resources the running WebView
still needs. Runtime pins are not persisted; after the next cold start, a
collection can reclaim those old files. A failed collection after a commit is
only logged.

The blacklist is capped at 256 entries; the oldest are dropped first. A device
that has seen more than 256 bad releases has a different problem.

## Inspecting it

```bash
# macOS
ls -la ~/Library/Application\ Support/com.example.app/tpk/
python3 -m json.tool ~/Library/Application\ Support/com.example.app/tpk/state.json

# Linux
ls -la ~/.local/share/com.example.app/tpk/

# Windows
dir %APPDATA%\com.example.app\tpk\

# Android (debuggable build)
adb shell run-as com.example.app ls -la files/tpk/

# iOS simulator
xcrun simctl get_app_container booted com.example.app data
```

To start from scratch, delete the `tpk` directory in both roots, or call
`reset({ clearBlacklist: true })`. See [Local testing](./local-testing.md).
