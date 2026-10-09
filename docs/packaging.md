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
ZIP entries, a fixed zstd level, and a fixed ZIP host system. You get it for
free as long as you pin this one flag.

That last one is not theoretical. The `zip` crate stamps the *building* host
into each central directory header's "version made by" — 3 on Unix, 0 on
Windows — so before it was pinned, the same input packed on Windows and on
Linux produced files that differed by one byte per entry and therefore by
SHA-256. Verified byte-identical across macOS and Windows 11.

### Other flags

| Flag | Meaning |
|---|---|
| `--kind` | `base` / `patch` / `dlc` / `mod` |
| `--min-shell` | Lowest shell version this pack supports |
| `--max-shell` | Highest, inclusive. Leave unset unless a specific break is known: every newer shell skips the pack |
| `--channel` | Channel this pack is built for |
| `--parent` | Parent `.tpk`, for `patch`. Repeat for a chain, base first |
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

The parent is read and compared against `--dist`. A changed file of at least
`--delta-threshold` bytes becomes a bsdiff `delta` when that is smaller, and a
`full` otherwise; every other file, including ones identical to the parent, is
written as `full`. Files gone from `--dist` become `delete` tombstones.

Once patches are published, the next patch stacks on the latest one. Pass the
whole chain, lowest first — the base, then each patch in order:

```bash
tpk pack --kind patch ... \
  --parent core-2.1.0.tpk \
  --parent core-2.1.1.tpk \
  --dist dist/ --out core-2.1.2.tpk
```

The chain is resolved in memory, deltas included, and the new pack is diffed
against the result. The first `--parent` must be a base, and each patch after it
must link to the pack before it (same id, `parent.version_code` and
`parent.manifest_sha256`), or `tpk pack` exits `2`. The new pack's parent is the
last `--parent`.

