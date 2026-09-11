---
title: Security
description: Threat model, what the plugin guarantees, and the app store boundaries you are responsible for.
---

# Security

## What is actually guaranteed

These are architectural properties, not promises — they follow from how the
plugin is built, and you can check each one:

- **Nothing executable is ever written.** Pack contents are parsed out of a ZIP
  into memory and handed to the WebView through the `Assets` trait. Nothing is
  written to disk as a file the OS could execute, no dynamic library is loaded,
  and `$RESOURCE` is never touched.
- **The rendering engine is the platform's.** iOS uses WKWebView
  (`wry/src/wkwebview/ios`), Android the system WebView. No custom interpreter,
  no JIT, no dynamically linked native code.
- **Content is verified before it is used.** A pack's manifest is signed with
  minisign (Ed25519); each blob carries its own digest, checked before decoding;
  the decoded result is checked again against the digest the signed manifest
  declares.
- **The update source cannot be changed at runtime.** `manifest_url` and
  `pubkeys` are native configuration with no setter and no command. A scripting
  bug in your frontend cannot repoint the updater.
- **A bad pack cannot brick the app.** A revision is on trial until the frontend
  acknowledges it; an unacknowledged one is rolled back and blacklisted.

## Threat model

| Threat | What stops it |
|---|---|
| Tampered CDN | minisign signature over the manifest bytes, plus a SHA-256 on every pack in the channel manifest and on every blob inside a pack |
| Replayed old manifest | Monotonic `watermark`, tracked per channel |
| Downgrade to known-bad content | Monotonic `version_code` per pack id, with the floor derived from the layers actually on disk — deleting `state.json` does not lower it |
| Half-written pack | Downloads land in `.part` and are only renamed in after the hash matches; `state.json` is written atomically with a directory fsync |
| Bad pack causing a blank screen | Three-state pointer: unacknowledged revisions roll back, and the release is blacklisted |
| A bad release being republished | The blacklist matches on `(id, version_code)` as well as on hash, so a CI rerun does not slip past it |
| Zip slip / path traversal | Paths are validated on parse: absolute, POSIX, NFC, no `..`, no empty segments, no drive letters, no reserved prefixes |
| Decompression bomb | Every blob declares `blob_size`; the zstd reader is bounded by it, and delta output is bounded by the signed `size` and by `max_asset_bytes` |
| Smuggled content | The container may only hold the manifest, its signature and blobs named by their own digest. Unreferenced blobs are rejected |
| Key compromise | Multiple trusted keys with a monotonic `key_epoch` floor; see below |

## Key rotation, and its real SLA

Multiple public keys let you rotate **in**. Rotating **out** is the hard part:
the trusted list is compiled into the binary, so removing a compromised key
requires a shell update — through the very channel OTA exists to avoid.

`key_epoch` narrows that window. Each trusted key carries a generation, the
channel manifest declares which generation signed it, and the client keeps a
monotonic floor. Whoever holds a retired key cannot sign a manifest claiming a
newer generation, so the only direction they can push the floor is up — which
retires them faster.

**The honest SLA**: a compromised key stops being accepted after
*shell update adoption time + one content publish*. On desktop that is weeks; on
mobile it can be months. Plan key custody accordingly.

## Capability inheritance

This is the part that surprises people.

Pack content runs on the `tauri://localhost` origin, which means **it inherits
every capability granted to that window**. The guarantee "a pack cannot contain
a dylib" is true. The guarantee "a pack cannot reach native functionality" is
not — if the window can, the pack can.

If your main window has `shell:allow-execute`, a signed content pack is
equivalent to arbitrary native code execution. That is a very different thing
from a content update, both technically and for App Review.

`status()` reports `unsafe_capabilities`: the entries of
`app.security.capabilities` that match a known-risky permission
(`shell:allow-execute`, `shell:allow-spawn`, `shell:allow-open`, `shell:default`,
`process:allow-restart`, `fs:allow-write-file`, `fs:allow-write-text-file`,
`fs:default`). Empty is what you want — assert it in a smoke test.

It is a smoke alarm, not an audit. It reads the capability set present in the
runtime config and matches on permission identifiers, so a permission not on
that list, or one reaching native functionality indirectly, will not show up.
Reviewing `src-tauri/capabilities/` by hand is still your job.

