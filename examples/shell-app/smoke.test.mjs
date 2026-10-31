import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import https from "node:https";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readFile, unlink, writeFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("./src/main.js", import.meta.url), "utf8");
const state = {
  pointer: "booting", rev: "r1", shell: "1.0.0", watermark: 1,
  pending: false, degraded: false, layers: [], failed_layers: [],
  unsafe_capabilities: [], has_embedded_fallback: true,
};

function frontend(invoke) {
  const frames = [];
  const elements = new Map();
  const document = {
    getElementById(id) {
      if (id === "btn-reset") return null;
      if (!elements.has(id)) elements.set(id, {
        textContent: "", disabled: false, hidden: true, style: {},
        handlers: {}, addEventListener(event, fn) { this.handlers[event] = fn; },
      });
      return elements.get(id);
    },
  };
  const startup = vm.runInNewContext(`(async () => {\n${source}\n})()`, {
    document,
    window: { __TAURI__: { core: { invoke }, event: { listen: async () => {} } } },
    requestAnimationFrame: (callback) => frames.push(callback),
  });
  return { elements, frames, startup };
}

async function paint(view) {
  await new Promise(setImmediate);
  assert.equal(view.frames.length, 1);
  view.frames.shift()();
  assert.equal(view.frames.length, 1);
  view.frames.shift()();
  await view.startup;
}

test("readiness waits for both render callbacks", async () => {
  let acknowledgements = 0;
  const view = frontend(async (command) => {
    if (command.endsWith("notify_ready")) { acknowledgements++; return { status: "committed", rev: "r1" }; }
    return state;
  });
  await new Promise(setImmediate);
  assert.equal(acknowledgements, 0);
  view.frames.shift()();
  assert.equal(acknowledgements, 0);
  view.frames.shift()();
  await view.startup;
  assert.equal(acknowledgements, 1);
});

test("rollback remains visible at startup and on status refresh", async () => {
  const view = frontend(async (command) => {
    if (command.endsWith("notify_ready")) return { status: "noop" };
    return { ...state, pointer: "committed", rolled_back: "r3" };
  });
  await paint(view);
  assert.match(view.elements.get("status").textContent, /rolled back: r3/);
  await view.elements.get("btn-status").handlers.click();
  assert.match(view.elements.get("status").textContent, /rolled back: r3/);
});

test("an inert plugin reports its error instead of leaving Loading", async () => {
  const view = frontend(async () => { throw { code: "E_DISABLED", message: "Plugin unavailable" }; });
  await view.startup;
  assert.match(view.elements.get("status").textContent, /E_DISABLED.*Plugin unavailable/);
  assert.equal(view.frames.length, 0);
  await view.elements.get("btn-status").handlers.click();
  assert.match(view.elements.get("log").textContent, /E_DISABLED/);
});

test("a rejected check restores controls and does not retain a stale download", async () => {
  const view = frontend(async (command) => {
    if (command.endsWith("check")) throw { code: "E_NETWORK", message: "Offline" };
    if (command.endsWith("notify_ready")) return { status: "noop" };
    return state;
  });
  await paint(view);
  view.elements.get("btn-download").disabled = false;
  await view.elements.get("btn-check").handlers.click();
  assert.equal(view.elements.get("btn-check").disabled, false);
  assert.equal(view.elements.get("btn-download").disabled, true);
  assert.match(view.elements.get("status").textContent, /Offline/);
});

test("failed outcomes and rejected downloads both allow retry", async () => {
  let reject = false;
  const view = frontend(async (command) => {
    if (command.endsWith("download")) {
      if (reject) throw { code: "E_IO", message: "Cache purged" };
      return { status: "failed", code: "E_NETWORK", message: "Offline" };
    }
    if (command.endsWith("notify_ready")) return { status: "noop" };
    return state;
  });
  await paint(view);
  await view.elements.get("btn-download").handlers.click();
  assert.equal(view.elements.get("btn-download").disabled, false);
  assert.equal(view.elements.get("progress").hidden, true);
  assert.match(view.elements.get("status").textContent, /Offline/);
  reject = true;
  await view.elements.get("btn-download").handlers.click();
  assert.equal(view.elements.get("btn-download").disabled, false);
  assert.match(view.elements.get("status").textContent, /Cache purged/);
});

