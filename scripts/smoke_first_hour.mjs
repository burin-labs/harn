import assert from 'node:assert/strict';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

assert.ok(process.env.HARN_BINARY && isAbsolute(process.env.HARN_BINARY), 'HARN_BINARY must name the installed executable by absolute path');
const binary = realpathSync(process.env.HARN_BINARY);
const examples = fileURLToPath(new URL('../examples/first-hour/', import.meta.url));
const root = mkdtempSync(join(tmpdir(), 'harn-first-hour-'));
const home = join(root, 'home');
mkdirSync(home);
const env = Object.fromEntries(Object.entries(process.env).filter(([name]) =>
  ['PATH', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'PATHEXT', 'TMP', 'TEMP', 'TMPDIR'].includes(name.toUpperCase())));
Object.assign(env, {
  HOME: home, USERPROFILE: home, XDG_CONFIG_HOME: join(home, 'config'),
  XDG_CACHE_HOME: join(home, 'cache'), XDG_DATA_HOME: join(home, 'data'),
  APPDATA: join(home, 'AppData', 'Roaming'), LOCALAPPDATA: join(home, 'AppData', 'Local'),
  HARN_LLM_CALLS_DISABLED: '1', NO_COLOR: '1',
});

function run(args, expectedStatus = 0) {
  const result = spawnSync(binary, args, { cwd: root, env, encoding: 'utf8', timeout: 30_000, maxBuffer: 2 * 1024 * 1024 });
  assert.equal(result.error, undefined, String(result.error));
  assert.equal(result.status, expectedStatus, `${args.join(' ')}\n${result.stdout}\n${result.stderr}`);
  console.log(`$ harn ${args.join(' ')}`);
  if (result.stdout) process.stdout.write(result.stdout);
  return result;
}

function expectOutput(result, expected) {
  assert.equal(result.stdout.replaceAll('\r\n', '\n'), `${expected}\n`);
}

try {
  for (const name of ['agent.harn', 'tools.harn', 'tools.jsonl', 'record-replay.harn', 'tool-recovery.harn', 'tool-recovery.jsonl']) {
    copyFileSync(join(examples, name), join(root, name));
  }
  const version = run(['--version']);
  assert.match(version.stdout, /^harn \d+\.\d+\.\d+/);
  expectOutput(run(['run', 'agent.harn']), 'Running: Say hello.\ndone\nMock response to 2-word prompt: Say hello.');
  expectOutput(run(['run', 'tools.harn', '--llm-mock', 'tools.jsonl']), 'tool: 2 + 3 = 5\ndone\n2 + 3 = 5.');

  const recorded = run(['run', 'record-replay.harn', '--llm-mock-record', 'responses.jsonl']);
  expectOutput(recorded, 'Mock response to 2-word prompt: Say hello.');
  assert.ok(statSync(join(root, 'responses.jsonl')).size > 0, 'recording must contain a response');
  const replayed = run(['run', 'record-replay.harn', '--llm-mock', 'responses.jsonl']);
  assert.equal(replayed.stdout, recorded.stdout, 'record and replay output must be identical');

  writeFileSync(join(root, 'sentinel.jsonl'), '{"schemaVersion":1,"strictScopes":false}\n{"id":"sentinel","scope":"default","consume":"once","text":"Recorded fixture reached."}\n');
  expectOutput(run(['run', 'record-replay.harn', '--llm-mock', 'sentinel.jsonl']), 'Recorded fixture reached.');
  writeFileSync(join(root, 'empty.jsonl'), '');
  const refused = run(['run', 'record-replay.harn', '--llm-mock', 'empty.jsonl'], 1);
  assert.match(refused.stderr, /No --llm-mock fixture matched prompt/);

  expectOutput(run(['run', 'tool-recovery.harn', '--llm-mock', 'tool-recovery.jsonl']),
    'tool: 2 + 3 = 5\ndone\nrejected calls: 1\nsuccessful calls: 1\n2 + 3 = 5.');
  const recovery = readFileSync(join(root, 'tool-recovery.jsonl'), 'utf8').trim().split('\n').map(line => JSON.parse(line));
  const correction = recovery.find(entry => entry.id === 'correct-after-error');
  assert.ok(correction, 'negative control must change the correction entry');
  correction.match = '*THIS FEEDBACK NEVER OCCURS*';
  writeFileSync(join(root, 'broken-recovery.jsonl'), `${recovery.map(entry => JSON.stringify(entry)).join('\n')}\n`);
  const broken = run(['run', 'tool-recovery.harn', '--llm-mock', 'broken-recovery.jsonl'], 1);
  assert.match(broken.stderr, /Thrown: the loop must complete/);
  assert.doesNotMatch(broken.stdout, /tool:/, 'bad arguments must not reach the handler');
  console.log('First-hour smoke passed: agent, tool dispatch, response replay, and tool-error recovery; both refusal controls fired.');
} finally {
  rmSync(root, { recursive: true, force: true });
}
