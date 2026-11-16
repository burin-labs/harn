import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {load} from 'js-yaml';
import test from 'node:test';
import {githubConsumerSelection, observerRefusal, observerReadCredentials} from '../lib/consumer-observer.mjs';
import {consumerCompositionPath, consumerSourceDeclaration} from '../lib/consumer-source.mjs';

const repository = 'fixture/private-consumer';
const sha = value => value.repeat(40);
const base = sha('a');
const head = sha('2');
const declaration = () => consumerSourceDeclaration('Pairs-with: consumer#7\nPairs-with: consumer#8\nConsumer-composition: consumer#9');
const commit = (value, ...parents) => ({sha: sha(value), parents: parents.map(parent => ({sha: sha(parent)}))});

function fixture() {
  const manifest = {schema: 'harn.consumer_composition.v1', repository, base_sha: base,
    adaptations: [{pull_request: 7, head_sha: sha('b')}, {pull_request: 8, head_sha: sha('c')}],
    repairs: [{pull_request: 12, head_sha: sha('d')}]};
  const records = new Map();
  const calls = [];
  const counts = new Map();
  const components = [commit('b', 'a'), commit('c', 'a'), commit('d', 'a')];
  const all = [...components, commit('e', 'a', 'b'), commit('f', 'e', 'c'), commit('1', 'f', 'd'), commit('2', '1')];
  const compare = (left, right, commits, status = 'ahead', mergeBase = left) => {
    records.set(`/compare/${left}...${right}?per_page=100&page=1`, {
      base_commit: {sha: left}, merge_base_commit: {sha: mergeBase}, status, total_commits: commits.length, commits,
    });
  };
  records.set('', {full_name: repository, default_branch: 'main'});
  records.set('/commits/main', {sha: base});
  compare(base, base, [], 'identical');
  compare(base, head, all);
  for (const component of components) {
    compare(base, component.sha, [component]);
    compare(component.sha, head, all.filter(item => item.sha !== component.sha));
  }
  const pull = (number, revision, branch) => ({number, state: 'open', merged: false,
    base: {repo: {full_name: repository}, ref: 'main'}, head: {repo: {full_name: repository}, ref: branch, sha: revision}});
  records.set('/pulls/9', pull(9, head, 'combined-private-branch'));
  for (const [number, revision] of [[7, 'b'], [8, 'c'], [12, 'd']]) {
    records.set(`/pulls/${number}`, pull(number, sha(revision), `private-${number}`));
  }
  const writeManifest = value => {
    const bytes = Buffer.from(JSON.stringify(value));
    const blob = createHash('sha1').update(`blob ${bytes.length}\0`).update(bytes).digest('hex');
    records.set(`/contents/${consumerCompositionPath}?ref=${head}`, {type: 'file', path: consumerCompositionPath,
      encoding: 'base64', size: bytes.length, sha: blob, content: bytes.toString('base64')});
    records.set(`/commits/${head}?per_page=100`, {...all.at(-1), files: [{filename: consumerCompositionPath, status: 'added', sha: blob}]});
    return bytes;
  };
  const bytes = writeManifest(manifest);
  const fetchRecord = async (url, options) => {
    assert.equal(new URL(url).origin, 'https://api.github.com');
    assert.equal(options.headers.Authorization, 'Bearer consumer-read');
    const suffix = new URL(url).pathname.replace(`/repos/${repository}`, '') + new URL(url).search;
    calls.push(suffix);
    const count = (counts.get(suffix) ?? 0) + 1;
    counts.set(suffix, count);
    const record = records.get(suffix);
    assert.notEqual(record, undefined, `unexpected authenticated read ${suffix}`);
    const value = typeof record === 'function' ? record(count) : record;
    return new Response(JSON.stringify(value), {status: 200});
  };
  return {manifest, records, calls, bytes, writeManifest, fetchRecord};
}