A useful asymmetry: iOS sandboxes forbid `fork`/`exec` and there is no sidecar,
so the worst case there is materially smaller than on desktop. The real
hazard zone is macOS App Store and Microsoft Store builds.

## `notifyReady()` is self-attestation

The acknowledgement is sent by the very code being judged. A pack that can run
its JavaScript at all can commit itself, even if the page is blank, the CSS is
missing or every route 404s.

The three-state machine protects against *content that cannot run*. It cannot
protect against *content that runs and is wrong*. Reduce the gap by calling
`notifyReady()` after your first screen has actually rendered — not at the top
of your entry point:

```ts
await router.isReady()
await firstDataLoad()
await notifyReady()   // now it means something
```

## Store compliance boundaries

> This project makes no compliance guarantee. Shipping is your responsibility,
> and policies change. Quotations below were current on 2026-09-11.

### Apple

Guideline **2.5.2** says an app may not "download, install, or execute code
which introduces or changes features or functionality of the app". There is no
WebView carve-out in the guidelines — the interpreted-code permission lives in
the **Developer Program License Agreement §3.3.1(B)**, which allows downloaded
interpreted code provided it:

1. does not change the app's primary purpose,
2. does not bypass signing, sandbox or other OS security features, and
3. (App Store builds) does not create a store or storefront.

DPLA **§3.3.1(C)** is stricter and less well known: an app "may not provide,
unlock or enable additional features or functionality through distribution
mechanisms other than the App Store" — with no mention of payment. Free does not
make it acceptable.

Guideline **2.3.1(a)** is the one most often missed: no "hidden, dormant, or
undocumented features", and all new functionality "must be described with
specificity in the Notes for Review section … and accessible for review". An OTA
mechanism is exactly what that sentence is about. See
[the review checklist](./app-review-checklist.md).

### Google Play

[Device and Network Abuse](https://support.google.com/googleplay/android-developer/answer/16559646)
states the restriction on downloading executable code "does not apply to code
that runs in a virtual machine or an interpreter … **such as JavaScript in a
webview or browser**" — an explicit permission, in the policy text itself.

Two caveats. The same policy says an app "may not modify, replace, or update
itself using any method other than Google Play's update mechanism"; the
interpreter carve-out grammatically attaches to the *other* sentence. Practice
treats this as being about APK replacement, but that is convention, not text.
And interpreted languages loaded at runtime "must not allow potential violations
of Google Play policies" — what your packs deliver is your responsibility.

### What must ship through the store

| Change | Route |
|---|---|
| HTML, CSS, JS, images, copy, layout | TPK |
| Bug fixes in existing features | TPK |
| A new Tauri command | Store update |
| A new capability or permission | Store update |
| A new top-level route or feature area | Store update |
| A new purchase flow | Store update (and IAP) |
| New data collection | Store update (and a privacy label change) |
| Anything needing a newer native API | Store update |

### Platform availability

| Pack kind | Desktop | iOS | Android |
|---|---|---|---|
| `base`, `patch` | ✅ | ✅ | ✅ |
| `dlc` | ✅ | ❌ | ❌ |
| `mod` | ✅ | ❌ | ❌ |

`dlc` is unavailable on mobile because Guideline 3.1.1 lists "game levels"
verbatim as requiring in-app purchase, IAP Attachment §2.4 permits only *data*
to be downloaded after a purchase, and §3.3.1(C) covers the free case. `mod`
is unavailable because an enable/disable UI for third-party packages is what
Guideline 3.2.2(i) describes.

## Backup and storage

`layers/` should be excluded from platform backups: it is large and entirely
re-downloadable. Apple's review checklist names *Optimizing Your App's Data for
iCloud Backup* as expected reading, and on Android exceeding Auto Backup's 25 MB
ceiling causes the whole app's backup to be skipped silently.

`state.json` should **not** be excluded — carrying the blacklist and the
watermark floor across a device migration is the behaviour you want.

For Android, add to your data extraction rules:

```xml
<data-extraction-rules>
  <cloud-backup>
    <exclude domain="file" path="tpk/layers/" />
  </cloud-backup>
</data-extraction-rules>
```

## Reporting a vulnerability

See [SECURITY.md](https://github.com/ChasLui/tauri-plugin-tpk/blob/main/SECURITY.md).
