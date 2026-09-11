---
title: API reference
description: The TypeScript guest API and the Rust surface.
---

# API reference

```bash
pnpm add tauri-plugin-tpk-api
```

```ts
import {
  check, download, notifyReady, status, reset,
  onDownloadProgress, onState, onError,
} from "tauri-plugin-tpk-api";
```

## Outcomes are not errors

`check()` and `download()` return a `status` for every ordinary answer —
up to date, shell too old, blacklisted, disabled. A rejected promise means
something you cannot act on. Branch on `status`, not on `try/catch`.

## `check()`

```ts
type CheckOutcome =
  | { status: "up_to_date"; watermark: number }
  | { status: "available"; packs: PackSummary[]; bytes: number; notes?: string }
  | { status: "shell_required"; min_shell: string }
  | { status: "disabled" }
  | { status: "degraded"; consecutive_rollbacks: number };
```

`shell_required` means the channel is serving content this binary is too old
for — route the user to a store update, not to a retry. `degraded` means three
releases in a row rolled back and automatic updating has stopped; see
[Architecture](./architecture.md#the-three-state-machine).

```ts
interface PackSummary {
  id: string;
  kind: "base" | "patch" | "dlc" | "mod";
  version: string;
  version_code: number;
  size: number;
}
```

`notes` is CDN-controlled text, truncated to 200 characters on parse. Render it
as text; it is not a sanctioned message channel into your UI.

## `download()`

```ts
type DownloadOutcome =
  | { status: "staged"; rev: string; bytes: number }
  | { status: "up_to_date" }
  | { status: "shell_required"; min_shell: string }
  | { status: "disabled" }
  | { status: "failed"; code: ErrorCode; message: string };
```

`staged` means the revision is downloaded, verified and waiting. It applies on
the **next cold start**. Tell the user that; do not reload the WebView, which
would mix modules from two revisions.

Downloads resume: a partial transfer lands in `.part` and a later call continues
it with a Range request.

## `notifyReady()`

```ts
type ReadyOutcome = { status: "committed"; rev: string } | { status: "noop" };
```

Acknowledges that the running revision works. Until it is called the revision is
on trial, and three unacknowledged launches roll it back.

Call it **after your first screen has actually rendered**:

```ts
await router.isReady();
await firstDataLoad();
await notifyReady();
```

Calling it at the top of your entry point makes it meaningless — see
[Security](./security.md#notifyready-is-self-attestation). Calling it more than
once is harmless.

## `status()`

```ts
interface Status {
  pointer: "committed" | "booting";
  rev?: string;
  layers: LayerSummary[];
  shell: string;
  watermark: number;
  pending: boolean;              // a revision is waiting for the next cold start
  degraded: boolean;
  failed_layers: string[];       // layers that failed to load this launch, by hash
  unsafe_capabilities: string[]; // see Security
  has_embedded_fallback: boolean;
  last_error?: { code: ErrorCode; message: string };
}
```

`has_embedded_fallback: false` means the binary ships no `index.html`, so a
rollback has nowhere to land. Treat it as a build error.

## `reset(options?)`

```ts
await reset();                            // discard downloaded content
await reset({ clearBlacklist: true });    // also forget condemned releases
```

Requires `tpk:allow-reset`. `clearBlacklist` is for support tooling — it lets a
device retry a release that was found to be broken, which is exactly why it is
not the default.

`install_id` survives a reset on purpose: re-rolling it would move the device
into a different rollout bucket and hand it a release it had already been
excluded from.

## Events

```ts
const un = await onDownloadProgress(({ downloaded, total, pack_index, pack_count }) => {
  setProgress(downloaded / total);
});

await onState(({ pointer, rev }) => {
  // "staged" | "committed" | "rolled_back" | "reset"
});

await onError(({ code, message }) => {
  telemetry.record(code, message);
});

un(); // unsubscribe
```

Event names are `tpk://download-progress`, `tpk://state` and `tpk://error`.

## Errors

A rejected command throws `{ code, message }` where `code` is one of the
fourteen frozen codes. See [Error codes](./error-codes.md).

```ts
try {
  await download();
} catch (e) {
  const err = e as TpkError;
  if (err.code === "E_NETWORK") retryLater();
  else report(err);
}
```

## Rust

```rust
pub fn attach(context: &mut tauri::Context) -> TpkHandle;
pub fn init<R: Runtime>(handle: TpkHandle) -> TauriPlugin<R, Option<TpkConfig>>;
pub fn init_with_config<R: Runtime>(handle: TpkHandle, config: TpkConfig)
    -> TauriPlugin<R, Option<TpkConfig>>;
```

`attach` must run before `Builder::build`; it resolves no path and opens no
file. Everything stateful happens in the plugin's `setup` hook. See
[Architecture](./architecture.md#startup).

Also exported: `TpkConfig`, `PubKey`, `PackAssets`, `TpkState`, `Error`,
`Result`, and the outcome types `CheckOutcome` / `DownloadOutcome` /
`ReadyOutcome` / `Status`.

## Command names

| Command | Permission |
|---|---|
| `plugin:tpk\|check` | `tpk:default` |
| `plugin:tpk\|download` | `tpk:default` |
| `plugin:tpk\|notify_ready` | `tpk:default` |
| `plugin:tpk\|status` | `tpk:default` |
| `plugin:tpk\|reset` | `tpk:allow-reset` |
| `plugin:tpk\|set_mod_enabled` | `tpk:allow-mods` |
