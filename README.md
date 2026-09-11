<p align="center">
  <h1 align="center">🔥🔁 tauri-plugin-tpk</h1>
  <p align="center">
    Open-source OTA frontend updates for Tauri v2 — ship HTML/CSS/JS changes without rebuilding the native binary. Self-hosted, signed, auto-rollback.
  </p>
</p>

<p align="center">
  <a href="https://crates.io/crates/tauri-plugin-tpk"><img src="https://img.shields.io/crates/v/tauri-plugin-tpk.svg" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/tauri-plugin-tpk-api"><img src="https://img.shields.io/npm/v/tauri-plugin-tpk-api.svg" alt="npm"></a>
  <a href="https://github.com/ChasLui/tauri-plugin-tpk/actions"><img src="https://github.com/ChasLui/tauri-plugin-tpk/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/ChasLui/tauri-plugin-tpk/blob/main/LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue" alt="License"></a>
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> ·
  <a href="https://chaslui.github.io/tauri-plugin-tpk/">Documentation</a> ·
  <a href="docs/api-reference.md">API Reference</a> ·
  <a href="docs/security.md">Security</a> 
</p>

---

## What is this?

An **open-source Tauri v2 plugin** that ships frontend updates to users without rebuilding the native binary and without requiring a cloud service. Self-hosted, bring your own CDN.

It works by swapping Tauri's embedded asset provider at startup. The WebView keeps loading from `tauri://localhost` — the swap is invisible. Your keys, your server, your infrastructure. If anything goes wrong, the app rolls back to embedded assets on next launch.

### Platform Support

| Platform | Status |
|----------|--------|
| macOS    | ✅ full |
| Windows  | ✅ full |
| Linux    | ✅ full |
| Android  | ⚠️ base/patch only — see [store boundaries](docs/security.md) |
| iOS      | ⚠️ base/patch only — see [store boundaries](docs/security.md) |

Content pack kinds by platform:

| Kind | Desktop | iOS | Android |
|------|---------|-----|---------|
| `base` / `patch` | ✅ | ✅ | ✅ |
| `dlc`  | ✅ | ❌ | ❌ |
| `mod`  | ✅ | ❌ | ❌ |


