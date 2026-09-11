---
title: Philosophy
description: Why this plugin exists, what it deliberately refuses to do, and when you should not use it.
---

# Philosophy

## The problem

A Tauri app ships its frontend inside the binary. Changing a button's label
means a new binary: rebuild, re-sign, re-notarize, re-submit, wait for review,
wait for users to update. For a native API change that cost is unavoidable. For
a typo it is absurd.

TPK lets the frontend ship on its own schedule while the shell — the Rust
binary, its commands, its capabilities — keeps shipping through the store.

## What it is not

**Not a way around app review.** Anything that changes what the app *does*
belongs in a store release. This plugin is for the parts that were always just
data: markup, styles, scripts, images, copy. See
[Security](./security.md#store-compliance-boundaries) for where the line is and
who drew it.

**Not a native updater.** The shell is updated by `tauri-plugin-updater`, the
app store, or your packaging system. TPK never replaces a binary. The two are
complementary and the boundary is explicit — see
[The updater boundary](./updater-boundary.md).

**Not a hot reload.** There is no way to swap layers in a running process, on
purpose. A WebView that has already imported half a bundle would end up mixing
modules from two revisions, and the failure mode is a blank screen with a stack
trace nobody can reproduce. Updates apply on the next cold start.

## Design commitments

### Nothing executable is written to disk

Pack contents are parsed out of a ZIP into memory and handed to the WebView
through Tauri's `Assets` trait. No file the OS could execute is written, no
dynamic library is loaded, `$RESOURCE` is never touched. This is what makes the
mechanism defensible under Apple's DPLA §3.3.1(B) and Google Play's interpreter
exemption — and it is an architectural property, not a policy we promise to
follow.

### A bad release cannot brick the app

A downloaded revision is `staged`. On the next cold start it becomes `booting` —
on trial. Only when the frontend calls `notifyReady()` does it become
`committed`. Three unacknowledged launches and it is rolled back and
blacklisted. The three-launch threshold exists because an OS kill, a power loss
and the user quitting are ordinary events; treating the first of them as "bad
pack" condemns perfectly good releases.

### Business outcomes are not errors

"You are up to date", "your shell is too old for this content", "this release
was blacklisted", "updating is disabled" are answers, not exceptions.
`check()` and `download()` return them as `status` values. `Err` is reserved for
things the caller cannot act on. Frontends that have to `try/catch` to learn
they are current end up swallowing real failures alongside.

### The update source is not runtime state

`manifest_url` and `pubkeys` live in `tauri.conf.json` and have no setter and no
command. A scripting bug in your frontend cannot repoint the updater at an
attacker's CDN. This costs some flexibility and buys the property that the
signed content chain has exactly one root, fixed at build time.

### Every rule lives in one place

Path validation, manifest cross-field rules, signature verification and the
container layout are all in `tpk-format`, and everything else — the CLI, the
store, the plugin — goes through it. The CLI that builds a pack and the client
that consumes it run the same code, so a pack that packs is a pack that loads.

### Determinism is a feature

`tpk pack` requires `--created-at` rather than defaulting to `now()`. Packing
the same input twice produces the same bytes, which is what makes the
blacklist's `(id, version_code)` matching and the channel manifest's SHA-256
mean anything. A CI rerun that produces a different hash for identical input
would let a condemned release slip back in.

## When not to use this

- **Your update is a native change.** New command, new capability, new
  permission, newer platform API — ship a binary.
- **You need instant rollout.** Updates apply on the next cold start. A
  long-lived desktop app may not cold start for days.
- **Your frontend is tiny.** If the bundle is 200 KB, a full store release is
  not the bottleneck and this is machinery you do not need.
- **You cannot control the signing key's custody.** A compromised key cannot be
  fully retired until a shell update ships. If that is unacceptable, so is this.
- **You are shipping `mod` or `dlc` content to mobile.** Those pack kinds do not
  compile for App Store targets. That is not an oversight.

## Further reading

- [Architecture](./architecture.md) — what runs when
- [Overlay resolution](./overlay.md) — how layers stack
- [Disk layout](./disk-layout.md) — what lives where, and what the OS may delete
- [Security](./security.md) — threat model and store boundaries