Parents are not signature-checked. Pass only packs you published yourself or
have run `tpk verify` on. As a backstop, any entry declaring more than 64 MiB,
decoded or stored (the desktop runtime's asset limit), is refused before it is
read. The whole chain may decode at most 1 GiB in total, counting every full
file, delta stream and rebuilt file, including ones a later patch replaces;
decoding stops where that budget runs out and `tpk pack` exits `2`. Every patch
carries the whole dist, so the cost grows as dist size × chain length — roughly
16 links of a 64 MiB dist. Collapse a long chain by publishing a new base.

The parent link must match **exactly** at install time. A patch built against
`version_code: 20260911120000` applies only on top of that version. The client
does not work around a missing parent: a device that is fresh or missed a patch
can only catch up if the channel still lists the base and every patch between.
That is why the channel keeps the whole chain, not just the newest pack.

Keep every published `.tpk` as a build artifact. You cannot produce a patch
without its parent.

## HTML rules

`tpk pack` rejects HTML containing inline or remote `<script>`.

The one inline body allowed is a JSON data block: the first `type` attribute,
trimmed and compared case-insensitively, is exactly `application/json` or
`application/ld+json`. Browsers never execute those. Everything else is still
rejected — no `type`, `module`, `importmap`, `speculationrules`, and JSON with
MIME parameters such as `application/json; charset=utf-8`. A data block with a
remote `src` is rejected like any other remote script.

Inline script is rejected because the CSP compiled into the binary carries
hashes computed from the *compile-time* HTML; overlay HTML with different inline
script would be blocked by a hash nobody can update without a shell release.
Remote script is rejected because it is an unsigned code path straight past
everything this format does.

## Worker rules

The same "nothing executable from outside the pack" rule reaches packed `.js`
and `.mjs`. `tpk pack` scans them for `new Worker(…)`, `new SharedWorker(…)`,
`<expr>.serviceWorker.register(…)` and the bundler form
`new Worker(new URL("…", import.meta.url))`, and rejects the pack when the first
argument is a string literal that is non-local by the same rule the HTML check
uses.

The scan also rejects non-local literal sources in `importScripts(…)`, dynamic
`import(…)`, static module imports and `export … from` re-exports.

An argument no static check can judge — a variable, a concatenation, a template
with a substitution — cannot be decided either way, so it is counted and
reported in one warning line and does not fail the build. Audit those call sites
yourself.

This is a lint, not a sandbox. Escapes inside the literal are not decoded, a
regex literal can desynchronise the token walk, and a runtime-computed URL is
invisible to it by construction. What it catches is the shape a bundler actually
emits.

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

Repeat `--pack` per pack. The manifest lists exactly the packs given, so pass
everything the channel should keep advertising: packs of other ids, and the base
plus every patch of a chain. Sizes and digests are computed from the files, so
the manifest cannot drift from what you are about to upload. Each URL is
`--url-base` plus the file name, so keep published files under their original
names. `--watermark auto` uses UTC `YYYYMMDDHHMM`.

Each entry also carries the pack's `min_shell` / `max_shell`, copied from its
signed manifest, so a client can skip a pack its shell is outside of without
downloading it. The channel-level `min_shell` defaults to the highest
`min_shell` among the packs; override it with `--min-shell`.

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

# The parents are the published chain; fetch them, do not rebuild them.
# Here core-2.1.0.tpk is the base and core-2.1.1.tpk the patch on it.
curl -fsSL "$CDN/core/core-2.1.0.tpk" -o core-2.1.0.tpk
curl -fsSL "$CDN/core/core-2.1.1.tpk" -o core-2.1.1.tpk

tpk pack --kind patch --id core \
  --version "$VERSION" --version-code "$VERSION_CODE" --created-at "$CREATED_AT" \
  --parent core-2.1.0.tpk --parent core-2.1.1.tpk \
  --dist dist/ --out "core-$VERSION.tpk"

# The channel keeps the whole chain, so fresh devices can still reach it.
tpk channel --channel stable \
  --pack core-2.1.0.tpk --pack core-2.1.1.tpk --pack "core-$VERSION.tpk" \
  --url-base "$CDN/core/" --watermark auto --out latest.json

# Gate before anything is uploaded.
tpk verify --pubkey "$TPK_PUBKEY" --file latest.json --file "core-$VERSION.tpk"

# Nothing may require a shell newer than the one users already run: a channel
# min_shell above it answers shell_required for everyone, a pack min_shell above
# it makes plan skip that pack. Either way the content never arrives.
jq -r '[.min_shell] + [.packs[].min_shell] | map(select(. != null))[]' \
  latest.json |
while read -r min; do
  [ "$(printf '%s\n%s\n' "$min" "$RELEASED_SHELL" | sort -V | tail -n1)" \
    = "$RELEASED_SHELL" ] || {
    echo "min_shell $min is above the released shell $RELEASED_SHELL"
    exit 1
  }
done

# Check every immutable object at the origin before writing anything.
watermark=$(jq -er '.watermark' latest.json)
for key in "core-$VERSION.tpk" "$watermark.json" "$watermark.json.minisig"; do
  if error=$(aws s3api head-object --bucket "$BUCKET" \
    --key "tpk/core/$key" 2>&1); then
    echo "$key is already published"
    exit 1
  elif [[ "$error" != *"(404)"* && "$error" != *"(NotFound)"* ]]; then
    echo "cannot check $key: $error"
    exit 1
  fi
done

# Packs first, then the immutable copy, then latest.json.
aws s3 cp "core-$VERSION.tpk" "s3://$BUCKET/tpk/core/"
aws s3 cp latest.json         "s3://$BUCKET/tpk/core/$watermark.json"
aws s3 cp latest.json.minisig "s3://$BUCKET/tpk/core/$watermark.json.minisig"

aws s3 cp latest.json          "s3://$BUCKET/tpk/core/"
aws s3 cp latest.json.minisig  "s3://$BUCKET/tpk/core/"
```

`TPK_SIGNING_KEY` comes from the environment. `set -euo pipefail` matters: without
it a failed `verify` does not stop the upload.

The upload role needs `s3:GetObject` for the channel objects and `s3:ListBucket`
for its prefix, so `head-object` can distinguish a missing object (404) from
access denied (403). Permission and transport failures stop publication.

Manifest last, always — see
[Server contract](./server-contract.md#publishing-order).

The `$watermark.json` copy is byte-identical to `latest.json`, so the same
detached signature covers it; upload it with the packs' long immutable
cache-control, not `latest.json`'s short one. It goes up **before**
`latest.json`, so a device that sees the new manifest can always fetch the
pinned copy of exactly what it saw.

`sort -V` orders plain `x.y.z` the way SemVer does, so keep `RELEASED_SHELL` a
released `MAJOR.MINOR.PATCH` version. The workflow rejects anything else in its
input validation, which is what makes the comparison exact: the only place
`sort -V` disagrees with SemVer is a prerelease, which it sorts *above* its own
release.

A complete GitHub Actions version of this, with the channel fetch, the watermark
gate, a post-upload smoke test through the CDN and a cold backup of the exact
bytes, is in
[`.github/workflows/content-release.yml`](https://github.com/ChasLui/tauri-plugin-tpk/blob/main/.github/workflows/content-release.yml).
It is a template: adapt `PACK_ID` and the frontend build step. Its
`released_shell` input drives the `min_shell` assertion above; leave it empty if
you do not track the shell version your users are on, and the assertion is
skipped rather than blocking the publish.

Three steps in it are worth copying even if you use something other than Actions.
The **channel fetch** downloads the packs the new manifest should keep, checks
each against its published digest and signature, and passes them back to
`tpk channel`: every other id, plus `PACK_ID`'s newest base and the patches on
it. For a patch that chain is also the repeated `--parent`; anything older than
that base is dropped. A base at 100% drops `PACK_ID`'s old chain entirely. The
packs are fetched rather than rebuilt — a rebuild that differs by one byte
produces a patch whose parent link no client can satisfy.
The **smoke test** fetches the manifest back through the CDN and compares its
watermark to what was just uploaded, because a stale edge serving the previous
manifest is the common failure and it is invisible unless you look. It also
fetches `<watermark>.json` back and diffs it against the manifest just
uploaded — a pinned copy that is not the same bytes is worse than none, since a
rollback or an audit replays it.
The **`min_shell` assertion** runs after `tpk channel` and before any upload,
so a manifest no shell in the field can accept is caught while it is still only
a file in the job.

## Rolling out gradually

```bash
tpk channel ... --rollout 10 --out latest.json   # 10%
tpk channel ... --rollout 50 --out latest.json   # then 50%
tpk channel ... --rollout 100 --out latest.json  # then everyone
```

`--rollout` applies only to the newest pack (highest `version_code`) of each id.
The base and older patches of that id stay at 100, so a 10% patch does not also
hold back the base a fresh device needs.

A new base below 100% keeps the old chain in the channel. Devices outside the
bucket, fresh installs included, still converge on the old base and its
patches; devices inside take the new base, which replaces the old layers of that
id. A fresh install inside the bucket downloads only the new base: the old base
and the patches on it are skipped as superseded. Once the base is at 100%, or once a patch is published on top of it, the
old chain is dropped and the new base goes to 100% for everyone.

Bucketing is deterministic per device and per release, so widening a rollout is
a superset. Watch `E_*` telemetry between steps; a release that is going to roll
back will do so within a few launches.
