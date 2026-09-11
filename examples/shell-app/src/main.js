// In a real app you would import the guest API:
//   import { check, download, notifyReady, status, onDownloadProgress }
//     from 'tauri-plugin-tpk-api';
// This example has no bundler, so it calls invoke directly.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const check = () => invoke("plugin:tpk|check");
const download = () => invoke("plugin:tpk|download");
const notifyReady = () => invoke("plugin:tpk|notify_ready");
const status = () => invoke("plugin:tpk|status");

const $ = (id) => document.getElementById(id);

function log(msg) {
  $("log").textContent += `${new Date().toISOString().slice(11, 19)}  ${msg}\n`;
}

function render(s) {
  $("revision").textContent = s.rev ?? "embedded";
  const layers = s.layers.map((l) => `${l.id}@${l.version_code}`).join(", ");
  $("status").textContent = [
    `pointer: ${s.pointer}`,
    `shell: ${s.shell}`,
    `watermark: ${s.watermark}`,
    s.pending ? "a revision is waiting for the next cold start" : null,
    s.degraded ? "DEGRADED — automatic updating has stopped" : null,
    layers ? `layers: ${layers}` : "layers: none (embedded assets)",
    s.failed_layers.length
      ? `failed layers: ${s.failed_layers.join(", ")}`
      : null,
    s.unsafe_capabilities.length
      ? `UNSAFE CAPABILITIES: ${s.unsafe_capabilities.join(", ")}`
      : null,
    s.has_embedded_fallback
      ? null
      : "NO EMBEDDED FALLBACK — a rollback has nowhere to land",
  ]
    .filter(Boolean)
    .join(" · ");
}

$("btn-check").addEventListener("click", async () => {
  const r = await check();
  log(`check -> ${JSON.stringify(r)}`);
  switch (r.status) {
    case "available":
      $("btn-download").disabled = false;
      log(`${r.packs.length} pack(s), ${r.bytes} bytes`);
      break;
    case "shell_required":
      log(
        `this build is too old; needs shell ${r.min_shell} — send the user to a store update`,
      );
      break;
    case "degraded":
      log(`degraded after ${r.consecutive_rollbacks} consecutive rollbacks`);
      break;
    default:
      break;
  }
});

$("btn-download").addEventListener("click", async () => {
  $("btn-download").disabled = true;
  $("progress").hidden = false;
  const r = await download();
  log(`download -> ${JSON.stringify(r)}`);
  if (r.status === "staged") {
    // There is deliberately no way to apply this to the running process: the
    // WebView has already imported modules from the current revision.
    $("status").textContent =
      "Update ready — it applies next time you open the app.";
  }
});

$("btn-status").addEventListener("click", async () => render(await status()));

await listen("tpk://download-progress", ({ payload }) => {
  const pct = payload.total ? (payload.downloaded / payload.total) * 100 : 0;
  $("progress-fill").style.width = `${pct}%`;
  $("progress-text").textContent =
    `pack ${payload.pack_index + 1}/${payload.pack_count} — ${Math.round(pct)}%`;
});

await listen("tpk://state", ({ payload }) =>
  log(`state -> ${JSON.stringify(payload)}`),
);
await listen("tpk://error", ({ payload }) =>
  log(`error ${payload.code}: ${payload.message}`),
);

// The first screen has rendered, so the acknowledgement means something.
// Calling this at the top of the file would commit a blank page.
render(await status());
log(`notifyReady -> ${JSON.stringify(await notifyReady())}`);
