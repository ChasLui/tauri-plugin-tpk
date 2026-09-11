---
title: Error codes
description: The fourteen frozen codes, what causes each, and what to do about it.
---

# Error codes

Every failure anywhere in the stack maps to exactly one of fourteen codes. They
are frozen by the specification and serialize as:

```json
{ "code": "E_HASH", "message": "blob 3a1f… does not match its declared digest" }
```

Branch on `code`. `message` is for logs and support tickets, not for control
flow.

| Code | Cause | What to do |
|---|---|---|
| `E_DISABLED` | The plugin is off by configuration | Nothing. Expected |
| `E_NETWORK` | Transport failure reaching the manifest or a pack | Retry later. Common and usually not your bug |
| `E_SIGNATURE` | No trusted key verified the signature | Stop. Either the key rotated without a shell release, or the content is not yours |
| `E_HASH` | Content did not match its declared digest | Stop. Corrupt CDN object or a truncated transfer |
| `E_SPEC` | The document violates the TPK/1 schema | A publishing bug. Run `tpk verify` in CI |
| `E_PATH` | A path violates the path rules | A packing bug. `tpk pack` should have caught it |
| `E_PARENT` | A patch's parent link is missing or unsatisfiable | Publish a full base, or fix the chain |
| `E_SHELL` | The shell version is outside the pack's range | Route the user to a store update |
| `E_WATERMARK` | The manifest is older than one already seen | Usually a stale CDN edge. If persistent, a replay |
| `E_BLACKLIST` | The pack was condemned on this device | Investigate the release. `reset({clearBlacklist:true})` to retry |
| `E_IO` | Filesystem failure | Disk full, permissions, or a purged cache |
| `E_DELTA` | A delta could not be applied | The base resolved to something other than `delta_base_sha256` |
| `E_STATE` | On-disk state is missing or inconsistent | The plugin degrades to embedded assets. Report it |
| `E_POLICY` | A policy was violated | Non-default override globs, or a pack kind unavailable on this platform |

## Notes on the ones that surprise people

### `E_SIGNATURE` after a key rotation

The trusted key list is compiled into the binary. Signing with a key the
shipped shell does not know produces `E_SIGNATURE` on every device until a shell
update lands. Always publish with the new key **while the old one is still
listed**, and drop the old one only in a later shell release.

### `E_WATERMARK` that clears itself

A CDN edge serving a stale `latest.json` will produce this and then stop. If it
does not stop, something is replaying an old manifest and you should treat it as
an incident.

### `E_BLACKLIST` on a release you just fixed

The blacklist matches on the pack's hash **and** on `(id, version_code)`.
Rebuilding the same `version_code` with different content does not get past it —
which is deliberate, because otherwise a CI rerun could push a condemned release
back onto devices that already rejected it. Bump `version_code`.

### `E_DELTA`

A patch declares the digest the layers below it must resolve to. If they resolve
to something else the patch fails rather than producing plausible garbage.
Usually it means the patch was built against a different parent than the one
installed — check `parent_version_code`.

### `E_POLICY` for `dlc` / `mod` on mobile

Not a bug. Those kinds do not compile for App Store targets; the reasons are in
[Security](./security.md#platform-availability).

## Outcomes that are not errors

These come back as a `status` from `check()` / `download()`, not as a rejected
promise:

`up_to_date`, `available`, `shell_required`, `disabled`, `degraded`, `staged`,
`noop`. See [API reference](./api-reference.md#outcomes-are-not-errors).

`degraded` deserves attention even though it is not an error: it means three
releases in a row rolled back and the device has stopped updating itself. Surface
it in telemetry.

## Telemetry

```ts
await onError(({ code, message }) => {
  telemetry.count(`tpk.error.${code}`);
  if (code !== "E_NETWORK") telemetry.log(message);
});
```

`E_NETWORK` is noise at volume. Everything else is worth a log line — and a
rising `E_SIGNATURE` or `E_HASH` rate is worth a page.
