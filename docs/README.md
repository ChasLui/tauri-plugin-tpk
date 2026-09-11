# Documentation

OTA frontend updates for Tauri v2. Content ships as signed `.tpk` packs that
stack as layers over the binary's embedded assets.

## Start here

- [Philosophy](./philosophy.md) — why this exists, and when not to use it
- [Configuration](./configuration.md) — every field of `plugins.tpk`
- [API reference](./api-reference.md) — the TypeScript and Rust surfaces
- [Packaging](./packaging.md) — building, signing and publishing with the CLI

## How it works

- [Architecture](./architecture.md) — the crates, startup, the state machine
- [Overlay resolution](./overlay.md) — how layers stack and what wins
- [Disk layout](./disk-layout.md) — what lives where, and what the OS may delete
- [Server contract](./server-contract.md) — the channel manifest

## Shipping it

- [Security](./security.md) — threat model and store boundaries
- [App review checklist](./app-review-checklist.md) — what to disclose and test
- [The updater boundary](./updater-boundary.md) — pack or binary
- [Error codes](./error-codes.md) — the fourteen frozen codes
- [Local testing](./local-testing.md) — serving a channel from your laptop

## Migrating

- [From tauri-plugin-hotswap](./migrating-from-hotswap.md)

## Specification

The frozen format contract is `spec/tpk-v1.md`. **Appendix A of that file
overrides the body** — it records where the original specification conflicts
with Tauri's real API, with App Store and Play policy, and with measured
performance.
