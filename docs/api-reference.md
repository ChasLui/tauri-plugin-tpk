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
up to date, shell too old, disabled, degraded, failed. A rejected promise means
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

`notes` is CDN-controlled text. On parse, in this order: tag-like spans are
stripped — anything from a `<` to the next `>` is dropped, an unterminated `<`
swallows the rest, a stray `>` goes too — then formatting characters that can
hide or reorder text are dropped, runs of control characters collapse to a
single space, the result is trimmed, truncated to 200 visible characters, and
becomes absent if nothing is left. Character references are deliberately left
encoded, since decoding them could reintroduce the markup that was just removed.

The rule is *drop what can hide or reorder text*, not *drop everything
invisible*. Dropped: bidi embeddings and overrides (U+202A–U+202E), isolates and
the deprecated shaping controls (U+2066–U+206F), the direction marks U+200E /
U+200F and U+061C, zero-width space U+200B, soft hyphen U+00AD, U+180E, the BOM
U+FEFF, interlinear annotations (U+FFF9–U+FFFB), musical formatting controls
(U+1D173–U+1D17A) and the tag block (U+E0000–U+E007F). Kept: ZWNJ U+200C and ZWJ
U+200D, which Persian and Arabic shaping and emoji sequences need; the word
joiner and invisible math operators (U+2060–U+2064); and script-specific marks
such as U+0600–U+0605. Deleting those would corrupt real notes to defend against
nothing.

Render it as text; it is not a sanctioned message channel into your UI.

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

`download()` acts on the most recent plan, not on a specific one you were
handed. There is a single pending slot, so a launch auto-check and a `check()`
your UI ran can overwrite each other, and what gets staged may not be what the
user was shown. Both plans come from a signed manifest for the same channel and
every pack is re-verified at stage, so the outcome is a different valid revision
rather than an unchecked one.

Downloads resume: a partial transfer lands in `.part` and a later call continues
it with a Range request. Finished downloads are deleted when the call returns,
staged or not. If a finished download cannot be read back (the cache root was
purged), the result is `failed` with `E_IO`, not a rejection.

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
  rolled_back?: string;          // revision rolled back during this launch's setup
  unsafe_capabilities: string[]; // see Security
  has_embedded_fallback: boolean;
  last_error?: { code: ErrorCode; message: string };
}
```

`has_embedded_fallback: false` means the binary ships no `index.html`, so a
rollback has nowhere to land. Treat it as a build error.

`last_error` is the last failure that kept a revision from reaching `staged`: a
failed channel poll, a failed download, a refused stage. Business outcomes are
not failures and are never recorded — up-to-date, `shell_required`, blacklisted
and `disabled` all leave it untouched, so a device that is merely a shell
version behind does not show a permanent error. It is cleared by a successful
`check` (the manifest was fetched and verified), by a successful stage, and by
`notifyReady()`. Repeating the identical failure does not rewrite `state.json` —
an offline device polling on a loop fails the same way every time, and that is
not a change worth an fsync. It carries no timestamp: it answers "why is nothing
landing", not "when did it break".

`rolled_back` is how a rollback is reported. It happens in `setup`, before any
window exists, so no `tpk://state` event is emitted for it. The value is fixed
for the life of the process; `reset()` does not clear it.

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
excluded from. So do the three anti-downgrade floors — the per-channel
watermark, the `key_epoch` floor and `version_floor`, the highest `version_code`
each pack id has ever committed here. A support action must not weaken them. The
version floor refuses only what is strictly older, so a reset device can still
re-fetch the version it was running; it just cannot be walked backwards.

## When the plugin is off

With `enabled: false`, or when `setup` degraded (no config, unusable keys, an
unusable disk), no plugin state exists. Commands still answer in their
documented shapes: `check()` and `download()` return `{ status: "disabled" }`,
`notifyReady()` returns `{ status: "noop" }`, and `status()`, `reset()` and
`set_mod_enabled` reject with `E_DISABLED`.

## Events

```ts
const un = await onDownloadProgress(({ downloaded, total, pack_index, pack_count }) => {
  setProgress(downloaded / total);
});

await onState(({ pointer, rev }) => {
  // "staged" | "committed" | "reset" — rollbacks show up in status().rolled_back
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
