// In a real app you would import the guest API:
//   import { check, download, notifyReady, status, onDownloadProgress }
//     from 'tauri-plugin-tpk-api';
// This example has no bundler, so it calls invoke directly.

// Smoke fixtures replace this constant only in the packed copy.
const AUTO_READY = true;

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

function report(error) {
  const message = `${error?.code ? `${error.code}: ` : ""}${error?.message ?? String(error)}`;
  log(message);
  $("status").textContent = message;
}

function firstPaint() {
  return new Promise((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(resolve));
  });
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
    s.rolled_back ? `rolled back: ${s.rolled_back}` : null,
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
  $("btn-check").disabled = true;
  $("btn-download").disabled = true;
  try {
    const r = await check();
    log(`check -> ${JSON.stringify(r)}`);
    if (r.status === "available") {
      $("btn-download").disabled = false;
      log(`${r.packs.length} pack(s), ${r.bytes} bytes`);
    } else if (r.status === "shell_required") {
      $("status").textContent = `This update needs shell ${r.min_shell}.`;
    } else if (r.status === "degraded") {
      $("status").textContent = `Updating stopped after ${r.consecutive_rollbacks} rollbacks.`;
    } else {
      $("status").textContent = `check: ${r.status}`;
    }
  } catch (error) {
    report(error);
  } finally {
    $("btn-check").disabled = false;
  }
});

$("btn-download").addEventListener("click", async () => {
  $("btn-download").disabled = true;
  $("progress").hidden = false;
  try {
    const r = await download();
    log(`download -> ${JSON.stringify(r)}`);
    if (r.status === "staged") {
      // The running WebView keeps its current revision until cold start.
      $("status").textContent = "Update ready — it applies next time you open the app.";
    } else if (r.status === "failed") {
      report(r);
      $("btn-download").disabled = false;
    } else {
      $("status").textContent = `download: ${r.status}`;
    }
  } catch (error) {
    report(error);
    $("btn-download").disabled = false;
  } finally {
    $("progress").hidden = true;
  }
});

$("btn-status").addEventListener("click", async () => {
  try {
    render(await status());
  } catch (error) {
    report(error);
  }
});

// Only smoke fixtures include this control and grant tpk:allow-reset.
$("btn-reset")?.addEventListener("click", async () => {
  try {
    const result = await invoke("plugin:tpk|reset", { options: { clear_blacklist: false } });
    log(`reset -> ${JSON.stringify(result)}`);
    $("btn-download").disabled = true;
    $("status").textContent = "Reset saved — this window keeps its revision until cold start.";
  } catch (error) {
    report(error);
  }
});

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

try {
  render(await status());
  // DOM assignment is not proof of a rendered frame. Keep the trial alive
  // until the first screen has had an opportunity to paint.
  await firstPaint();
  if (AUTO_READY) {
    log(`notifyReady -> ${JSON.stringify(await notifyReady())}`);
    render(await status());
  } else {
    log("Smoke rollback fixture: this revision deliberately does not acknowledge readiness.");
  }
} catch (error) {
  report(error);
}
