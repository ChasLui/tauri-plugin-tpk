import { spawnSync } from 'node:child_process';

/** Run a command, streaming nothing; return {code, out}. */
export function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, {
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    ...opts,
  });
  if (r.error) return { code: 127, out: String(r.error.message) };
  return { code: r.status ?? 1, out: `${r.stdout ?? ''}${r.stderr ?? ''}` };
}

/** Print a failure body and exit non-zero without emitting the success token. */
export function fail(label, detail) {
  console.error(`${label} FAILED`);
  if (detail) console.error(detail.slice(-4000));
  process.exit(1);
}
