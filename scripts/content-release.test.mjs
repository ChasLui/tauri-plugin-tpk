import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

// Exercise the actual upload step with no network or credentials.
const workflow = readFileSync(new URL('../.github/workflows/content-release.yml', import.meta.url), 'utf8');
const upload = workflow.split('      - name: Upload\n')[1]?.split('\n      - name:')[0];
assert.ok(upload, 'Upload step exists');
const script = upload.split('        run: |\n')[1]?.replace(/^          /gm, '');
assert.ok(script, 'Upload script exists');
const docs = readFileSync(new URL('../docs/packaging.md', import.meta.url), 'utf8');
const docScript = docs.match(/# Check every immutable object[^]*?\n```/)?.[0].slice(0, -4);
assert.ok(docScript, 'Documented upload script exists');

const cases = [
  ['missing objects allow upload', '', '404', true],
  ['NotFound allows upload', '', 'NotFound', true],
  ['old pack outside the channel blocks upload', 'stable/core-1.0.0.tpk', '404', false],
  ['existing pinned manifest blocks all uploads', 'stable/123.json', '404', false],
  ['existing pinned signature blocks all uploads', 'stable/123.json.minisig', '404', false],
  ['access denied blocks all uploads', '', '403', false],
  ['transport failure blocks all uploads', '', 'transport', false],
];

for (const [source, body, prefix] of [['workflow', script, 'stable'], ['docs', docScript, 'tpk/core']]) {
  for (const [name, existingKey, errorCode, allowed] of cases) {
    test(`${source}: ${name}`, () => {
      const dir = mkdtempSync(join(tmpdir(), 'tpk-upload-'));
      try {
        writeFileSync(join(dir, 'aws'), `#!/bin/bash
if [ "$1" = s3api ]; then
  [ "$6" = "$EXISTING_KEY" ] && exit 0
  echo "An error occurred ($ERROR_CODE) when calling the HeadObject operation" >&2
  exit 1
fi
echo "$*" >> "$UPLOAD_LOG"
`, { mode: 0o755 });
        writeFileSync(join(dir, 'jq'), '#!/bin/bash\necho 123\n', { mode: 0o755 });
        writeFileSync(join(dir, 'sleep'), '#!/bin/bash\nexit 0\n', { mode: 0o755 });
        const log = join(dir, 'uploads');
        writeFileSync(log, '');
        const result = spawnSync('bash', ['-c', `set -euo pipefail\n${body}`], {
          cwd: dir,
          encoding: 'utf8',
          env: { PATH: `${dir}:/usr/bin:/bin`, BUCKET: 'test', CHANNEL: 'stable',
            PACK_ID: 'core', VERSION: '1.0.0', EXISTING_KEY: existingKey.replace(/^stable/, prefix),
            ERROR_CODE: errorCode, UPLOAD_LOG: log },
        });
        assert.equal(result.status === 0, allowed, result.stdout + result.stderr);
        const uploads = readFileSync(log, 'utf8').trim().split('\n').filter(Boolean);
        assert.equal(uploads.length, allowed ? 5 : 0, 'check every object before any write');
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    });
  }
}
