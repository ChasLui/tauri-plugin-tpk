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
| `dlc` | Optional add-on content | Removed at compile time by the `app-store` feature |
| `mod` | Unsigned user content | Desktop only, only when explicitly enabled, and removed by `app-store` |

`PackKind` deliberately has no `#[serde(other)]` catch-all. An unknown kind
fails with `E_SPEC` rather than degrading into a layer that is silently ignored.

`dlc` and `mod` must not be available on App Store targets; the reasons are
policy, not technical, and are set out in
[Security](./security.md#platform-availability). Build with
`--features app-store` and those two variants stop existing: a pack manifest
declaring `kind: dlc` fails with `E_POLICY`, and `tpk pack --kind` no longer
accepts them.

A channel manifest is treated differently. An entry whose kind is exactly `dlc`
or `mod` is **dropped** during deserialization rather than failing the whole
document, so one DLC entry cannot block base and security updates for every App
Store client reading that channel. Any other unknown kind still fails the
manifest.

The JSON schemas in `spec/json-schema/` keep listing all four kinds on purpose:
they describe the wire format, which is the same for every build target.

## Patch chains

A `patch` names its parent by `(id, version, version_code)` and the match must
be **exact**. There is no "close enough" — a patch built against
`version_code: 20260911120000` will not apply to `20260911130000`; the client
skips it and plans only from what the channel lists — a newer base, or the
patches in between.

`version_code` is monotonic per pack id, and the client answers two questions
with two different numbers. What a patch may stack on is the version really
installed — the layers in the stack right now. How far back an id may go is
`version_floor` in `state.json`: the highest `version_code` that id ever
committed, raised at commit, never lowered, kept across a `reset`. Only a
strictly older `version_code` is a downgrade; equality is a reinstall, which is
what lets a reset device re-fetch the version it was already running.

A blacklisted layer does not count as installed. One predicate decides that, and
the resolver's stack, the inherit filter at stage time and the planner all use
it, so they cannot disagree. This matters in one case: when a base is condemned,
the installed version for that id drops, and the client starts asking for a full
base instead of patches. Without it the planner would keep choosing patches on a
base the stack had already dropped, and every one of them would be refused at
stage with `E_PARENT` — the same doomed download on every check. `version_floor`
does not drop with it, so condemning a base does not open the door to an older
signed one.

## CSP

Pack content runs on `tauri://localhost`, the same origin the embedded assets
use, so same-origin `<script src>` is already allowed by the `'self'` Tauri
force-injects into the policy. Overlay scripts need no hashes.

Inline scripts are the problem: the CSP baked into the binary carries hashes
computed from the *compile-time* HTML, and an overlay that replaces that HTML
with different inline script would be blocked by a hash nobody can update
without a shell release. So `tpk pack` **rejects HTML containing inline or
remote `<script>`** (inline `application/json` / `application/ld+json` data
blocks aside, see [Packaging](./packaging.md#html-rules)), and when the overlay
owns an HTML file its stale compile-time hashes are dropped rather than
inherited.

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
