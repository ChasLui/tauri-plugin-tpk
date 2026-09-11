// Every job of the CI workflow must succeed on the commit currently at HEAD.
// "The run I looked at was green" is not the claim; "this commit is green" is.
import { run, fail } from './_run.mjs';

const head = run('git', ['rev-parse', 'HEAD']).out.trim();
if (!/^[0-9a-f]{40}$/.test(head)) fail('ci', `cannot resolve HEAD: ${head}`);

const r = run('gh', [
  'run', 'list', '--workflow', 'ci.yml', '--commit', head,
  '--json', 'status,conclusion,databaseId', '--limit', '5',
]);
if (r.code !== 0) fail('ci', r.out);

let runs;
try { runs = JSON.parse(r.out); } catch { fail('ci', `unparseable gh output: ${r.out}`); }
if (!Array.isArray(runs) || runs.length === 0) {
  fail('ci', `no CI run found for ${head}; push it and wait`);
}

const latest = runs[0];
if (latest.status !== 'completed') {
  fail('ci', `run ${latest.databaseId} is ${latest.status}, not completed`);
}
if (latest.conclusion !== 'success') {
  fail('ci', `run ${latest.databaseId} concluded ${latest.conclusion}`);
}

// A run is "success" even when jobs were skipped. Require every job to have
// actually succeeded.
const jobs = run('gh', [
  'run', 'view', String(latest.databaseId), '--json', 'jobs',
]);
if (jobs.code !== 0) fail('ci', jobs.out);
const parsed = JSON.parse(jobs.out).jobs ?? [];
const bad = parsed.filter((j) => j.conclusion !== 'success');
if (bad.length) {
  fail('ci', bad.map((j) => `${j.name}: ${j.conclusion}`).join('\n'));
}
if (parsed.length < 8) fail('ci', `only ${parsed.length} jobs ran; the matrix is incomplete`);

console.log(`commit ${head.slice(0, 7)}: ${parsed.length}/${parsed.length} CI jobs succeeded`);
console.log('GATE_CI_GREEN_PASS');