test('authenticated selection reaches every component and refreshes identities after graph proof', async () => {
  const f = fixture();
  const source = await githubConsumerSelection('consumer-read', repository, declaration(), f.fetchRecord);
  assert.deepEqual(source, {headSha: head, branch: 'combined-private-branch', composition: {
    schema: 'harn.consumer_composition_identity.v1', pull: '9', requiredPulls: ['7', '8'],
    manifestSha256: createHash('sha256').update('harn.consumer_composition_manifest.v1\0').update(f.bytes).digest('hex'),
  }});
  for (const number of [7, 8, 12, 9]) assert.equal(f.calls.filter(call => call === `/pulls/${number}`).length, 2);
  assert.equal(f.calls.at(-1), '/pulls/9');
  assert.equal(Object.isFrozen(source.composition.requiredPulls), true);
});

const invalid = [
  ['foreign selected pull', f => { f.records.get('/pulls/9').head.repo.full_name = 'foreign/fork'; }],
  ['wrong selected pull identity', f => { f.records.get('/pulls/9').number = 10; }],
  ['retargeted selected pull', f => { f.records.get('/pulls/9').base.ref = 'other'; }],
  ['closed composition', f => { f.records.get('/pulls/9').state = 'closed'; }],
  ['foreign component', f => { f.records.get('/pulls/7').head.repo.full_name = 'foreign/fork'; }],
  ['closed unmerged component', f => { f.records.get('/pulls/7').state = 'closed'; }],
  ['component head moved before read', f => { f.records.get('/pulls/7').head.sha = sha('4'); }],
  ['missing adaptation', f => { f.manifest.adaptations.pop(); f.writeManifest(f.manifest); }],
  ['repair substituting for adaptation', f => { f.manifest.repairs.push(f.manifest.adaptations.pop()); f.writeManifest(f.manifest); }],
  ['manifest blob identity mismatch', f => { f.records.get(`/contents/${consumerCompositionPath}?ref=${head}`).sha = sha('0'); }],
  ['manifest byte count mismatch', f => { f.records.get(`/contents/${consumerCompositionPath}?ref=${head}`).size++; }],
  ['manifest path redirect', f => { f.records.get(`/contents/${consumerCompositionPath}?ref=${head}`).path = 'other.json'; }],
  ['base outside owning main', f => { f.records.get(`/compare/${base}...${base}?per_page=100&page=1`).status = 'diverged'; }],
  ['advertised component missing from merged ancestry', f => { f.records.get(`/compare/${sha('b')}...${head}?per_page=100&page=1`).merge_base_commit.sha = base; }],
  ['partial graph census', f => { f.records.get(`/compare/${base}...${head}?per_page=100&page=1`).total_commits++; }],
  ['empty graph census', f => { const value = f.records.get(`/compare/${base}...${head}?per_page=100&page=1`); value.total_commits = 0; value.commits = []; }],
  ['extra linear code commit', f => { const value = f.records.get(`/compare/${base}...${head}?per_page=100&page=1`); value.commits.push(commit('3', '1')); value.total_commits++; }],
  ['extra file in manifest commit', f => { f.records.get(`/commits/${head}?per_page=100`).files.push({filename: 'code.rs', status: 'added', sha: sha('4')}); }],
  ['manifest commit changes different blob', f => { f.records.get(`/commits/${head}?per_page=100`).files[0].sha = sha('4'); }],
];
for (const [name, change] of invalid) test(`authenticated composition refuses ${name}`, async () => {
  const f = fixture();
  change(f);
  await assert.rejects(githubConsumerSelection('consumer-read', repository, declaration(), f.fetchRecord), error => {
    const {line} = observerRefusal(error, 'unmeasured');
    assert.match(line, /^CONSUMER_OBSERVER reason=consumer_(composition|source)_/);
    for (const privateValue of [repository, 'combined-private-branch', head, 'foreign/fork']) assert.equal(line.includes(privateValue), false);
    return true;
  });
});

