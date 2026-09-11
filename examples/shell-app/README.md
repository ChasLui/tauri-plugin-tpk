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

## Serving a channel locally

```bash
tpk keygen --out tpk-secret.key            # prints the public key
export TPK_SECRET_KEY=$(cat tpk-secret.key)
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
- **`notifyReady()` is called after the first render**, not at the top of
  `main.js`. Moving it up would commit a blank page.
- **`unsafe_capabilities` is empty.** `capabilities/default.json` grants only
  `core:default` and `tpk:default`. Add `shell:allow-execute` and watch the
  status line change — that is what the warning is for.

For the rollback path and mobile testing, see
[docs/local-testing.md](../../docs/local-testing.md).