test("prepared fixtures serve verified HTTPS channels and resumable packs", {
  skip: !process.env.TPK_SMOKE_SESSION,
  timeout: 30000,
}, async () => {
  const dir = path.resolve(process.env.TPK_SMOKE_SESSION);
  const session = JSON.parse(await readFile(path.join(dir, "session.json"), "utf8"));
  const script = fileURLToPath(new URL("./smoke.mjs", import.meta.url));
  const ca = await readFile(path.join(dir, "ca.crt"));
  const child = spawn(process.execPath, [script, "serve", "--dir", dir], { stdio: ["ignore", "pipe", "pipe"] });
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const exit = once(child, "exit");
  try {
    await new Promise((resolve, reject) => {
      child.stdout.on("data", (chunk) => { if (chunk.toString().includes("Serving ")) resolve(); });
      child.once("exit", (code) => reject(new Error(`Server stopped (${code}): ${stderr}`)));
      child.once("error", reject);
    });
    function get(resource, headers = {}, method = "GET") {
      return new Promise((resolve, reject) => {
        // Process-scoped CA only: this test never changes system certificate trust.
        const request = https.request({ hostname: "127.0.0.1", servername: "localhost", port: session.port, path: resource, ca, headers, method }, (response) => {
          const chunks = [];
          response.on("data", (chunk) => chunks.push(chunk));
          response.on("end", () => resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks) }));
          response.on("error", reject);
        });
        request.on("error", reject);
        request.end();
      });
    }
    const before = JSON.parse(await readFile(path.join(dir, "active.json"), "utf8"));
    const orphan = path.join(dir, "cdn/stable", `channel-${before.watermark + 1}.json`);
    await writeFile(orphan, "interrupted immutable publish", { flag: "wx" });
    const lockPath = path.join(dir, ".publish.lock");
    await writeFile(lockPath, "test publisher", { flag: "wx" });
    try {
      assert.throws(() => execFileSync(process.execPath, [script, "publish", "--dir", dir, "--scenario", "base"], { stdio: "pipe" }), /A publisher is active/);
      assert.deepEqual(JSON.parse(await readFile(path.join(dir, "active.json"), "utf8")), before);
    } finally { await unlink(lockPath); }
    let watermark = before.watermark;
    for (const scenario of ["base", "patch", "broken", "base"]) {
      execFileSync(process.execPath, [script, "publish", "--dir", dir, "--scenario", scenario, "--tpk", session.tpk], { stdio: "pipe" });
      const manifest = await get("/stable/latest.json");
      const signature = await get("/stable/latest.json.minisig");
      assert.equal(manifest.status, 200);
      assert.equal(signature.status, 200);
      const document = JSON.parse(manifest.body);
      assert.ok(document.watermark > watermark);
      watermark = document.watermark;
      const active = JSON.parse(await readFile(path.join(dir, "active.json"), "utf8"));
      assert.deepEqual(manifest.body, await readFile(path.join(dir, "cdn/stable", active.manifest)));
      assert.deepEqual(signature.body, await readFile(path.join(dir, "cdn/stable", `${active.manifest}.minisig`)));
      assert.deepEqual(document.packs.map((p) => p.kind), scenario === "patch" ? ["base", "patch"] : ["base"]);
    }
    assert.equal(await readFile(orphan, "utf8"), "interrupted immutable publish");
    const file = session.packs.base;
    const expected = await readFile(path.join(dir, "cdn/stable", file));
    const resumed = await get(`/stable/${file}`, { Range: "bytes=10-" });
    assert.equal(resumed.status, 206);
    assert.equal(resumed.headers["content-range"], `bytes 10-${expected.length - 1}/${expected.length}`);
    assert.deepEqual(resumed.body, expected.subarray(10));
    const head = await get(`/stable/${file}`, {}, "HEAD");
    assert.equal(Number(head.headers["content-length"]), expected.length);
    assert.equal(head.body.length, 0);
    assert.equal((await get(`/stable/${file}`, { Range: `bytes=${expected.length}-` })).status, 416);
    assert.equal((await get("/stable/session.json")).status, 404);
    assert.equal((await get("/stable/../session.json")).status, 404);
    assert.equal((await get("/stable/server.key")).status, 404);
    assert.equal((await get("/__stop", {}, "POST")).status, 403);
    execFileSync(process.execPath, [script, "stop", "--dir", dir], { stdio: "pipe" });
    const [code] = await exit;
    assert.equal(code, 0);
  } finally {
    child.kill("SIGTERM");
    await exit;
  }
});
