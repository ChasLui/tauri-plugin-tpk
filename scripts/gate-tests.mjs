import { run, fail } from './_run.mjs';

const r = run('cargo', ['test', '--workspace', '--all-features']);
if (r.code !== 0) fail('tests', r.out);

// Count the tests rather than trusting "exit 0": a build that compiled no test
// targets also exits zero.
let passed = 0;
for (const m of r.out.matchAll(/^test result: ok\. (\d+) passed/gm)) passed += Number(m[1]);
if (passed < 300) fail('tests', `only ${passed} tests ran; expected the full suite`);

console.log(`${passed} tests passed`);
console.log('GATE_TESTS_PASS');
