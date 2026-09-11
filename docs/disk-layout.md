---
title: Disk layout
description: What lives where, which root the OS may purge, and what to exclude from backup.
---

# Disk layout

Two roots, and the split matters.

```
$APPLOCALDATA/tpk/            durable — survives, must be backed up selectively
├── state.json                the three-state pointer, watermarks, key epoch floor
├── blacklist.json            condemned releases
├── keys-cache.json           audit trail for key epoch changes
└── layers/
    └── <file_sha256>.tpk     the content-addressed layer pool

$APPCACHE/tpk/                purgeable — the OS may delete this at any moment
├── materialized/
│   └── <sha256>              delta results, named by the hash of their content
└── tmp/
    └── *.part                in-flight downloads
```

## Why a content-addressed pool

The specification describes three parallel directories (`staged/`, `booting/`,
`committed/`) holding copies of the same packs. This implementation keeps one
pool keyed by the file's own SHA-256, and `state.json` refers into it.

That turns promotion into a single atomic `state.json` write. Moving directories
around is not atomic, and a crash halfway through leaves a pointer to a revision
whose files are partly in two places — which is exactly the situation the state
machine exists to prevent.

It also deduplicates: a `base` shared by the committed and staged revisions is
stored once.

## Atomicity

`state.json` is written through a temp file, `fsync`, `rename`, then an `fsync`
of the parent directory. Without that last step the rename can be reordered past
a power loss on several filesystems and the file comes back empty.

Layer files land as `.part` in the cache root and are only moved into `layers/`
after the hash matches. A pack that is present in the pool has, by construction,
already been verified.

## What the OS may delete

`$APPCACHE` is purgeable on every platform this plugin targets. iOS will clear it
under storage pressure without telling you; Android's cache directory is the
first thing the system reclaims.

So the lazy re-materialization path must always exist: if a delta result is
missing when it is needed, it is rebuilt from the layer pool. Nothing in the
durable root is ever assumed to be reconstructible from the cache root, and
nothing in the cache root is ever assumed to still be there.

## Backup

Exclude `layers/` — it is large and entirely re-downloadable. On Apple platforms
this is `NSURLIsExcludedFromBackupKey`, which the plugin sets. On Android it
belongs in the host app's data extraction rules, because a plugin cannot merge
into that manifest:

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

`Store::gc()` removes layer files no revision refers to. It runs after a commit,
so the revision that was rolled back releases its exclusive layers once the
rollback is acknowledged as final — not before, because a rollback that is
itself rolled back needs them.

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
