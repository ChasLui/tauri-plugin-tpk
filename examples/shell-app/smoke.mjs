#!/usr/bin/env node
// Disposable, localhost-only OTA fixtures. Never installs trust or launches apps.
import { execFileSync } from "node:child_process";
import { randomBytes, timingSafeEqual, X509Certificate } from "node:crypto";
import { createReadStream } from "node:fs";
import { chmod, copyFile, mkdir, mkdtemp, open, readFile, realpath, rename, stat, unlink, writeFile } from "node:fs/promises";
import https from "node:https";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const example = path.dirname(fileURLToPath(import.meta.url));
const scenarios = ["base", "patch", "broken"];
const shellQuote = (value) => `'${String(value).replaceAll("'", "'\\''")}'`;

function options(args) {
  const result = {};
  for (let i = 0; i < args.length; i += 2) {
    if (!["--dir", "--tpk", "--port", "--scenario", "--private-root"].includes(args[i]) || !args[i + 1]) {
      throw new Error(`Unknown or incomplete option: ${args[i]}`);
    }
    if (Object.hasOwn(result, args[i])) throw new Error(`Repeated option: ${args[i]}`);
    result[args[i]] = args[i + 1];
  }
  return result;
}

function run(binary, args, env = process.env) {
  try {
    return execFileSync(binary, args, { env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  } catch (error) {
    // Signing material travels only in the child's environment, never argv/logs.
    throw new Error(`${path.basename(binary)} ${args[0]} failed: ${error.stderr?.toString() ?? error.message}`);
  }
}

async function load(dir) {
  return JSON.parse(await readFile(path.join(dir, "session.json"), "utf8"));
}

async function publish(dir, scenario, executable) {
  if (!scenarios.includes(scenario)) throw new Error(`Scenario must be ${scenarios.join(", ")}`);
  const lockPath = path.join(dir, ".publish.lock");
  const lock = await open(lockPath, "wx").catch((error) => {
    if (error.code === "EEXIST") throw new Error("A publisher is active, or an interrupted publish left .publish.lock; inspect it before retrying.");
    throw error;
  });
  try {
    await lock.writeFile(JSON.stringify({ pid: process.pid, started_at: new Date().toISOString() }));
    const session = await load(dir);
    const tpk = executable ? path.resolve(executable) : session.tpk;
    const active = JSON.parse(await readFile(path.join(dir, "active.json"), "utf8"));
    let watermark = active.watermark + 1;
    const root = path.join(dir, "cdn", "stable");
    // Interrupted publishes can leave unreferenced numbered manifests. Keep
    // them immutable and allocate a fresh number under the exclusive lock.
    for (;;) {
      let occupied = false;
      for (const suffix of [".json", ".json.minisig"]) {
        try { await stat(path.join(root, `channel-${watermark}${suffix}`)); occupied = true; }
        catch (error) { if (error.code !== "ENOENT") throw error; }
      }
      if (!occupied) break;
      watermark++;
    }
    const manifest = `channel-${watermark}.json`;
    const env = { ...process.env, TPK_SIGNING_KEY: await readFile(path.join(session.secretsDir, "signing.key"), "utf8") };
    const args = ["channel", "--channel", "stable", "--url-base", `${session.origin}/stable/`, "--watermark", String(watermark), "--out", path.join(root, manifest)];
    for (const pack of scenario === "patch" ? ["base", "patch"] : [scenario]) {
      args.push("--pack", path.join(root, session.packs[pack]));
    }
    run(tpk, args, env);
    run(tpk, ["verify", "--pubkey", session.publicKey, "--file", path.join(root, manifest)]);
    // Packs and numbered manifests are immutable; only this public pointer moves.
    const tmp = path.join(dir, `active.${randomBytes(8).toString("hex")}.tmp`);
    await writeFile(tmp, JSON.stringify({ scenario, watermark, manifest }), { flag: "wx" });
    await rename(tmp, path.join(dir, "active.json"));
    console.log(`Published ${scenario}, watermark ${watermark}.`);
  } finally {
    await lock.close();
    await unlink(lockPath);
  }
}

async function prepare(opts) {
  const port = Number(opts["--port"] ?? 8443);
  if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error("Port must be 1024–65535.");
  const id = `s${randomBytes(6).toString("hex")}`;
  const dir = path.resolve(opts["--dir"] ?? path.join(example, "test-server", id));
  const tpk = opts["--tpk"] ? path.resolve(opts["--tpk"]) : "tpk";
  await mkdir(path.dirname(dir), { recursive: true });
  await mkdir(dir); // Never reuse or overwrite an existing session.
  const privateRoot = await realpath(opts["--private-root"] ?? os.tmpdir());
  for (let cursor = privateRoot; ; cursor = path.dirname(cursor)) {
    let gitEntry;
    try { gitEntry = await stat(path.join(cursor, ".git")); }
    catch (error) { if (error.code !== "ENOENT") throw error; }
    if (gitEntry) throw new Error("Private keys must stay outside Git worktrees; choose an OS temporary directory.");
    if (path.dirname(cursor) === cursor) break;
  }
  const secretsDir = await mkdtemp(path.join(privateRoot, "tpk-smoke-private-"));
  await chmod(secretsDir, 0o700);
  const signing = run(tpk, ["keygen", "--out", path.join(secretsDir, "signing.key")]);
  const publicKey = JSON.parse(signing.trim()).key;
  const env = { ...process.env, TPK_SIGNING_KEY: await readFile(path.join(secretsDir, "signing.key"), "utf8") };
  const origin = `https://localhost:${port}`;
  const ca = path.join(dir, "ca.crt");
  run("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", `/CN=TPK Smoke ${id}`, "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign", "-keyout", path.join(secretsDir, "ca.key"), "-out", ca]);
  const extensions = path.join(secretsDir, "server.ext");
  await writeFile(extensions, "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost\n");
  run("openssl", ["req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost", "-keyout", path.join(secretsDir, "server.key"), "-out", path.join(secretsDir, "server.csr")]);
  run("openssl", ["x509", "-req", "-in", path.join(secretsDir, "server.csr"), "-CA", ca, "-CAkey", path.join(secretsDir, "ca.key"), "-CAcreateserial", "-CAserial", path.join(secretsDir, "ca.srl"), "-days", "2", "-extfile", extensions, "-out", path.join(dir, "server.crt")]);
  for (const key of ["ca.key", "server.key"]) await chmod(path.join(secretsDir, key), 0o600);

  const root = path.join(dir, "cdn", "stable");
  await mkdir(root, { recursive: true });
  const payload = randomBytes(32768).toString("hex");
  const packs = {};
  const created = new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
  for (const [index, scenario] of ["embedded", ...scenarios].entries()) {
    const dist = path.join(dir, scenario);
    await mkdir(dist);
    for (const name of ["index.html", "main.js", "style.css"]) {
      await copyFile(path.join(example, "src", name), path.join(dist, name));
    }
    let html = await readFile(path.join(dist, "index.html"), "utf8");
    html = html.replace("<h1>TPK Example</h1>", `<h1>TPK Smoke ${scenario} ${index}</h1>`)
      .replace('<div class="actions">', '<div class="actions">\n    <button id="btn-reset">Reset for next launch</button>\n    <button id="btn-probe">Read smoke resource</button>\n    <button id="btn-tombstone">Check tombstone</button>');
    await writeFile(path.join(dist, "index.html"), html);
    let js = await readFile(path.join(dist, "main.js"), "utf8");
    if (!js.includes("const AUTO_READY = true;")) throw new Error("Example readiness marker is missing.");
    if (scenario === "broken") js = js.replace("const AUTO_READY = true;", "const AUTO_READY = false;");
    js += `\nfor (const [button, resource] of [["btn-probe", "smoke-data.txt"], ["btn-tombstone", "removed.txt"]]) {\n  document.getElementById(button).addEventListener("click", async () => {\n    try {\n      const response = await fetch(resource, { cache: "no-store" });\n      log(resource + " -> " + response.status + ": " + (await response.text()).slice(0, 80));\n    } catch (error) { report(error); }\n  });\n}\n`;
    await writeFile(path.join(dist, "main.js"), js);
    await writeFile(path.join(dist, "smoke-data.txt"), `${scenario.toUpperCase()} DATA\n${payload}\n`);
    if (scenario !== "patch") await writeFile(path.join(dist, "removed.txt"), `${scenario.toUpperCase()} OLD RESOURCE\n`);
    if (scenario === "embedded") continue;
    const filename = `core-${scenario}.tpk`;
    packs[scenario] = filename;
    const args = ["pack", "--kind", scenario === "patch" ? "patch" : "base", "--id", "core", "--version", `1.0.${index}`, "--version-code", String(index), "--created-at", created, "--channel", "stable", "--dist", dist, "--out", path.join(root, filename)];
    if (scenario === "patch") args.push("--parent", path.join(root, packs.base), "--delta-threshold", "1");
    run(tpk, args, env);
    run(tpk, ["verify", "--pubkey", publicKey, "--file", path.join(root, filename)]);
  }
  const patch = JSON.parse(run(tpk, ["inspect", path.join(root, packs.patch), "--json"]));
  if (!patch.entries.some((entry) => entry.op === "delta") || !patch.entries.some((entry) => entry.op === "delete")) {
    throw new Error("Smoke patch must contain both a delta and a tombstone.");
  }
  const identifier = `com.example.tpk.smoke.${id}`;
  const config = path.join(dir, "tauri.smoke.json");
  await writeFile(config, JSON.stringify({
    productName: `TPK Smoke ${id}`,
    identifier,
    build: { frontendDist: path.join(dir, "embedded") },
    app: { windows: [{ label: "main", title: `TPK Smoke ${id}`, width: 800, height: 600 }], security: { capabilities: [{ identifier: `smoke-${id}`, windows: ["main"], permissions: ["core:default", "tpk:default", "tpk:allow-reset"] }] } },
    plugins: { tpk: { manifest_url: `${origin}/{{channel}}/latest.json`, pubkeys: [{ key: publicKey, epoch: 1 }], channel: "stable", auto_check_on_launch: false, auto_download: false, cache_budget_bytes: 0 } },
  }, null, 2));
  const session = { identifier, tpk, origin, port, publicKey, packs, secretsDir, config };
  await writeFile(path.join(dir, "session.json"), JSON.stringify(session, null, 2));
  await writeFile(path.join(dir, "active.json"), JSON.stringify({ watermark: 0 }));
  await publish(dir, "base");
  const fingerprint = new X509Certificate(await readFile(ca)).fingerprint256;
  console.log(`Session: ${dir}\nBundle ID: ${identifier}\nCA SHA-256: ${fingerprint}`);
  console.log(`Build: pnpm -C examples/shell-app tauri build --debug --bundles app --config ${shellQuote(config)}`);
  console.log(`Serve: node examples/shell-app/smoke.mjs serve --dir ${shellQuote(dir)}`);
  console.log(`Stop: node examples/shell-app/smoke.mjs stop --dir ${shellQuote(dir)}`);
  console.log(`Publish: node examples/shell-app/smoke.mjs publish --dir ${shellQuote(dir)} --scenario patch`);
  if (process.platform === "darwin") {
    console.log(`Trust (requires explicit approval): security add-trusted-cert -r trustRoot -p ssl -s localhost -k ${shellQuote(path.join(os.homedir(), "Library/Keychains/login.keychain-db"))} ${shellQuote(ca)}`);
    console.log(`Remove trust after testing: security remove-trusted-cert ${shellQuote(ca)}`);
  }
  console.log("No certificate trust, application data, or GUI has been changed. Private keys remain in a private temporary directory outside the repository.");
}

function validToken(token) {
  if (!/^[0-9a-f]{64}$/.test(token)) throw new Error("Invalid smoke server control token.");
  return token;
}

async function controlToken(session) {
  const file = path.join(session.secretsDir, "control.token");
  try { return validToken((await readFile(file, "utf8")).trim()); }
  catch (error) { if (error.code !== "ENOENT") throw error; }
  const token = randomBytes(32).toString("hex");
  try { await writeFile(file, token, { flag: "wx", mode: 0o600 }); return token; }
  catch (error) { if (error.code !== "EEXIST") throw error; return validToken((await readFile(file, "utf8")).trim()); }
}

async function stop(dir) {
  const session = await load(dir);
  const token = validToken((await readFile(path.join(session.secretsDir, "control.token"), "utf8")).trim());
  const ca = await readFile(path.join(dir, "ca.crt"));
  await new Promise((resolve, reject) => {
    const request = https.request({
      hostname: "127.0.0.1", servername: "localhost", port: session.port,
      path: "/__stop", method: "POST", ca, timeout: 5000,
      headers: { Authorization: `Bearer ${token}` },
    }, (response) => {
      response.resume();
      response.once("end", () => response.statusCode === 204 ? resolve() : reject(new Error(`Stop refused (${response.statusCode}).`)));
    });
    request.once("error", reject);
    request.once("timeout", () => request.destroy(new Error("Stop request timed out.")));
    request.end();
  });
  console.log("Smoke server stopped.");
}

async function serve(dir) {
  const session = await load(dir);
  const token = await controlToken(session);
  const root = path.join(dir, "cdn", "stable");
  const tls = { key: await readFile(path.join(session.secretsDir, "server.key")), cert: await readFile(path.join(dir, "server.crt")) };
  const server = https.createServer(tls, async (request, response) => {
    try {
      const url = new URL(request.url, session.origin);
      if (request.method === "POST" && url.pathname === "/__stop") {
        const expected = Buffer.from(`Bearer ${token}`);
        const supplied = Buffer.from(request.headers.authorization ?? "");
        if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) {
          response.writeHead(403).end(); return;
        }
        response.writeHead(204).end();
        server.close();
        return;
      }
      if (!["GET", "HEAD"].includes(request.method)) { response.writeHead(405).end(); return; }
      const name = url.pathname.startsWith("/stable/") ? url.pathname.slice(8) : "";
      let filename;
      if (["latest.json", "latest.json.minisig"].includes(name)) {
        const active = JSON.parse(await readFile(path.join(dir, "active.json"), "utf8"));
        filename = active.manifest + (name.endsWith(".minisig") ? ".minisig" : "");
      } else if (Object.values(session.packs).includes(name)) {
        filename = name;
      } else { response.writeHead(404).end(); return; }
      const file = path.join(root, filename);
      const size = (await stat(file)).size;
      let start = 0;
      let end = size - 1;
      let code = 200;
      if (request.headers.range) {
        const match = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range);
        if (!match || Number(match[1]) >= size || (match[2] && Number(match[2]) < Number(match[1]))) {
          response.writeHead(416, { "Content-Range": `bytes */${size}` }).end(); return;
        }
        start = Number(match[1]);
        end = match[2] ? Math.min(Number(match[2]), size - 1) : size - 1;
        code = 206;
      }
      const headers = { "Content-Length": end - start + 1, "Cache-Control": "no-store", "Content-Type": filename.endsWith(".json") ? "application/json" : "application/octet-stream" };
      if (code === 206) headers["Content-Range"] = `bytes ${start}-${end}/${size}`;
      response.writeHead(code, headers);
      if (request.method === "HEAD") response.end();
      else {
        const stream = createReadStream(file, { start, end });
        stream.on("error", () => response.destroy());
        stream.pipe(response);
      }
    } catch { if (!response.headersSent) response.writeHead(500); response.end(); }
  });
  server.on("error", (error) => { console.error(error.message); process.exitCode = 1; });
  server.listen(session.port, "127.0.0.1", () => console.log(`Serving ${session.origin}; Ctrl+C stops the server.`));
  for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, () => server.close());
}

const [command, ...args] = process.argv.slice(2);
try {
  const opts = options(args);
  const accepted = command === "prepare" ? ["--dir", "--tpk", "--port", "--private-root"]
    : command === "publish" ? ["--dir", "--scenario", "--tpk"] : ["--dir"];
  if (Object.keys(opts).some((flag) => !accepted.includes(flag))) {
    throw new Error(`Option is not valid for ${command}.`);
  }
  if (command === "prepare") await prepare(opts);
  else if (["publish", "serve", "stop"].includes(command) && opts["--dir"]) {
    const dir = path.resolve(opts["--dir"]);
    if (command === "publish") await publish(dir, opts["--scenario"], opts["--tpk"]);
    else if (command === "stop") await stop(dir);
    else await serve(dir);
  } else {
    throw new Error("Usage: smoke.mjs prepare [--tpk PATH] [--dir NEW_DIRECTORY] [--port 8443] [--private-root OS_TEMP_DIRECTORY] | publish --dir DIRECTORY --scenario base|patch|broken [--tpk PATH] | serve --dir DIRECTORY | stop --dir DIRECTORY");
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
