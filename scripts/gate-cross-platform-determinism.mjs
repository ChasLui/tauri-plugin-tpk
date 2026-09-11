// `tpk pack` promises that the same input, --version-code and --created-at
// produce the same bytes. The blacklist matches on (id, version_code) and the
// channel manifest carries a SHA-256, so if a rebuild on another OS differs, a
// condemned release can slip back onto devices and a published digest stops
// meaning anything.
//
// No CI job compares artifacts across runners, so this builds the same fixture
// on this host and inside a Parallels Windows 11 VM and compares digests.
//
// The comparison must be able to fail. Before trusting equality it perturbs
// --created-at and asserts the digests then DIFFER; a checker that compared
// nothing would pass the equality test and fail this control.
//
// Requires: a running Parallels VM with prlctl exec, C:\tpk\tpk.exe already
// staged, and this host reachable from the VM over GATE_HOST_IP:GATE_HTTP_PORT.
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { run, fail } from './_run.mjs';

const VM = process.env.GATE_VM ?? 'Windows 11';
const HOST_IP = process.env.GATE_HOST_IP ?? '192.168.8.31';
const PORT = Number(process.env.GATE_HTTP_PORT ?? 18899);
const ID = 'core';
const VERSION = '1.0.0';
const VERSION_CODE = '20260911120000';
const CREATED_AT = '2026-09-11T12:00:00Z';
const PERTURBED = '2026-09-11T12:00:01Z';

// Nested directories and a name that sorts after them: the path-separator and
// read_dir-order differences between platforms only appear below the top level.
const FIXTURE = {
  'index.html':
    '<!DOCTYPE html><html><head><link rel="stylesheet" href="./a/s.css"></head>' +
    '<body><h1>x</h1><script type="module" src="./a/m.js"></script></body></html>\n',
  'a/s.css': 'body{color:red}\n',
  'a/m.js': 'export const n = 1;\n',
  'a/b/deep.json': '{"k":"v"}\n',
  'z-last.txt': 'zzz\n',
};

const HOST_BASE = `http://${HOST_IP}:${PORT}`;

const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

const key = process.env.TPK_SIGNING_KEY;
if (!key) fail('determinism', 'TPK_SIGNING_KEY is not set');

// Serve the key and a driver script to the VM. prlctl exec cannot carry a
// command long enough to inline them.
const served = new Map();
served.set('/signing.key', Buffer.from(key, 'utf8'));

// The VM rebuilds the fixture from literal bytes rather than receiving a copy,
// so its own filesystem and directory-iteration order are what get exercised.
let driver = "$ErrorActionPreference='Stop'\n$d='C:\\tpk\\dist'\n" +
  "if (Test-Path $d) { Remove-Item -Recurse -Force $d }\n";
