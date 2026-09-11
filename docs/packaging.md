---
title: Packaging
description: Building, signing and publishing packs with the `tpk` CLI.
---

# Packaging

```bash
cargo install tpk-cli
```

Six subcommands. Exit codes: `0` success, `2` verification failed, `3` bad input
or usage.

## Keys

```bash
tpk keygen --out signing.key
# prints the public key to stdout
```

The secret key is unencrypted minisign. Put it in a CI secret and read it from
the environment; never commit it, and never write it to the runner's disk.
Scrypt-encrypted keys are deliberately unsupported — a passphrase in a CI
variable protects nothing while adding a way to get the automation wedged.

Copy the public key into `plugins.tpk.pubkeys`.

## Building a base pack

```bash
tpk pack \
  --kind base \
  --id core \
  --version 2.1.0 \
  --version-code 20260911120000 \
  --created-at 2026-09-11T12:00:00Z \
  --dist dist/ \
  --out core-2.1.0.tpk
```

### `--version-code`

Monotonic per pack id. Never reuse it, never lower it. UTC `YYYYMMDDHHMMSS` is
the recommended source: monotonic, stateless, and unaffected by moving the
repository or rebuilding CI.

```bash
VERSION_CODE=$(date -u +%Y%m%d%H%M%S)
```

### `--created-at`

Required, not defaulted to `now()`. Packing the same input twice must produce
the same bytes: the blacklist matches on `(id, version_code)` and the channel
manifest carries a SHA-256, and a CI rerun that produced different bytes for
identical input would let a condemned release slip back in.

```bash
CREATED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
```

Determinism also comes from sorted entries, a fixed DOS-epoch mtime, `Stored`
ZIP entries and a fixed zstd level. You get it for free as long as you pin this
one flag.

### Other flags

| Flag | Meaning |
|---|---|
| `--kind` | `base` / `patch` / `dlc` / `mod` |
| `--min-shell` | Lowest shell version this pack supports |
| `--max-shell` | Highest. Leave unset unless a specific break is known |
| `--channel` | Channel this pack is built for |
| `--parent` | Parent `.tpk`. Required for `--kind patch` |
| `--delta-threshold` | Files below this size are never deltified. Default 262144 |

## Building a patch

```bash
tpk pack \
  --kind patch \
  --id core \
  --version 2.1.1 \
  --version-code 20260911150000 \
  --created-at 2026-09-11T15:00:00Z \
  --parent core-2.1.0.tpk \
  --dist dist/ \
  --out core-2.1.1.tpk
```

The parent is read, compared against `--dist`, and each changed file becomes a
`full` or a bsdiff `delta` depending on which is smaller. Files gone from
`--dist` become `delete` tombstones.

The parent link must match **exactly** at install time. A patch built against
`version_code: 20260911120000` does not apply to anything else, and the client
will plan a full base instead of guessing.

Keep every published `.tpk` as a build artifact. You cannot produce a patch
without its parent.

## HTML rules

`tpk pack` rejects HTML containing inline or remote `<script>`.

Inline script is rejected because the CSP compiled into the binary carries
hashes computed from the *compile-time* HTML; overlay HTML with different inline
script would be blocked by a hash nobody can update without a shell release.
Remote script is rejected because it is an unsigned code path straight past
everything this format does.

If your bundler inlines a bootstrap script, turn that off. Vite:

```js
export default { build: { rollupOptions: { output: { inlineDynamicImports: false } } } }
```

## Building the channel manifest

```bash
tpk channel \
  --channel stable \
  --pack core-2.1.1.tpk \
  --url-base https://cdn.example.com/tpk/core/ \
  --watermark auto \
  --key-epoch 1 \
  --rollout 10 \
  --out latest.json
```

Repeat `--pack` per pack. Sizes and digests are computed from the files, so the
manifest cannot drift from what you are about to upload. `--watermark auto` uses
UTC `YYYYMMDDHHMM`.

Writes `latest.json` and `latest.json.minisig`.

## Verifying

```bash
tpk verify --pubkey "$TPK_PUBKEY" --file latest.json --file core-2.1.1.tpk
tpk inspect core-2.1.1.tpk --json | jq '.entries | length'
```

`verify` checks signatures and digests; `inspect` prints the manifest **without**
verifying it, which is why it is a debugging tool and not a gate.

`--min-epoch` makes `verify` reject anything signed with a retired key — set it
to the floor your clients have reached.

## A publish job

```bash
set -euo pipefail

VERSION_CODE=$(date -u +%Y%m%d%H%M%S)
CREATED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)

pnpm build

# The parent is the previously published pack; fetch it, do not rebuild it.
curl -fsSL "$CDN/core/previous.tpk" -o previous.tpk

tpk pack --kind patch --id core \
  --version "$VERSION" --version-code "$VERSION_CODE" --created-at "$CREATED_AT" \
  --parent previous.tpk --dist dist/ --out "core-$VERSION.tpk"

tpk channel --channel stable --pack "core-$VERSION.tpk" \
  --url-base "$CDN/core/" --watermark auto --out latest.json

# Gate before anything is uploaded.
tpk verify --pubkey "$TPK_PUBKEY" --file latest.json --file "core-$VERSION.tpk"

# Packs first, manifest last.
aws s3 cp "core-$VERSION.tpk" "s3://$BUCKET/tpk/core/"
aws s3 cp latest.json          "s3://$BUCKET/tpk/core/"
aws s3 cp latest.json.minisig  "s3://$BUCKET/tpk/core/"
```

`TPK_SIGNING_KEY` comes from the environment. `set -euo pipefail` matters: without
it a failed `verify` does not stop the upload.

Manifest last, always — see
[Server contract](./server-contract.md#publishing-order).

A complete GitHub Actions version of this, with the parent fetch, the watermark
gate, a post-upload smoke test through the CDN and a cold backup of the exact
bytes, is in
[`.github/workflows/content-release.yml`](https://github.com/ChasLui/tauri-plugin-tpk/blob/main/.github/workflows/content-release.yml).
It is a template: adapt `PACK_ID` and the frontend build step.

Two steps in it are worth copying even if you use something other than Actions.
The **parent fetch** downloads the published pack and checks it against the
digest in the published manifest, rather than rebuilding it — a rebuild that
differs by one byte produces a patch whose parent link no client can satisfy.
The **smoke test** fetches the manifest back through the CDN and compares its
watermark to what was just uploaded, because a stale edge serving the previous
manifest is the common failure and it is invisible unless you look.

## Rolling out gradually

```bash
tpk channel ... --rollout 10 --out latest.json   # 10%
tpk channel ... --rollout 50 --out latest.json   # then 50%
tpk channel ... --rollout 100 --out latest.json  # then everyone
```

Bucketing is deterministic per device and per release, so widening a rollout is
a superset. Watch `E_*` telemetry between steps; a release that is going to roll
back will do so within a few launches.
