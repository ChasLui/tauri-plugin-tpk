---
title: Overlay resolution
description: How layers stack, what wins, and what each operation means.
---

# Overlay resolution

A running app serves assets from a stack. The binary's embedded assets are the
floor; every installed pack is a layer above it. For any path, the **highest
layer that mentions it wins**.

```
  ▲  mod        unsigned user content, desktop only, off unless enabled
  │  dlc        optional add-on content
  │  patch      file-level diffs, in version_code order
  │  base       a complete content tree
  ▼  embedded   what the binary shipped with
```

Order is decided by the store when a revision is staged — `(kind, version_code)`
— and handed to the resolver as a flat slice. The resolver never re-sorts;
keeping the policy out of the resolver is what lets the overlay be tested
without a disk.

## Operations

Every entry in a pack's manifest names a path and does one of three things.

### `full`

Replace the path with the decoded blob. The commonest case: whatever was below,
including the embedded asset, is no longer visible at that path.

### `delta`

Reconstruct the path by applying a bsdiff patch to whatever the layers below
resolve to. `delta_base_sha256` records the digest the base must have; if the
layers below produce something else the entry fails rather than producing
plausible garbage.

Deltas are applied when the layer is staged, not at boot — see
[Architecture](./architecture.md#where-deltas-are-applied).

### `delete`

A tombstone. The path stops being served, **including from the embedded
assets**. This is the whole reason tombstones exist: a patch that removes a
route has to be able to hide a file the binary still carries.

A tombstone is not the same as a missing entry:

| Lookup result | Behaviour |
|---|---|
| Winning entry is `full`/`delta` | Serve it |
| Winning entry is `delete` | 404. Does **not** fall through to embedded |
| No entry in any layer | Fall through to embedded |
| Layer present but unreadable | Record the failure, fall through to embedded |

## The index

The index is built once during `setup` and stores exactly one `Loc` per path —
the winning layer, the winning entry, and its operation. Serving an asset is
therefore one hash lookup, not a walk down the stack, and index memory stays
around 150 bytes per entry rather than growing with layer count.

This is also why layers are frozen for the process lifetime: a `OnceLock`, not a
`RwLock`. Swapping the index under a WebView that has already imported half a
bundle mixes modules from two revisions.

## Pack kinds

| Kind | What it is | Constraints |
|---|---|---|
| `base` | A complete content tree | One per id; the bottom layer above embedded |
| `patch` | A diff against a parent | `parent` must name an installed pack's exact `version_code` |
| `dlc` | Optional add-on content | Not available on App Store targets |
| `mod` | Unsigned user content | Desktop only, and only when explicitly enabled |

`PackKind` deliberately has no `#[serde(other)]` catch-all. An unknown kind
fails with `E_SPEC` rather than degrading into a layer that is silently ignored.

`dlc` and `mod` do not compile for App Store targets; the reasons are policy,
not technical, and are set out in
[Security](./security.md#platform-availability).

## Patch chains

A `patch` names its parent by `(id, version, version_code)` and the match must
be **exact**. There is no "close enough" — a patch built against
`version_code: 20260911120000` will not apply to `20260911130000`, and the
client will plan a full base instead.

`version_code` is monotonic per pack id and the floor is derived from the layers
actually on disk, not from `state.json`. Deleting `state.json` does not let a
downgrade back in.

## CSP

Pack content runs on `tauri://localhost`, the same origin the embedded assets
use, so same-origin `<script src>` is already allowed by the `'self'` Tauri
force-injects into the policy. Overlay scripts need no hashes.

Inline scripts are the problem: the CSP baked into the binary carries hashes
computed from the *compile-time* HTML, and an overlay that replaces that HTML
with different inline script would be blocked by a hash nobody can update
without a shell release. So `tpk pack` **rejects HTML containing inline or
remote `<script>` outright**, and when the overlay owns an HTML file its stale
compile-time hashes are dropped rather than inherited.

If your build inlines a bootstrap script, configure it not to. That is a
one-line change in every bundler worth using.

## Path rules

Paths are validated when a manifest is parsed, not when it is used:

- absolute, POSIX, `/`-separated
- NFC-normalized
- no `.` or `..` segments, no empty segments
- no drive letters, no NUL
- no reserved prefixes (`/.tauri`, `/__tauri`)

Asset keys arriving from the WebView are *normalized* rather than rejected —
they come from the platform, not from a pack, and the platform is allowed to be
sloppy. Note that Tauri's `AssetKey::from` prepends a `/`, so an empty key
becomes `/` and then falls through to `/index.html`. That is correct behaviour,
not a bug.
