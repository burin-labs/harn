import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';

const root = resolve(import.meta.dirname, '../..');
const scratch = mkdtempSync(`${tmpdir()}/harn-strict-measurement-`);
const fake = `${scratch}/harn`;
writeFileSync(fake, '#!/usr/bin/env node\nimport("node:fs").then(({readFileSync}) => { process.stdout.write(readFileSync(process.env.FAKE_REPORT)); process.exitCode = Number(process.env.FAKE_STATUS); });\n', { mode: 0o700 });
const path = 'crates/harn-stdlib/src/stdlib/cli/try.harn';
const expected = execFileSync('git', ['ls-files', '-z', '--', 'crates/harn-stdlib/src/stdlib'], {
  cwd: root, encoding: 'utf8',
}).split('\0').filter(path => path.endsWith('.harn') || path.endsWith('.harn.txt'));
const diagnostic = (code, severity = 'warning', source = 'lint') => ({
  code, severity, source, message: 'recorded finding',
});
const report = (diagnostics = [], filePath = path) => {
  const status = diagnostics.some(item => item.severity === 'error') ? 'error'
    : diagnostics.some(item => item.severity === 'warning') ? 'warning' : 'ok';
  return {
    schemaVersion: 1, ok: true, error: null, warnings: [],
    data: {
      files: [{ path: filePath, status, diagnostics, predicate_manifest: null },
        ...expected.filter(candidate => candidate !== filePath).map(path => ({
          path, status: 'ok', diagnostics: [], predicate_manifest: null,
        }))],
      summary: { ok: expected.length - Number(status !== 'ok'), warnings: Number(status === 'warning'),
        errors: Number(status === 'error'), diagnostics: diagnostics.length },
    },
  };
};
let trials = 0;
function check(value, expected, status = value?.ok === false ? 1 : 0, message = '') {
  const input = `${scratch}/report.json`;
  writeFileSync(input, typeof value === 'string' ? value : JSON.stringify(value));
  const result = spawnSync(`${root}/scripts/check_stdlib_strict_types.sh`, [], {
    cwd: root, encoding: 'utf8', timeout: 10_000,
    env: { PATH: process.env.PATH, HOME: process.env.HOME, HARN_BIN: fake,
      FAKE_REPORT: input, FAKE_STATUS: String(status) },
  });
  assert.equal(result.error, undefined);
  assert.equal(result.status, expected, `${message}: ${result.stdout}${result.stderr}`);
  assert.match(`${result.stdout}${result.stderr}`, expected === 0 ? /\d+ measured files/ : /refused|gate failed/);
  trials += 1;
}
try {
  check(report(), 0);
  check(report([diagnostic('HARN-LNT-032')]), 0);
  check(report([diagnostic('HARN-OWN-004')], 'crates/harn-stdlib/src/stdlib/agent/user.harn'), 0);
  check(report([diagnostic('HARN-OWN-004')]), 1);
  const failed = report([diagnostic('HARN-TYP-006', 'error', 'type')]);
  failed.ok = false;
  failed.error = { code: 'check_failed', message: 'findings', details: null };
  check(failed, 1, 1, 'ordinary type error without prose locator');
  const preflight = structuredClone(failed);
  preflight.data.files[0].diagnostics = [diagnostic('HARN-MOD-001', 'error', 'preflight')];
  check(preflight, 1);
  check('', 1, 73, 'silent crash');
  check('', 1, 1, 'empty diagnostic failure');
  check('', 1, 0, 'empty successful process');
  check('{broken', 1);
  check(report(), 1, 1, 'status contradicts success');
  check(report(), 1, 73, 'report cannot excuse abnormal exit');
  const empty = report(); empty.data.files = []; empty.data.summary.ok = 0; check(empty, 1);
  const duplicate = report(); duplicate.data.files.push(duplicate.data.files[0]); check(duplicate, 1);
  check(report([], 'scripts/other.harn'), 1);
  const summary = report(); summary.data.summary.ok = 99; check(summary, 1);
  const invalid = report([diagnostic('HARN-LNT-032', 'unexpected')]); check(invalid, 1);
  const missing = report(); delete missing.data.files[0].diagnostics; check(missing, 1);
  const partial = report(); partial.data.files.pop(); partial.data.summary.ok -= 1; check(partial, 1);
  const version = report(); version.schemaVersion = 2; check(version, 1);
  const noData = { schemaVersion: 1, ok: false, data: null,
    error: { code: 'no_harn_files', message: 'empty', details: null }, warnings: [] };
  check(noData, 1);
  const emptyFailure = report(); emptyFailure.ok = false;
  emptyFailure.error = { code: 'check_failed', message: 'unknown failure', details: null };
  check(emptyFailure, 1);
  const badSpan = report([diagnostic('HARN-LNT-032')]);
  badSpan.data.files[0].diagnostics[0].span = { start: 4, end: 3 }; check(badSpan, 1);
  console.log(`check_stdlib_strict_types_test: ${trials} measured cases passed`);
} finally {
  rmSync(scratch, { recursive: true });
}