> **⚠️ This is not a tool for bypassing app store review.**
>
> TPK updates WebView content (HTML/CSS/JS/assets). Any change that alters what
> your app *does* — new native commands, new capabilities, new top-level routes,
> new purchase flows, new data collection — must ship through a store update.
>
> - **Apple**: content delivery is governed by the [Developer Program License
>   Agreement §3.3.1(B)](https://developer.apple.com/support/terms/apple-developer-program-license-agreement/),
>   which permits downloaded interpreted code only while it does not change the
>   app's primary purpose, does not bypass OS security, and does not create a
>   storefront. App Review cites [Guideline 2.5.2](https://developer.apple.com/app-store/review/guidelines/#software-requirements)
>   when it rejects. [Guideline 2.3.1(a)](https://developer.apple.com/app-store/review/guidelines/#accurate-metadata)
>   additionally requires you to disclose the OTA mechanism in Notes for Review.
> - **Google Play**: [Device and Network Abuse](https://support.google.com/googleplay/android-developer/answer/16559646)
>   states the restriction on downloading executable code *"does not apply to
>   code that runs in a virtual machine or an interpreter … such as JavaScript
>   in a webview or browser"*.
>
> Read [docs/security.md](docs/security.md) before shipping to a store. This
> project makes no compliance guarantee; the responsibility is yours.

### How it works

```mermaid
flowchart TD
    A["Your CDN / S3 / any HTTPS host
    manifest.json
    signed frontend.tar.gz"] -- "download + verify signature" --> B
    B["Tauri App

    HotswapAssets::get(key)
    1. filesystem (cached)
    2. embedded (fallback)"]
```

---

<a id="quickstart"></a>
## 🚀 Quickstart

### 1. Install

```toml
# src-tauri/Cargo.toml
[dependencies]
tauri-plugin-tpk = "0.1.0"
```

```bash
npm install tauri-plugin-tpk-api
```

### 2. Configure

Add to your `tauri.conf.json`:

```json
{
  "plugins": {
    "tpk": {
      "endpoint": "https://your-server.com/api/updates/{{current_sequence}}",
      "pubkey": "<YOUR_MINISIGN_PUBKEY>"
    }
  }
}
```

> **Config source matters:**
> - `init(context)` reads `plugins.tpk` from `tauri.conf.json` and requires it.
> - `init_with_config(context, config)` and `TpkBuilder` are programmatic paths; `plugins.tpk` in JSON is optional for these.

### 3. Register the plugin

```rust
// src-tauri/src/lib.rs
pub fn run() {
    let context = tauri::generate_context!();
    // init() consumes the context to swap the asset provider,
    // then returns the modified context alongside the plugin.
    let (tpk, context) = tauri_plugin_tpk::init(context)
        .expect("failed to initialize tpk");

    tauri::Builder::default()
        .plugin(tpk)
        .run(context)
        .expect("error running app");
}
```

Programmatic alternative (no `plugins.tpk` required in `tauri.conf.json`):

```rust
let context = tauri::generate_context!();
let config = tauri_plugin_tpk::TpkConfig::new("<YOUR_MINISIGN_PUBKEY>")
    .endpoint("https://your-server.com/api/updates/{{current_sequence}}");
let (tpk, context) = tauri_plugin_tpk::init_with_config(context, config)
    .expect("failed to initialize tpk");
```

### 4. Add capability

In `src-tauri/capabilities/default.json`:

```json
{
  "identifier": "default",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "tpk:default"
  ]
}
```

### 5. Use from the frontend

```typescript
import { checkUpdate, applyUpdate, notifyReady } from 'tauri-plugin-tpk-api';

// ✅ Confirm current version works (call on every startup)
await notifyReady();

// 🔍 Check for updates
const result = await checkUpdate();

if (result.available) {
  // ⬇️ Download, verify, and activate
  await applyUpdate();

  // 🔄 Reload to serve new assets
  window.location.reload();
}
```

That's it. A few lines to add OTA updates to your Tauri app.

You can also change configuration at runtime — for example, to switch channels without restarting:

```typescript
import { configure } from 'tauri-plugin-tpk-api';

// Switch to a beta channel at runtime
await configure({ channel: 'beta' });
```

---

## ✨ Features

| Feature | Description |
|---------|-------------|
| 🔐 **Signed bundles** | Every download is verified with minisign before extraction |
| ↩️ **Auto-rollback** | If `notifyReady()` isn't called, the next launch rolls back automatically |
| 📡 **Channels** | Route users to `production`, `staging`, `beta` — switchable at runtime via `configure()` |
| 🔑 **Custom headers** | Auth tokens, API keys — sent on every check and download request |
| 🔄 **Retry with backoff** | Failed downloads retry automatically (1s → 2s → 4s → 8s) |
| 🔀 **Download/activate split** | Download now, apply later — you control the timing |
| 📊 **Lifecycle events** | `tpk://lifecycle` events for telemetry (Sentry, PostHog, etc.) |
| 📏 **Bundle size + mandatory flag** | Warn users on mobile data, force security patches |
| 🌍 **Platform-aware** | Sends `platform`, `arch`, `channel` on every check request |
| 🛡️ **Size limits** | Configurable max bundle size prevents memory exhaustion |
| 🔒 **HTTPS enforced** | Non-HTTPS URLs rejected by default |
| ⚡ **Atomic operations** | Temp dir extraction + rename; temp file pointer writes |
| 🤖 **Custom resolvers** | `HotswapResolver` trait — bring your own update source |
| 📦 **Zip support** | Enable with `features = ["zip"]` |

---

## 📖 Documentation

| Document | Description |
|----------|-------------|
| **[Design Philosophy](docs/philosophy.md)** | Opinionated defaults, extensible when you need it |
| **[Configuration](docs/configuration.md)** | All config options, builder API, tauri.conf.json reference |
| **[API Reference](docs/api-reference.md)** | Full JS and Rust API with examples |
| **[Server Contract](docs/server-contract.md)** | What your update endpoint needs to return |
| **[Security](docs/security.md)** | Threat model, mitigations, signing guide |
| **[Architecture](docs/architecture.md)** | How the plugin works internally |
| **[Creating Bundles](docs/creating-bundles.md)** | Build, sign, upload your frontend bundles |
| **[CONTRIBUTING](CONTRIBUTING.md)** | How to contribute to this project |
| **[CHANGELOG](CHANGELOG.md)** | Version history |

---

## 🛡️ Security

Every update is **cryptographically signed** with minisign and verified before extraction. The plugin is designed to fail safely — if anything goes wrong, the app falls back to embedded assets.

See the full [Security documentation](docs/security.md) for the threat model and all mitigations.

---

## License

MIT OR Apache-2.0 (same as Tauri)