for (const [rel, body] of Object.entries(FIXTURE)) {
  const win = rel.replace(/\//g, '\\');
  const b64 = Buffer.from(body, 'utf8').toString('base64');
  driver += `$p="C:\\tpk\\dist\\${win}"\n` +
    `New-Item -ItemType Directory -Force -Path (Split-Path $p) | Out-Null\n` +
    `[IO.File]::WriteAllBytes($p, [Convert]::FromBase64String('${b64}'))\n`;
}
driver =
  // The driver fetches its own inputs: `prlctl exec` rejects any argument
  // containing a semicolon, so the VM can only be handed single statements.
  `$ErrorActionPreference='Stop'\n` +
  `$ProgressPreference='SilentlyContinue'\n` +
  `Invoke-WebRequest -Uri '${HOST_BASE}/signing.key' -OutFile 'C:\\tpk\\signing.key' -UseBasicParsing\n` +
  driver +
  `$env:TPK_SIGNING_KEY = [IO.File]::ReadAllText('C:\\tpk\\signing.key')\n` +
  `function Pack($createdAt, $out) {\n` +
  `  & C:\\tpk\\tpk.exe pack --kind base --id ${ID} --version ${VERSION} \`\n` +
  `    --version-code ${VERSION_CODE} --created-at $createdAt \`\n` +
  `    --dist C:\\tpk\\dist --out $out | Out-Null\n` +
  `  if ($LASTEXITCODE -ne 0) { throw "pack failed: $LASTEXITCODE" }\n` +
  `  (Get-FileHash $out -Algorithm SHA256).Hash.ToLower()\n` +
  `}\n` +
  `$same = Pack '${CREATED_AT}' 'C:\\tpk\\vm-same.tpk'\n` +
  `$diff = Pack '${PERTURBED}' 'C:\\tpk\\vm-diff.tpk'\n` +
  `Write-Output ("VMSAME=" + $same)\n` +
  `Write-Output ("VMDIFF=" + $diff)\n`;
served.set('/driver.ps1', Buffer.from(driver, 'utf8'));

// Files the VM fetches. Served by `python3 -m http.server` rather than an
// in-process Node server: macOS blocks an unapproved binary from listening, and
// a firewall prompt nobody sees turns into a checker that times out.
const SERVE_DIR = mkdtempSync(join(tmpdir(), 'tpk-serve-'));
for (const [name, body] of served) {
  writeFileSync(join(SERVE_DIR, name.replace(/^\//, '')), body);
}

function startServer() {
  const child = spawn('python3', ['-m', 'http.server', String(PORT), '--bind', '0.0.0.0'], {
    cwd: SERVE_DIR, stdio: 'ignore', detached: false,
  });
  return child;
}

function packHost(dist, out, createdAt) {
  const r = run('cargo', [
    'run', '-q', '-p', 'tpk-cli', '--', 'pack',
    '--kind', 'base', '--id', ID, '--version', VERSION,
    '--version-code', VERSION_CODE, '--created-at', createdAt,
    '--dist', dist, '--out', out,
  ]);
  if (r.code !== 0) fail('determinism', `host pack failed:\n${r.out}`);
  return sha256(readFileSync(out));
}

async function main() {
  const server = startServer();
  const stop = () => { try { server.kill('SIGKILL'); } catch { /* already gone */ } };
  process.on('exit', stop);
  // Give the server a moment to bind; a failed bind shows up as a fetch
  // timeout below, which fails the gate rather than hanging it.
  await new Promise((r) => setTimeout(r, 1500));

  const work = mkdtempSync(join(tmpdir(), 'tpk-det-'));
  const dist = join(work, 'dist');
  for (const [rel, body] of Object.entries(FIXTURE)) {
    const abs = join(dist, rel);
    mkdirSync(dirname(abs), { recursive: true });
    // Explicit LF bytes: a CRLF checkout would change content, which is a git
    // configuration difference and not the property under test.
    writeFileSync(abs, Buffer.from(body, 'utf8'));
  }

  const hostSame = packHost(dist, join(work, 'host-same.tpk'), CREATED_AT);
  const hostDiff = packHost(dist, join(work, 'host-diff.tpk'), PERTURBED);

  // Two calls, each a single statement with no semicolon anywhere.
  const fetched = run('prlctl', ['exec', VM, 'powershell.exe', '-NoProfile', '-Command',
    `Invoke-WebRequest -Uri http://${HOST_IP}:${PORT}/driver.ps1 -OutFile C:\\tpk\\driver.ps1 -UseBasicParsing`,
  ], { timeout: 120_000 });
  if (fetched.code !== 0) {
    stop();
    fail('determinism', `could not stage the driver in the VM:\n${fetched.out}`);
  }
  const vm = run('prlctl', ['exec', VM, 'powershell.exe', '-NoProfile',
    '-ExecutionPolicy', 'Bypass', '-File', 'C:\\tpk\\driver.ps1'],
    { timeout: 300_000 });
  stop();

  const same = /VMSAME=([0-9a-f]{64})/.exec(vm.out);
  const diff = /VMDIFF=([0-9a-f]{64})/.exec(vm.out);
  if (!same || !diff) fail('determinism', `VM run produced no digests:\n${vm.out}`);

  console.log(`host same    ${hostSame}`);
  console.log(`vm   same    ${same[1]}`);
  console.log(`host control ${hostDiff}`);
  console.log(`vm   control ${diff[1]}`);

  // Positive control first: a checker that ignored its inputs would pass the
  // equality assertion below, so require the perturbation to move the bytes on
  // both sides before any equality result is trusted.
  if (hostDiff === hostSame) {
    fail('determinism', 'control failed on host: changing --created-at did not change the bytes');
  }
  if (diff[1] === same[1]) {
    fail('determinism', 'control failed in VM: changing --created-at did not change the bytes');
  }

  if (hostSame !== same[1]) {
    fail('determinism',
      `macOS and Windows 11 produced different bytes for identical input:\n` +
      `  host ${hostSame}\n  vm   ${same[1]}`);
  }

  console.log('macOS and Windows 11 packs are byte-identical; both controls confirm the comparison can fail');
  console.log('GATE_DETERMINISM_PASS');
}

main().catch((e) => fail('determinism', String(e?.stack ?? e)));
