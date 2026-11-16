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
import {consumerCompositionPath} from '../lib/consumer-source.mjs';
import {repository,sha,base,head,declaration,commit,compositionFixture as fixture} from './fixtures/consumer_composition.mjs';

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
  ['empty unequal-head ancestry', f => { const value = f.records.get(`/compare/${sha('b')}...${head}?per_page=100&page=1`); value.total_commits = 0; value.commits = []; }],
  ['component absent from combined commit inventory', f => { const value = f.records.get(`/compare/${base}...${head}?per_page=100&page=1`); value.commits = value.commits.filter(item => item.sha !== sha('b')); value.total_commits--; }],
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

test('a default branch retarget during graph reads refuses the original declaration', async () => {
  const f = fixture();
  const original = structuredClone(f.records.get(''));
  f.records.set('', count => count === 1 ? original : {...original, default_branch: 'retargeted'});
  await assert.rejects(githubConsumerSelection('consumer-read', repository, declaration(), f.fetchRecord), /consumer_composition_source_moved$/);
});

for (const status of ['ahead', 'diverged']) test(`a default head moving ${status} receives a fresh ancestry decision`, async () => {
  const f = fixture();
  f.records.set('/commits/main', count => ({sha: count === 1 ? base : sha('4')}));
  f.records.set(`/compare/${base}...${sha('4')}?per_page=100&page=1`, {
    base_commit: {sha: base}, merge_base_commit: {sha: status === 'ahead' ? base : sha('3')},
    status, total_commits: 1, commits: [commit('4', 'a')],
  });
  const selected = githubConsumerSelection('consumer-read', repository, declaration(), f.fetchRecord);
  if (status === 'ahead') assert.equal((await selected).headSha, head);
  else await assert.rejects(selected, /consumer_composition_ancestry_unproven$/);
  assert.equal(f.calls.filter(call => call === `/compare/${base}...${sha('4')}?per_page=100&page=1`).length, 1);
});

test('a complete paginated graph is accepted and a missing next page refuses', async () => {
  for (const partial of [false, true]) {
    const f = fixture();
    const path = `/compare/${base}...${head}?per_page=100&page=`;
    const original = f.records.get(`${path}1`);
    const history = Array.from({length: 94}, (_, index) => ({sha: index.toString(16).padStart(40, '0'), parents: [{sha: base}]}));
    const all = [...history, ...original.commits];
    f.records.set(`${path}1`, {...original, total_commits: all.length, commits: all.slice(0, 100)});
    f.records.set(`${path}2`, {...original, total_commits: all.length, commits: partial ? [] : all.slice(100)});
    const componentPath = `/compare/${base}...${sha('b')}?per_page=100&page=1`;
    const component = f.records.get(componentPath);
    f.records.set(componentPath, {...component, total_commits: 95, commits: [...history, ...component.commits]});
    const fetchRecord = async (url, options) => {
      const response = await f.fetchRecord(url, options);
      if (url.endsWith(`${path}1`)) response.headers.set('link', '<https://api.github.com/next>; rel="next"');
      return response;
    };
    const selected = githubConsumerSelection('consumer-read', repository, declaration(), fetchRecord);
    if (partial) await assert.rejects(selected, /consumer_composition_graph_unmeasured$/);
    else assert.equal((await selected).headSha, head);
    assert.equal(f.calls.filter(call => call === `${path}2`).length, 1);
  }
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
