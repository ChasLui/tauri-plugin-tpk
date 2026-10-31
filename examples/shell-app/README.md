# shell-app

A minimal Tauri app wired to `tauri-plugin-tpk`. It is a standalone crate with
its own lockfile, deliberately **not** a workspace member — `--workspace` never
covers it, so CI checks it separately.

```bash
pnpm install
pnpm tauri dev
```

`src/` doubles as the app's embedded frontend and as the content you pack, which
is why it has no inline `<script>`: `tpk pack` rejects HTML containing inline or
remote script, so the same directory can be shipped either way unchanged.

## Prepared smoke fixtures

From the repository root:

```sh
node examples/shell-app/smoke.mjs prepare \
  --tpk /path/to/tpk \
  --dir examples/shell-app/test-server/new-session
```

It creates a unique test app configuration and verified base, delta patch,
tombstone and no-ready rollback fixtures. Public artifacts are ignored; private keys stay
outside Git in a private OS temporary directory. It never installs certificate
trust or launches an application.

Follow the printed build/serve commands and the complete
[desktop smoke sequence](../../docs/local-testing.md#prepared-desktop-smoke-session).
The generated configuration alone grants the reset capability and disables the
resolver cache; the normal example permissions stay unchanged.

Run frontend tests from the repository root with
`node --test examples/shell-app/smoke.test.mjs`. Set `TPK_SMOKE_SESSION` to a
prepared session to include the real HTTPS/Range fixture test.

## Isolated iOS simulator build

On an Apple Silicon Mac with Xcode, run from the repository root:

```sh
sh examples/shell-app/ios-simulator.sh check
sh examples/shell-app/ios-simulator.sh init
sh examples/shell-app/ios-simulator.sh build
```

The script uses Rust 1.88 in `target/ios-rustup` and artifacts in
`target/ios-smoke`. `check` validates Rust compilation; `build` produces an
unsigned simulator app after initialization. Missing native initialization tools
are reported before Tauri can install them globally. See the
[toolchain setup and validation boundaries](../../docs/local-testing.md#isolated-apple-silicon-simulator-build).

## Serving a channel locally

```bash
key_dir=$(mktemp -d)                       # outside the repository
chmod 700 "$key_dir"
tpk keygen --out "$key_dir/signing.key"      # prints the public key
export TPK_SIGNING_KEY=$(cat "$key_dir/signing.key")
export TPK_PUBKEY=RWT...                   # paste the printed key

./publish.sh                               # -> cdn/stable/

mkcert -install && mkcert localhost
npx http-server ./cdn -S -C localhost.pem -K localhost-key.pem -p 8443
```

Put the public key into `plugins.tpk.pubkeys` in `src-tauri/tauri.conf.json`,
then run the app and press **Check**. `manifest_url` must be https — the plugin
rejects anything else.

A second run with a parent builds a patch:

```bash
./publish.sh cdn/stable/core-1.0.0.tpk
```

## What to notice

- **Download applies on the next cold start.** There is no apply button, on
  purpose — see [the philosophy](../../docs/philosophy.md).
- **`notifyReady()` waits for two animation-frame callbacks after rendering**,
  rather than assuming a DOM assignment has painted. The broken smoke fixture
  deliberately skips it to exercise rollback.
- **IPC and download failures stay visible and retryable.** An inert plugin
  reports its error instead of leaving the status at Loading.
- **The rolled-back revision is shown in status.** Startup rollback emits no
  frontend event, so the status field is how the GUI confirms it happened.
- **Deleted assets return an empty 404.** Rust startup explicitly builds the
  saved window configurations with the TPK response guard, preserving their
  labels, URLs, titles and sizes while retaining normal SPA fallback for misses.
- **`unsafe_capabilities` is empty.** `capabilities/default.json` grants only
  `core:default` and `tpk:default`. Add `shell:allow-execute` and watch the
  status line change — that is what the warning is for.

For the rollback path and mobile testing, see
[docs/local-testing.md](../../docs/local-testing.md).
