// `cargo deny check` with no subset argument runs advisories, bans, licenses
// and sources. Passing only `licenses bans` locally is what hid five RUSTSEC
// vulnerabilities.
import { run, fail } from './_run.mjs';

const r = run('cargo', ['deny', 'check']);
if (r.code !== 0) fail('cargo-deny', r.out);

// Guard against a future `cargo deny check` that silently skips advisories.
if (!/advisories ok/.test(r.out)) {
  fail('cargo-deny', `exit 0 but advisories did not report ok:\n${r.out}`);
}

console.log(r.out.trim().split('\n').slice(-1)[0]);
console.log('GATE_DENY_PASS');
