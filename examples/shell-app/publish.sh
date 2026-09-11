#!/bin/sh
# Build a pack from src/ and a channel manifest, into ./cdn/stable/.
#
# Serve that directory over https and point `plugins.tpk.manifest_url` at it.
# See docs/local-testing.md.
#
#   TPK_SIGNING_KEY=$(cat signing.key) ./publish.sh [parent.tpk]

set -eu

: "${TPK_SIGNING_KEY:?set TPK_SIGNING_KEY (tpk keygen --out signing.key)}"

URL_BASE="${URL_BASE:-https://localhost:8443/stable/}"
OUT_DIR="cdn/stable"
VERSION="${VERSION:-1.0.0}"

# Monotonic, stateless, unaffected by moving the repo or rebuilding CI.
VERSION_CODE=$(date -u +%Y%m%d%H%M%S)
# Required rather than defaulted to now(): packing the same input twice must
# produce the same bytes, or the blacklist and the manifest digest mean nothing.
CREATED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)

mkdir -p "$OUT_DIR"
PACK="$OUT_DIR/core-$VERSION.tpk"

if [ $# -ge 1 ]; then
  tpk pack --kind patch --id core \
    --version "$VERSION" --version-code "$VERSION_CODE" --created-at "$CREATED_AT" \
    --parent "$1" --dist src/ --out "$PACK"
else
  tpk pack --kind base --id core \
    --version "$VERSION" --version-code "$VERSION_CODE" --created-at "$CREATED_AT" \
    --dist src/ --out "$PACK"
fi

tpk channel --channel stable --pack "$PACK" \
  --url-base "$URL_BASE" --watermark auto \
  --out "$OUT_DIR/latest.json"

# Gate before anything is served. `set -e` makes a non-zero exit stop here.
tpk verify --pubkey "${TPK_PUBKEY:?set TPK_PUBKEY}" \
  --file "$OUT_DIR/latest.json" --file "$PACK"

echo "published $PACK (version_code $VERSION_CODE)"
