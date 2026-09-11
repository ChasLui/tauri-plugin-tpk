---
title: App review checklist
description: What to disclose, what to test, and what will get you rejected.
---

# App review checklist

> This project makes no compliance guarantee. Shipping is your responsibility,
> and policies change. Quotations were current on 2026-09-11. Read
> [Security](./security.md#store-compliance-boundaries) first — it has the
> citations; this page has the actions.

## Before you submit

### Disclose it

Guideline **2.3.1(a)** forbids "hidden, dormant, or undocumented features" and
requires new functionality to be "described with specificity in the Notes for
Review section … and accessible for review". An OTA mechanism is precisely what
that sentence is about. Do not let a reviewer discover it.

A Notes for Review paragraph that has worked:

> This app updates its web frontend (HTML, CSS, JavaScript, images) over the air
> using signed content packages rendered by WKWebView. No native code, no
> libraries and no executable files are downloaded or loaded. The app's
> features, its native capabilities and its data collection are fixed at build
> time and are not changed by these updates. Content is signed with Ed25519 and
> verified before use; the update endpoint is compiled into the binary and
> cannot be changed at runtime. Update checking is off by default and is
> user-initiated under Settings → Check for Updates.

Adjust the last sentence to whatever is actually true. Do not claim
user-initiated updates if `auto_check_on_launch` is on.

### Audit the capabilities

Pack content runs on `tauri://localhost` and inherits every capability the
window has. A window with `shell:allow-execute` turns a content update into
arbitrary native code execution — which is a different submission, and a
different conversation with a reviewer.

- [ ] `status().unsafe_capabilities` is empty in a smoke test
- [ ] `src-tauri/capabilities/` reviewed by hand — the runtime scan is a smoke
      alarm, not an audit
- [ ] No `shell:`, no `process:allow-restart`, no `fs:` write permission on the
      window that renders pack content

### Check the pack kinds

- [ ] No `dlc` packs in a mobile build (they do not compile for App Store
      targets — if yours does, check your target configuration)
- [ ] No `mod` packs, and no UI enabling third-party content (Guideline
      3.2.2(i))
- [ ] Nothing a pack delivers unlocks a feature that is not already in the
      binary (DPLA §3.3.1(C) — free does not make it acceptable)

### Check the defaults

- [ ] `auto_check_on_launch` is `false` on mobile unless you have a reason and
      have disclosed it
- [ ] `auto_download` likewise
- [ ] A first launch with no network shows the embedded frontend and works

### Check the backup story

Apple's review checklist names *Optimizing Your App's Data for iCloud Backup* as
expected reading.

- [ ] `layers/` excluded from iCloud backup
- [ ] Android: `<exclude domain="file" path="tpk/layers/" />` in the data
      extraction rules — Auto Backup's 25 MB ceiling silently skips the whole
      app's backup when exceeded
- [ ] `state.json` **not** excluded

### Check the launch path

- [ ] No delta is applied during `boot()` — materialization happens at stage
      time, because a slow boot trips the iOS 20-second watchdog or an Android
      ANR, which kills the process, which increments `boot_attempts`, which
      blacklists a pack that was never broken
- [ ] Cold start measured with a realistic layer stack on the slowest device you
      support
- [ ] The binary ships an `index.html` — `status().has_embedded_fallback` is
      `true`, so a rollback has somewhere to land

### Test the review build

Reviewers test the binary you submit, not your dev build.

- [ ] `cargo tauri ios build --debug` / `android build --debug` — not
      `ios dev` / `android dev`, which proxy every asset request to the dev
      server so `Assets::get()` is never called and you test nothing
- [ ] Airplane mode: the app launches and works on embedded assets
- [ ] A staged pack applies on the next cold start
- [ ] A deliberately broken pack rolls back after three launches

## Google Play

The [Device and Network Abuse](https://support.google.com/googleplay/android-developer/answer/16559646)
policy explicitly permits "code that runs in a virtual machine or an interpreter
… such as JavaScript in a webview or browser". That carve-out is in the policy
text, which makes Play the easier of the two.

Two things to still get right:

- [ ] The Data safety form reflects what your packs actually collect. Content
      that starts collecting something new needs a form update, and the form is
      a store-side artefact
- [ ] Nothing a pack delivers violates another Play policy — "interpreted
      languages loaded at runtime must not allow potential violations", and the
      packs are yours

## Things that get rejections

| Mistake | Why |
|---|---|
| Not disclosing OTA in Notes for Review | 2.3.1(a) — undocumented feature |
| Shipping `dlc`/`mod` on iOS | 3.1.1 names game levels; 3.2.2(i) names third-party content UI |
| A pack that unlocks a feature the binary does not have | DPLA §3.3.1(C) |
| A window with `shell:` permissions rendering pack content | It is not a content update any more |
| Claiming OTA is "a way to avoid review" in public docs | You told them |
| Downloading before the first screen | Poor review experience, and 2.5.2 attention you do not want |

## After approval

- [ ] Keep the secret key out of the repository and out of the runner's disk
- [ ] Keep `--created-at` pinned in CI so rebuilds are byte-identical
- [ ] Roll out at 10% first and watch `E_*` telemetry
- [ ] Have a store release ready: a compromised key cannot be retired until one
      ships. See [Security](./security.md#key-rotation-and-its-real-sla)
