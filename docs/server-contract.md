---
title: Server contract
description: The channel manifest, what the server must guarantee, and what it must never do.
---

# Server contract

The server is a static file host. There is no API, no session, no negotiation.
Two kinds of file:

- `latest.json` — the channel manifest, plus `latest.json.minisig`
- `*.tpk` — the packs, plus nothing (their signature is inside)

Any CDN or object store will do.

## Channel manifest

```json
{
  "spec": "tpk-channel/1",
  "channel": "stable",
  "published_at": "2026-09-11T12:00:00Z",
  "watermark": 202609111200,
  "key_epoch": 1,
  "min_shell": "1.4.0",
  "notes": "Fixes the checkout button.",
  "packs": [
    {
      "id": "core",
      "kind": "base",
      "version": "2.1.0",
      "version_code": 20260911120000,
      "url": "https://cdn.example.com/tpk/core/core-2.1.0.tpk",
      "size": 1048576,
      "sha256": "a3f1...",
      "rollout": 100
    }
  ]
}
```

`spec` must be exactly `tpk-channel/1`. Build it with `tpk channel`, never by
hand — the CLI computes sizes and digests from the actual files, which is the
whole point.

### Fields

| Field | Required | Meaning |
|---|---|---|
| `spec` | ✅ | `"tpk-channel/1"` |
| `channel` | ✅ | Must match the client's configured channel |
| `published_at` | ✅ | RFC 3339 |
| `watermark` | ✅ | Monotonic freshness marker, compared per channel |
| `key_epoch` | | Signing key generation. Default `1` |
| `min_shell` | | Lowest shell version any pack supports |
| `force_shell` | | Below this, no pack is applied at all |
| `notes` | | Truncated to 200 characters on parse |
| `packs` | ✅ | What is on offer |

### `packs[]`

| Field | Required | Meaning |
|---|---|---|
| `id` | ✅ | `[a-z0-9][a-z0-9-]{0,62}` |
| `kind` | ✅ | `base` / `patch` / `dlc` / `mod` |
| `version` | ✅ | SemVer, for humans |
| `version_code` | ✅ | Monotonic ordering key, per id |
| `parent_version_code` | for `patch` | The exact `version_code` this applies on top of |
| `url` | ✅ | Absolute https URL |
| `size` | ✅ | Exact byte size of the `.tpk` |
| `sha256` | ✅ | Digest of the `.tpk` file, lowercase hex |
| `optional` | | Clients may skip it. Default `false` |
| `rollout` | | 1..=100. Default `100` |

`(id, version_code)` must be unique within a manifest.

## Signature

`latest.json.minisig` is a detached minisign signature over the manifest bytes,
in **prehashed mode** — the `ED` tag, signing a BLAKE2b-512 of the payload.
That is minisign's default; legacy `Ed` signatures are not accepted.

Scrypt-encrypted secret keys are deliberately unsupported. CI reads the key from
the environment, and a passphrase in a CI variable protects nothing while adding
a way to get the automation wedged.

```bash
tpk channel --channel stable --pack core-2.1.0.tpk \
  --url-base https://cdn.example.com/tpk/core/ \
  --out latest.json
# `tpk channel` signs the manifest it writes
```

## Watermark

Monotonic, per channel. The client records the highest it has seen for each
channel and refuses anything lower, which is what stops an attacker replaying
last month's manifest to undo a security fix.

Per channel, not global: a single scalar makes a stable→beta→stable switch
discard every stable manifest forever, with no error anywhere.

Use UTC `YYYYMMDDHHMM` — `tpk channel --watermark auto` does. It is monotonic,
stateless, and unaffected by moving the repository or rebuilding CI. A counter in
a file will eventually be reset by someone.

Equal watermarks are accepted, so republishing an identical manifest is safe.

## `key_epoch`

Declares which key generation signed this manifest. The client keeps a monotonic
floor and refuses anything below it. Bump it in the same publish that introduces
a new key, and never lower it. See
[Security](./security.md#key-rotation-and-its-real-sla) for what this actually
buys you.

## Shell gating

`min_shell` is advisory per pack; the client skips packs the running shell is
too old for and reports `shell_required` if nothing is left.

`force_shell` is a blunt instrument: below it, **no** pack is applied, including
hotfixes for the version you are trying to retire. Use it when the shell should
not receive content at all, not to nudge people to upgrade.

## Staged rollout

`rollout` is a percentage. The client buckets itself with
`sha256(install_id:id:version_code)` — deterministic per device and per release,
so a device never flips in and out of a rollout, and raising 10 → 50 is a
superset rather than a reshuffle.

There is no server-side cohort tracking, and no way to target a device. That is
a feature: the manifest is a static file and stays one.

## Caching

```
latest.json         Cache-Control: max-age=60
latest.json.minisig Cache-Control: max-age=60
*.tpk               Cache-Control: max-age=31536000, immutable
```

Packs are content-addressed by the digest in the manifest, so they are immutable
by construction. Never overwrite a published `.tpk` — clients that already saw
the old digest will reject the new bytes, correctly, and you will spend an
afternoon on it.

## Range requests

Pack URLs should support `Range`. Without it an interrupted download starts over
rather than resuming. The client handles a server that answers `200` to a Range
request by discarding the partial file and starting again, so this is a
performance requirement, not a correctness one.

## Publishing order

1. Upload the `.tpk` files
2. Wait for CDN propagation
3. Upload `latest.json` and `latest.json.minisig`

Manifest last, always. A manifest advertising a pack that is not yet reachable
produces download failures for every client that polls in the gap.

## Verifying before you publish

```bash
tpk verify --pubkey "RWT..." --file latest.json --file core-2.1.0.tpk
```

Exit code `0` clean, `2` verification failed, `3` bad input. Wire it into the
publish job between packing and upload. See [Packaging](./packaging.md).
