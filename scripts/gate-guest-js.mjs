// The guest API must build under the toolchain CI uses, and produce exactly the
// files `package.json` promises.
//
// Two failures motivated this gate, both of the same shape — a local toolchain
// newer than CI's, reporting success for something CI would reject:
//   - tsdown needs Node >= 22.18; CI pinned Node 20, so `pnpm install` passed
//     and the build died on `Promise.withResolvers is not a function`. pnpm
//     does not enforce a transitive tool's `engines` at install time.
//   - pnpm 11 locally blocks esbuild's install script while CI pins pnpm 10, so
//     a local run can fail for a reason CI never sees, and vice versa.
//
// So this pins BOTH versions to what the workflow declares, reading them out of
// the workflow rather than restating them.
import { readFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { run, fail } from './_run.mjs';

const PKG_DIR = 'packages/tauri-plugin-tpk-api';
const WORKFLOW = '.github/workflows/ci.yml';

const ci = readFileSync(WORKFLOW, 'utf8');
const nodePin = /node-version:\s*(\d+)/.exec(ci);
const pnpmPin = /pnpm\/action-setup[\s\S]{0,200}?version:\s*(\d+)/.exec(ci);
if (!nodePin) fail('guest-js', `no node-version found in ${WORKFLOW}`);
if (!pnpmPin) fail('guest-js', `no pnpm version found in ${WORKFLOW}`);
const wantNode = Number(nodePin[1]);
const wantPnpm = pnpmPin[1];

// The declared build-time floor. Building on something CI does not use proves
// nothing about CI.
const pkg = JSON.parse(readFileSync(join(PKG_DIR, 'package.json'), 'utf8'));
const floor = pkg.devEngines?.runtime?.version;
if (!floor) fail('guest-js', 'package.json declares no devEngines.runtime.version');
const floorMajor = Number(/(\d+)/.exec(floor)[1]);
if (wantNode < floorMajor) {
  fail('guest-js',
    `${WORKFLOW} pins Node ${wantNode} but the build needs ${floor}; ` +
    `CI will fail at build time even though install succeeds`);
}

const major = Number(process.versions.node.split('.')[0]);
if (major !== wantNode) {
  fail('guest-js',
    `this Node is ${process.versions.node} but CI uses ${wantNode}; ` +
    `run under Node ${wantNode} or this result does not predict CI`);
}

const opts = { cwd: PKG_DIR };
const install = run('npx', ['--yes', `pnpm@${wantPnpm}`, 'install', '--frozen-lockfile'], opts);
if (install.code !== 0) fail('guest-js', install.out);

const build = run('npx', ['--yes', `pnpm@${wantPnpm}`, 'build'], opts);
if (build.code !== 0) fail('guest-js', build.out);

// Exit zero is not enough: tsdown's `platform: node` default emits .mjs/.d.mts,
// which builds "successfully" and ships a package whose `exports` resolve to
// nothing. Check the files the manifest actually points at.
const declared = [
  pkg.main,
  pkg.types,
  pkg.exports?.['.']?.import,
  pkg.exports?.['.']?.types,
].filter(Boolean);
const missing = [...new Set(declared)].filter(
  (rel) => !existsSync(join(PKG_DIR, rel.replace(/^\.\//, ''))),
);
if (missing.length) {
  fail('guest-js', `package.json points at files the build did not emit: ${missing.join(', ')}`);
}

console.log(`built under Node ${process.versions.node} / pnpm ${wantPnpm}`);
console.log(`all declared entry points exist: ${[...new Set(declared)].join(', ')}`);
console.log('GATE_GUEST_JS_PASS');
