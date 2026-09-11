// Clippy must be clean under the toolchain CI uses. Running it under whatever
// happens to be default locally is how `chunks_exact_to_as_chunks` reached main
// five times: the lint only exists from 1.98.
//
// So this pins the toolchain explicitly rather than inheriting a directory
// override, and refuses to report success from a clippy older than CI's.
import { run, fail } from './_run.mjs';

const MIN = [1, 98];
const TOOLCHAIN = process.env.GATE_TOOLCHAIN ?? 'stable';

const v = run('rustup', ['run', TOOLCHAIN, 'cargo', 'clippy', '--version']);
if (v.code !== 0) fail('clippy', `cannot run clippy under ${TOOLCHAIN}:\n${v.out}`);
// clippy reports its own version as `clippy 0.1.<rustc-minor>`, so the number
// that corresponds to a Rust release is the last component, not the middle one.
const m = /clippy 0\.1\.(\d+)/.exec(v.out);
if (!m) fail('clippy', `cannot parse clippy version from: ${v.out}`);
const minor = Number(m[1]);
if (minor < MIN[1]) {
  fail('clippy', `clippy 1.${minor} is older than CI's 1.${MIN[1]}; this run proves nothing`);
}

const r = run('rustup', [
  'run', TOOLCHAIN, 'cargo', 'clippy',
  '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings',
]);
if (r.code !== 0) fail('clippy', r.out);

console.log(`clippy 1.${minor} clean across the workspace (toolchain ${TOOLCHAIN})`);
console.log('GATE_CLIPPY_PASS');