for (const number of [7, 8, 12, 9]) test(`final refresh refuses pull ${number} moving after graph measurement`, async () => {
  const f = fixture();
  const original = structuredClone(f.records.get(`/pulls/${number}`));
  f.records.set(`/pulls/${number}`, count => count === 1 ? original : {...original, head: {...original.head, sha: sha('4')}});
  await assert.rejects(githubConsumerSelection('consumer-read', repository, declaration(), f.fetchRecord), /consumer_composition_(component|source)_moved$/);
});

test('a refused private read does not try parent authority or a weaker source', async () => {
  const calls = [];
  const credentials = observerReadCredentials('parent-read', 'consumer-read', 'burin-labs/harn', repository);
  await assert.rejects(githubConsumerSelection(credentials, repository, declaration(), async (url, options) => {
    calls.push({url, authorization: options.headers.Authorization});
    return new Response('{}', {status: 403});
  }), /consumer_source_read_refused$/);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].authorization, 'Bearer consumer-read');
});

test('actual CI pin and private Bash source entrypoints select the combined head and refuse a missing component', t => {
  const root = fileURLToPath(new URL('../../', import.meta.url));
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-composition-entries-'));
  t.after(() => fs.rmSync(scratch, {recursive: true, force: true}));
  const f = fixture();
  const records = path.join(scratch, 'records.json');
  const preload = path.join(scratch, 'fetch.mjs');
  const output = path.join(scratch, 'output');
  fs.writeFileSync(preload, `import fs from 'node:fs';
    const records = new Map(JSON.parse(fs.readFileSync(${JSON.stringify(records)}, 'utf8')));
    globalThis.fetch = async (url, options) => {
      if (options.headers.Authorization !== 'Bearer consumer-read') throw new Error('wrong authority');
      const endpoint = new URL(url);
      const key = endpoint.pathname.replace('/repos/${repository}', '') + endpoint.search;
      const value = records.get(key);
      return new Response(JSON.stringify(value ?? {}), {status: value ? 200 : 404});
    };`, {mode: 0o600});
  const text = 'Pairs-with: consumer#7\nPairs-with: consumer#8\nConsumer-composition: consumer#9';
  const env = {PATH: process.env.PATH, NODE_OPTIONS: `--import=${preload}`, GH_TOKEN: 'consumer-read',
    CONSUMER: repository, CANARY_REPOSITORY: repository, PULL_REQUEST_BODY: text, GITHUB_OUTPUT: output,
    CANARY_PAIRED_PULL: '9', CANARY_SOURCE_DECLARATION: JSON.stringify(declaration())};
  const workflow = load(fs.readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8'));
  const recipe = workflow.jobs['consumer-pin'].steps.find(step => step.id === 'read').run;
  const script = path.join(scratch, 'pin.sh');
  fs.writeFileSync(script, recipe);
  const execute = (command, args) => spawnSync(command, args, {cwd: root, encoding: 'utf8', env});
  for (const missing of [false, true]) {
    fs.writeFileSync(output, '');
    const data = new Map(f.records);
    if (missing) data.delete('/pulls/8');
    fs.writeFileSync(records, JSON.stringify([...data]), {mode: 0o600});
    const pin = execute('bash', [script]);
    assert.equal(pin.status, missing ? 1 : 0, pin.stderr);
    assert.equal(fs.readFileSync(output, 'utf8'), missing ? '' : `sha=${head}\n`);
    assert.doesNotMatch(pin.stdout + pin.stderr, /private-consumer|combined-private-branch|consumer-read/);
    const source = execute(process.execPath, ['scripts/ci/consumer-source.mjs', '--selected']);
    assert.equal(source.status, missing ? 1 : 0, source.stderr);
    if (missing) assert.equal(source.stdout, '');
    else assert.equal(JSON.parse(source.stdout).source.headSha, head);
    assert.doesNotMatch(source.stderr, /private-consumer|combined-private-branch|consumer-read/);
  }
});
