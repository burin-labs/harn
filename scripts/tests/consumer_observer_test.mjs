import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import {
  admitChild, admitObserver, githubRecord, observerContext, observerRunName,
  observerDeclarationName, observerRunNameExpression, observerDispatchActor,
  qualifyOwnedChild, githubArtifactCensus, githubChildCensus, reconcileOwnedChildren,
  createTerminalReceipt, qualifyTerminalReceipt, parentHeadSha, observerParentWorkflows, observerRefusal,
  readRetry,
} from '../lib/consumer-observer.mjs';

const context = {
  schema: 'harn.consumer_observer.v2',
  parent_repository: 'burin-labs/harn',
  parent_run_id: '42',
  parent_run_attempt: 2,
  parent_head_sha: 'a'.repeat(40),
  source_revision: 'b'.repeat(40),
  dispatch_nonce: '01234567-89ab-4cde-8fab-0123456789ab',
  started_at: 1000,
};
const parent = {
  id: 42, run_attempt: 2, head_sha: context.parent_head_sha,
  head_repository: { full_name: context.parent_repository },
  path: '.github/workflows/build-release-binaries.yml', status: 'in_progress',
};
const expected = {
  repository: 'acme/widget-host', runId: '73', attempt: 1,
  headSha: 'c'.repeat(40), workflow: '.github/workflows/rehearsal.yml',
  sourceRevision: context.source_revision,
};
const child = {
  id: 73, run_attempt: 1, head_sha: expected.headSha,
  head_repository: { full_name: expected.repository },
  path: expected.workflow, event: 'workflow_dispatch', status: 'in_progress',
  actor: { login: observerDispatchActor, type: 'Bot' },
  display_title: observerRunName(context),
};
const event = { inputs: { observer: JSON.stringify(context), source_revision: context.source_revision } };

test('admission binds an authenticated child to a live current parent and separately frozen source', () => {
  const receipt = admitChild(context, parent, expected, child, event, context);
  assert.equal(receipt.child.run_id, '73');
  assert.match(receipt.child.receipt_nonce,/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.notEqual(receipt.child.receipt_nonce,admitChild(context,parent,expected,child,event,context).child.receipt_nonce);
  assert.deepEqual(receipt.observer, observerContext(context));
  assert.notEqual(receipt.observer.parent_head_sha, receipt.observer.source_revision);
});

test('a pull request parent is identified by its recorded head, not its test merge commit', () => {
  const head = 'a'.repeat(40);
  const merge = 'b'.repeat(40);
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'observer-pr-'));
  const eventPath = path.join(directory, 'event.json');
  fs.writeFileSync(eventPath, JSON.stringify({ pull_request: { head: { sha: head } } }));
  assert.equal(parentHeadSha({ GITHUB_EVENT_NAME: 'pull_request', GITHUB_EVENT_PATH: eventPath, GITHUB_SHA: merge }), head);
  assert.equal(parentHeadSha({ GITHUB_EVENT_NAME: 'schedule', GITHUB_EVENT_PATH: eventPath, GITHUB_SHA: merge }), merge);
  const parent = { id: 42, run_attempt: 2, status: 'in_progress', head_sha: head,
    head_repository: { full_name: 'burin-labs/harn' }, path: observerParentWorkflows.pull_request };
  assert.deepEqual(admitObserver({ ...context, parent_head_sha: head }, parent), { ...context, parent_head_sha: head });
  assert.throws(() => admitObserver({ ...context, parent_head_sha: merge }, parent), /parent_identity_unproven/);
  fs.rmSync(directory, { recursive: true, force: true });
});

test('dead, superseded, malformed, and unrelated parent readings cannot admit work', () => {
  for (const record of [
    null, {}, { ...parent, id: 41 }, { ...parent, head_sha: 'd'.repeat(40) },
    { ...parent, run_attempt: 3 }, { ...parent, status: 'completed' },
    { ...parent, status: undefined }, { ...parent, path: '.github/workflows/unrelated.yml' },
    { ...parent, head_repository: { full_name: 'acme/fork' } },
  ]) assert.throws(() => admitObserver(context, record), /CONSUMER_OBSERVER reason=/);
  assert.throws(() => observerContext({ ...context, extra: true }), /context_shape_invalid/);
  assert.throws(() => observerContext({ ...context, parent_run_id: '' }), /context_identity_invalid/);
});

test('owned receipts remain bound to the historical parent and fresh child attempt', () => {
  const receipt = admitChild(context, parent, expected, child, event, context);
  const historicalParent = { ...parent, status: 'completed', conclusion: 'cancelled' };
  const configured = { repository: expected.repository, workflow: expected.workflow };
  const qualify = (reading = child, owner = receipt, declaration = context, parentReading = historicalParent) =>
    qualifyOwnedChild(owner, context, parentReading, reading, configured, declaration);
  assert.deepEqual(qualify(), receipt);
  assert.deepEqual(qualify({ ...child, status: 'completed', conclusion: 'success' }), receipt);
  assert.throws(() => admitObserver(context, historicalParent), /parent_ended/);
  for (const reading of [
    { ...child, run_attempt: 2 }, { ...child, id: 74 },
    { ...child, head_sha: 'd'.repeat(40) }, { ...child, path: '.github/workflows/unrelated.yml' },
    { ...child, actor: { login: 'unrelated[bot]', type: 'Bot' } },
    { ...child, status: 'completed', conclusion: null }, { ...child, status: 'unknown' },
  ]) assert.throws(() => qualify(reading), /CONSUMER_OBSERVER reason=/);
  assert.throws(() => qualify(child, { ...receipt, reused: true }), /child_receipt_shape_or_context_unproven/);
  for(const receiptNonce of [undefined,null,'',context.parent_run_id]){
    assert.throws(()=>qualify(child,{...receipt,child:{...receipt.child,receipt_nonce:receiptNonce}}),/child_receipt_identity_unproven/);
  }
  assert.throws(() => qualify(child, { ...receipt, child: { ...receipt.child, run_attempt: 2 } }), /child_dispatch_identity_unproven/);
  assert.throws(() => qualify(child, receipt, { ...context, dispatch_nonce: '11234567-89ab-4cde-8fab-0123456789ab' }), /dispatch_declaration_mismatch/);
  assert.throws(() => qualify(child, receipt, context, { ...historicalParent, run_attempt: 3 }), /parent_attempt_superseded/);
});

test('a private product decision requires authenticated custody and fresh completed success', () => {
  const ownership = admitChild(context, parent, expected, child, event, context);
  const configured = { repository: expected.repository, workflow: expected.workflow };
  const product = {
    source_revision: context.source_revision, verdict: 'pass', owed: 2, proven: 2, pending: 0, failed: 0, failed_suites: [],
  };
  const receipt = createTerminalReceipt(ownership, context, parent, child, configured, context, product);
  const historicalParent = { ...parent, status: 'completed', conclusion: 'success' };
  const completed = { ...child, status: 'completed', conclusion: 'success' };
  const qualify = (input = receipt, reading = completed, parentReading = historicalParent, declaration = context) =>
    qualifyTerminalReceipt(input, context, parentReading, reading, configured, declaration);
  assert.deepEqual(qualify(), receipt);
  for (const reading of [
    child, { ...completed, conclusion: 'failure' }, { ...completed, conclusion: 'cancelled' },
    { ...completed, conclusion: 'skipped' }, { ...completed, run_attempt: 2 },
    { ...completed, head_sha: 'd'.repeat(40) },
  ]) assert.throws(() => qualify(receipt, reading), /CONSUMER_OBSERVER reason=/);
  for (const decision of [
    null, {}, { ...product, owed: 0, proven: 0 }, { ...product, proven: 1 },
    { ...product, pending: 1 }, { ...product, failed: 1 }, { ...product, verdict: 'fail' },
    { ...product, source_revision: 'd'.repeat(40) }, { ...product, private_job: 'unowned' },
  ]) assert.throws(() => qualify({ ...receipt, product: decision }), /product_decision_unmeasured/);
  assert.throws(() => qualify({ ...receipt, reused: true }), /terminal_receipt_shape_unproven/);
  assert.throws(() => qualify(receipt, completed, { ...historicalParent, run_attempt: 3 }), /parent_attempt_superseded/);
  assert.throws(() => qualify(receipt, completed, historicalParent,
    { ...context, dispatch_nonce: '11234567-89ab-4cde-8fab-0123456789ab' }), /dispatch_declaration_mismatch/);
});

test('historical parent API reads cannot silently substitute the latest attempt', async () => {
  const reading = { ...parent, status: 'completed', conclusion: 'cancelled' };
  const requested = [];
  const fetchRecord = async url => {
    requested.push(url);
    return { ok: true, json: async () => reading };
  };
  assert.deepEqual(await githubRecord('fixture-read', context.parent_repository, '42', fetchRecord, 2), reading);
  assert.equal(requested[0], 'https://api.github.com/repos/burin-labs/harn/actions/runs/42/attempts/2');
  await assert.rejects(() => githubRecord('fixture-read', context.parent_repository, '42', fetchRecord, 1), /run_read_unmeasured/);
  await assert.rejects(() => githubRecord('fixture-read', context.parent_repository, '42', fetchRecord, 0), /read_authority_or_identity_missing/);
});

test('artifact discovery measures complete pages and refuses false zero or partial custody', async () => {
  const artifacts = Array.from({ length: 101 }, (_, index) => ({ id: index + 1 }));
  const calls = [];
  const read = pages => githubArtifactCensus('fixture-read', 'acme/widget-host', '73', async url => {
    calls.push(url);
    return { ok: true, json: async () => pages[Number(new URL(url).searchParams.get('page')) - 1] };
  });
  assert.deepEqual(await read([{ total_count: 0, artifacts: [] }]), []);
  assert.deepEqual(await read([
    { total_count: 101, artifacts: artifacts.slice(0, 100) },
    { total_count: 101, artifacts: artifacts.slice(100) },
  ]), artifacts);
  assert.ok(calls.at(-1).endsWith('per_page=100&page=2'));
  for (const pages of [
    [{}], [{ total_count: 1, artifacts: [] }], [{ total_count: 1001, artifacts: [] }],
    [{ total_count: 2, artifacts: [{ id: 1 }, { id: 1 }] }],
    [{ total_count: 101, artifacts: artifacts.slice(0, 100) }, { total_count: 100, artifacts: [] }],
    [{ total_count: 101, artifacts: artifacts.slice(0, 100) }, { total_count: 101, artifacts: [] }],
  ]) await assert.rejects(() => read(pages), /observer_census_unmeasured/);
});

test('a refused census read carries status and rate limit, retries one exhausted limit, and prints no endpoint', async () => {
  const headers = values => ({ get: name => values[name] });
  const refused = (status, values) => ({ ok: false, status, headers: headers(values) });
  const read = fetchRecord => githubArtifactCensus('fixture-read', 'acme/widget-host', '73', fetchRecord);
  const error = await read(async () => refused(403, {
    'x-ratelimit-remaining': '0', 'x-ratelimit-limit': '5000', 'x-ratelimit-resource': 'core',
    'x-ratelimit-reset': '4102444800',
  })).then(() => assert.fail('a refused read must reject'), failure => failure);
  assert.equal(error.message, 'CONSUMER_OBSERVER reason=observer_census_read_refused');
  const { reason, line } = observerRefusal(error, 'job_timing_unmeasured');
  assert.equal(reason, 'observer_census_read_refused');
  assert.equal(line, 'CONSUMER_OBSERVER reason=observer_census_read_refused status=403 '
    + 'ratelimit_remaining=0 ratelimit_limit=5000 ratelimit_reset=4102444800 ratelimit_resource=core');
  assert.ok(!/widget-host|acme|73/.test(line), 'the public line never names the consumer repository or run');
  const forged = Object.assign(new Error('x'), { diagnostic: { status: 'acme/widget-host', ratelimit_resource: 'a b' } });
  assert.equal(observerRefusal(forged, 'fallback_reason').line, 'CONSUMER_OBSERVER reason=fallback_reason');
  let reads = 0;
  assert.deepEqual(await read(async () => {
    reads += 1;
    if (reads === 1) return refused(429, { 'x-ratelimit-remaining': '0', 'x-ratelimit-reset': '1' });
    return { ok: true, json: async () => ({ total_count: 0, artifacts: [] }) };
  }), []);
  assert.equal(reads, 2);
  reads = 0;
  await assert.rejects(() => read(async () => { reads += 1; return refused(403, { 'x-ratelimit-remaining': '7' }); }),
    /observer_census_read_refused/);
  assert.equal(reads, 1, 'a refusal with budget left is not a rate limit and is not retried');
});

test('a run read retries a rate limit, a server error or a dropped connection, and fails a denial at once', async () => {
  const backoff = readRetry.backoffMs;
  readRetry.backoffMs = 0;
  try {
    const headers = values => ({ get: name => values[name] });
    const refused = (status, values = {}, body = { message: 'Resource not accessible by integration' }) =>
      ({ ok: false, status, headers: headers(values), clone: () => ({ json: async () => body }) });
    const secondary = { message: 'You have exceeded a secondary rate limit. Please wait a few minutes before you try again.' };
    const run = { id: 73, status: 'in_progress', run_attempt: 1 };
    const read = replies => {
      const seen = [];
      const fetchRecord = async () => {
        const reply = replies[seen.length];
        seen.push(reply);
        if (reply instanceof Error) throw reply;
        return reply ?? { ok: true, json: async () => run };
      };
      return { seen, result: githubRecord('fixture-read', 'acme/widget-host', '73', fetchRecord) };
    };
    for (const replies of [
      [refused(403, { 'retry-after': '0' }), refused(502), new Error('socket hang up')],
      [refused(429, { 'x-ratelimit-remaining': '0', 'x-ratelimit-reset': '1' })],
      [refused(403, { 'x-ratelimit-remaining': '4000' }, secondary)],
    ]) {
      const transient = read(replies);
      assert.equal((await transient.result).id, 73);
      assert.equal(transient.seen.length, replies.length + 1, 'each refusal is retried, then the read measures');
    }
    for (const status of [401, 403, 404]) {
      const denied = read([refused(status, { 'x-ratelimit-remaining': '4000' })]);
      const error = await denied.result.then(() => assert.fail('a denial must reject'), failure => failure);
      assert.equal(denied.seen.length, 1, `a ${status} denial is not retried`);
      assert.equal(observerRefusal(error, 'fallback').line,
        `CONSUMER_OBSERVER reason=run_read_refused status=${status} ratelimit_remaining=4000`);
    }
    const exhausted = read([refused(503), refused(503), refused(503), refused(503), refused(503)]);
    await assert.rejects(exhausted.result, /run_read_refused/);
    assert.equal(exhausted.seen.length, readRetry.attempts, 'retries stop at the bound');
    const distant = read([refused(403, { 'x-ratelimit-remaining': '0', 'x-ratelimit-reset': '4102444800' })]);
    await assert.rejects(distant.result, /run_read_refused/);
    assert.equal(distant.seen.length, 1, 'a reset beyond the wait bound is not slept on');
  } finally {
    readRetry.backoffMs = backoff;
  }
});

test('every observer entrypoint prints its failure through the one public refusal owner', () => {
  const directory = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'ci');
  for (const name of fs.readdirSync(directory).filter(entry => /^consumer-observer-.*\.mjs$/.test(entry))) {
    const text = fs.readFileSync(path.join(directory, name), 'utf8');
    assert.ok(!/exec\(error\.message\)/.test(text), `${name} parses the refusal message itself`);
  }
});

test('child discovery keeps a complete bounded census and never treats run names as authority', async () => {
  let endpoint;
  const read = async url => {
    endpoint = new URL(url);
    return { ok: true, json: async () => ({ total_count: 1, workflow_runs: [child] }) };
  };
  assert.deepEqual(await githubChildCensus('fixture-read', expected.repository,
    expected.workflow, '2026-10-11T20:00:00Z', read), [child]);
  assert.equal(endpoint.pathname, '/repos/acme/widget-host/actions/workflows/rehearsal.yml/runs');
  assert.equal(endpoint.searchParams.get('created'), '>=2026-10-11T20:00:00Z');
  assert.equal(endpoint.searchParams.get('event'), 'workflow_dispatch');
  await assert.rejects(() => githubChildCensus('fixture-read', expected.repository,
    '../unrelated.yml', '2026-10-11T20:00:00Z', read), /child_discovery_identity_invalid/);
});

test('the same reconciler cancels only receipt-owned live children of an ended exact parent', async () => {
  const receipt = admitChild(context, parent, expected, child, event, context);
  const endedParent = { ...parent, status: 'completed', conclusion: 'cancelled', created_at: '2026-10-11T20:00:00Z' };
  const bytes = Buffer.from('authenticated-fixture-archive');
  const digest = `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
  const metadata = (artifactId, name, runId, headSha) => ({
    id: artifactId, name, expired: false, digest, size_in_bytes: bytes.length,
    workflow_run: { id: Number(runId), head_sha: headSha },
  });
  const configured = { repository: expected.repository, workflow: expected.workflow };
  const run = async ({ latest = child, parentReading = endedParent, missing = false, runs = [child], archiveReceipt = receipt,
    settlesAfter = 0, appeared = [], now = 1100, jobFailure = false } = {}) => {
    const calls = [];
    const waits = [];
    let cancelled = false;
    let cancellationRead = 0;
    let discoveries = 0;
    const fetchRecord = async (url, options = {}) => {
      calls.push({ url, method: options.method ?? 'GET' });
      const endpoint = new URL(url);
      if (endpoint.pathname.endsWith('/cancel')) {
        cancelled = true;
        return { status: 202 };
      }
      if (endpoint.pathname.endsWith('/runs/42/attempts/2')) return { ok: true, json: async () => parentReading };
      if (endpoint.pathname.endsWith('/runs/42/artifacts')) return { ok: true, json: async () => ({
        total_count: 1, artifacts: [metadata(501, observerDeclarationName(context), '42', context.parent_head_sha)],
      }) };
      if (endpoint.pathname.endsWith('/rehearsal.yml/runs')) {
        const discovered = ++discoveries > 1 ? [...runs, ...appeared] : runs;
        return { ok: true, json: async () => ({ total_count: discovered.length, workflow_runs: discovered }) };
      }
      if (endpoint.pathname.endsWith('/runs/73/artifacts')) return { ok: true, json: async () => ({
        total_count: missing ? 0 : 1,
        artifacts: missing ? [] : [metadata(502, `consumer-observer-${context.dispatch_nonce}`, '73', child.head_sha)],
      }) };
      if (endpoint.pathname.endsWith('/runs/73/attempts/1/jobs')) {
        if (jobFailure) return {ok:false,status:401};
        return {ok:true,json:async()=>({total_count:1,jobs:[{
          id:91,run_id:73,run_attempt:1,head_sha:child.head_sha,status:'in_progress',conclusion:null,
          created_at:new Date(1000*1000).toISOString(),started_at:new Date(1010*1000).toISOString(),
          completed_at:null,runner_id:1,runner_name:'private-worker',
          steps:[{status:'in_progress',conclusion:null,started_at:new Date(1010*1000).toISOString()}],
        }]})};
      }
      if (endpoint.pathname.endsWith('/runs/73')) return { ok: true, json: async () => cancelled && ++cancellationRead > settlesAfter
        ? { ...latest, status: 'completed', conclusion: 'cancelled' } : latest };
      if (endpoint.pathname.endsWith('/zip')) return { ok: true, arrayBuffer: async () => bytes };
      throw new Error('unexpected authenticated fixture request');
    };
    const result = await reconcileOwnedChildren('fixture-read', context, configured, fetchRecord,
      (_, filename) => filename === 'consumer-observer.json' ? context : archiveReceipt,
      async milliseconds => { waits.push(milliseconds); }, () => now);
    return { result, calls, waits };
  };
  const positive = await run();
  assert.deepEqual(positive.result, { discovered: 1, owned: 1, terminal: 1, cancelled: 1, pending: 0, refused: 0, retained: 0 });
  assert.equal(positive.calls.filter(call => call.method === 'POST').length, 1);
  for (const control of [
    { missing: true }, { latest: { ...child, run_attempt: 2 } },
    { latest: { ...child, actor: { login: 'unrelated[bot]', type: 'Bot' } } },
    { archiveReceipt: { ...receipt, child: { ...receipt.child, head_sha: 'd'.repeat(40) } } },
    { parentReading: { ...endedParent, status: 'in_progress', conclusion: null } },
    { runs: [] }, { runs: [{ ...child, display_title: 'ordinary successful rehearsal' }] },
  ]) {
    const negative = await run(control);
    assert.equal(negative.calls.filter(call => call.method === 'POST').length, 0);
    assert.ok(negative.result.pending + negative.result.refused > 0);
  }
  const complete = await run({ latest: { ...child, status: 'completed', conclusion: 'success' } });
  assert.equal(complete.result.terminal, 1);
  assert.equal(complete.calls.filter(call => call.method === 'POST').length, 0);
  const failedObserver = {...endedParent,conclusion:'failure'};
  const preserved = await run({parentReading:failedObserver});
  assert.deepEqual(preserved.result,{discovered:1,owned:1,terminal:0,cancelled:0,pending:1,refused:0,retained:1});
  assert.equal(preserved.calls.filter(call => call.method === 'POST').length,0,
    'an ended failed observer cannot discard a live measurement within its budget');
  const expired = await run({parentReading:failedObserver,now:8210});
  assert.equal(expired.result.cancelled,1,'fresh job execution exceeding the original budget permits cleanup');
  const incomplete = await run({parentReading:failedObserver,jobFailure:true});
  assert.equal(incomplete.calls.filter(call => call.method === 'POST').length,0,'an unreadable timeout census grants no cancellation');
  assert.equal(incomplete.result.refused,1);
  const settling = await run({ settlesAfter: 2 });
  assert.equal(settling.result.terminal, 1);
  assert.deepEqual(settling.waits, [5000, 5000]);
  const unended = await run({ settlesAfter: 10 });
  assert.equal(unended.result.cancelled, 1, 'accepted cancellation remains separate from termination');
  assert.equal(unended.result.terminal, 0);
  assert.equal(unended.result.pending, 1);
  assert.deepEqual(unended.waits, [5000, 5000, 5000, 5000, 5000], 'bounded observation does not use real-time sleeps in tests');
  const late = await run({ appeared: [{ ...child, id: 74 }] });
  assert.equal(late.result.discovered, 2);
  assert.equal(late.result.pending, 1, 'a dispatch appearing during cleanup cannot be omitted from the verdict');
  assert.equal(late.calls.filter(call => call.method === 'POST').length, 1, 'late discovery alone cannot grant cancellation authority');
});

test('a matching run name cannot substitute for dispatch custody', () => {
  for (const record of [
    { ...child, id: 74 }, { ...child, path: '.github/workflows/other.yml' },
    { ...child, event: 'pull_request' }, { ...child, head_sha: 'd'.repeat(40) },
    { ...child, actor: { login: 'unrelated[bot]', type: 'Bot' } },
    { ...child, actor: { login: observerDispatchActor, type: 'User' } },
    { ...child, run_attempt: 2 },
  ]) assert.throws(() => admitChild(context, parent, expected, record, event, context), /child_dispatch_identity_unproven/);
  assert.throws(() => admitChild(context, parent, expected, child, {
    inputs: { ...event.inputs, source_revision: 'd'.repeat(40) },
  }, context), /child_dispatch_identity_unproven/);
  assert.throws(() => admitChild(context, parent, expected, child, {
    inputs: { ...event.inputs, observer: JSON.stringify({ ...context, parent_head_sha: 'd'.repeat(40) }) },
  }, context), /child_dispatch_identity_unproven/);
  for (const status of ['completed', 'unknown', undefined]) {
    assert.throws(() => admitChild(context, parent, expected, { ...child, status }, event, context), /child_state_unmeasured_or_ended/);
  }
});

test('workflow run-name projection agrees mechanically with the owning renderer', () => {
  const fields = [...observerRunNameExpression.matchAll(/fromJSON\(inputs.observer\)\.([a-z_]+)/g)]
    .map(match => match[1]);
  const template = /^format\('([^']+)'/.exec(observerRunNameExpression)[1];
  const projected = template.replace(/\{([0-9]+)\}/g, (_, index) => context[fields[Number(index)]]);
  assert.equal(projected, observerRunName(context));
  assert.ok(projected.length < 255);
  const documentation = fs.readFileSync(new URL('../../docs/src/dev/consumer-observer.md', import.meta.url), 'utf8');
  assert.ok(documentation.includes(observerRunNameExpression), 'documented caller projection must match the owner');
});

test('missing authenticated API records are unknown, while a nonempty reading reaches admission', async () => {
  const response = record => async () => ({ ok: true, json: async () => record });
  assert.deepEqual(await githubRecord('fixture-token', context.parent_repository, '42', response(parent)), parent);
  for (const record of [null, {}, { ...parent, id: 43 }, { ...parent, run_attempt: undefined }]) {
    await assert.rejects(githubRecord('fixture-token', context.parent_repository, '42', response(record)), /run_read_unmeasured/);
  }
  await assert.rejects(githubRecord('', context.parent_repository, '42', response(parent)), /read_authority_or_identity_missing/);
  await assert.rejects(githubRecord('fixture-token', context.parent_repository, '42', async () => ({ ok: false })), /run_read_refused/);
});

test('the action entrypoint reaches private receipt creation and refuses dead parents without publishing identities', () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-observer-entry-'));
  try {
    const output = path.join(scratch, 'output');
    const eventFile = path.join(scratch, 'event.json');
    const preload = path.join(scratch, 'fetch.mjs');
    const parentFile = path.join(scratch, 'parent.json');
    const childFile = path.join(scratch, 'child.json');
    const censusFile = path.join(scratch, 'census.json');
    const declarationFile = path.join(scratch, 'consumer-observer.json');
    const archiveFile = path.join(scratch, 'declaration.zip');
    fs.writeFileSync(declarationFile, JSON.stringify(context));
    const zipped = spawnSync('zip', ['-j', archiveFile, declarationFile], {
      env: { PATH: process.env.PATH, LC_ALL: 'C' }, encoding: 'utf8',
    });
    assert.equal(zipped.status, 0, zipped.stderr);
    const archiveBytes = fs.readFileSync(archiveFile);
    const census = { total_count: 1, artifacts: [{
      id: 11, name: observerDeclarationName(context), expired: false,
      workflow_run: { id: 42, head_sha: context.parent_head_sha },
      size_in_bytes: archiveBytes.length,
      digest: `sha256:${createHash('sha256').update(archiveBytes).digest('hex')}`,
    }] };
    fs.writeFileSync(censusFile, JSON.stringify(census));
    fs.writeFileSync(eventFile, JSON.stringify(event));
    fs.writeFileSync(preload, `import fs from 'node:fs';
      let parentReads = 0;
      function parentRead() {
        const record = JSON.parse(fs.readFileSync(${JSON.stringify(parentFile)}, 'utf8'));
        return record.fixture_end_after_first_read && ++parentReads > 1
          ? {...record, status: 'completed'} : record;
      }
      function artifactRead(url) {
        const name = new URL(url).searchParams.get('name');
        if (!name) {
          return {total_count: 101, artifacts: Array.from({length: 100}, (_, index) =>
            ({id: index + 100, name: 'unrelated-build-artifact-' + index}))};
        }
        if (name !== ${JSON.stringify(observerDeclarationName(context))}) {
          return {total_count: 0, artifacts: []};
        }
        return JSON.parse(fs.readFileSync(${JSON.stringify(censusFile)}, 'utf8'));
      }
      globalThis.fetch = async url => ({ok: true,
        arrayBuffer: async () => fs.readFileSync(${JSON.stringify(archiveFile)}),
        json: async () => url.includes('/artifacts?')
          ? artifactRead(url)
          : url.endsWith('/42') ? parentRead()
          : JSON.parse(fs.readFileSync(${JSON.stringify(childFile)}, 'utf8'))});`);
    const env = {
      PATH: process.env.PATH,
      NODE_OPTIONS: `--import=${preload}`,
      OBSERVER_CONTEXT: JSON.stringify(context),
      GH_TOKEN: 'fixture-token',
      GITHUB_OUTPUT: output,
      GITHUB_EVENT_PATH: eventFile,
      GITHUB_REPOSITORY: expected.repository,
      GITHUB_RUN_ID: expected.runId,
      GITHUB_RUN_ATTEMPT: String(expected.attempt),
      GITHUB_SHA: expected.headSha,
      GITHUB_WORKFLOW_REF: `${expected.repository}/${expected.workflow}@refs/heads/main`,
      SOURCE_REVISION: expected.sourceRevision,
      RUNNER_TEMP: scratch,
    };
    const entry = fileURLToPath(new URL('../ci/consumer-observer-admit.mjs', import.meta.url));
    fs.writeFileSync(parentFile, JSON.stringify(parent));
    fs.writeFileSync(childFile, JSON.stringify(child));
    const admitted = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
    assert.equal(admitted.status, 0, admitted.stderr);
    assert.match(fs.readFileSync(output, 'utf8'), /^owned=true\n/);
    const receiptFile = path.join(scratch, `consumer-observer-${context.dispatch_nonce}.json`);
    assert.equal(JSON.parse(fs.readFileSync(receiptFile, 'utf8')).child.run_id, expected.runId);
    assert.equal(fs.statSync(receiptFile).mode & 0o777, 0o600);
    assert.doesNotMatch(admitted.stdout + admitted.stderr, /widget-host|run_id|73/);

    fs.rmSync(receiptFile);
    fs.writeFileSync(output, '');
    fs.writeFileSync(childFile, JSON.stringify({ ...child, actor: { login: 'unrelated[bot]', type: 'Bot' } }));
    const forgedActor = spawnSync(process.execPath, [entry], {
      env: { ...env, OBSERVER_DISPATCH_ACTOR: 'unrelated[bot]' }, encoding: 'utf8',
    });
    assert.equal(forgedActor.status, 1);
    assert.match(forgedActor.stderr, /reason=child_dispatch_identity_unproven/);
    assert.equal(fs.existsSync(receiptFile), false);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    fs.writeFileSync(childFile, JSON.stringify(child));
    fs.writeFileSync(censusFile, JSON.stringify({ total_count: 0, artifacts: [] }));
    const unissued = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
    assert.equal(unissued.status, 1);
    assert.match(unissued.stderr, /reason=dispatch_declaration_missing_or_ambiguous/);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    assert.equal(fs.existsSync(receiptFile), false);
    fs.writeFileSync(censusFile, JSON.stringify(census));
    const forged = { ...context, dispatch_nonce: '11234567-89ab-4cde-8fab-0123456789ab' };
    const forgedAttempt = spawnSync(process.execPath, [entry], {
      env: { ...env, OBSERVER_CONTEXT: JSON.stringify(forged) }, encoding: 'utf8',
    });
    assert.equal(forgedAttempt.status, 1);
    assert.match(forgedAttempt.stderr, /reason=dispatch_declaration_missing_or_ambiguous/);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    for (const [reading, reason] of [
      [{ total_count: 2, artifacts: [census.artifacts[0], { ...census.artifacts[0], id: 12 }] }, 'missing_or_ambiguous'],
      [{ ...census, total_count: 2 }, 'census_unmeasured'],
      [{ ...census, artifacts: [{ ...census.artifacts[0], expired: true }] }, 'identity_unproven'],
      [{ ...census, artifacts: [{ ...census.artifacts[0], size_in_bytes: 0 }] }, 'identity_unproven'],
      [{ ...census, artifacts: [{ ...census.artifacts[0], size_in_bytes: -1 }] }, 'identity_unproven'],
      [{ ...census, artifacts: [{ ...census.artifacts[0], digest: `sha256:${'0'.repeat(64)}` }] }, 'digest_mismatch'],
    ]) {
      fs.writeFileSync(censusFile, JSON.stringify(reading));
      const refused = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
      assert.equal(refused.status, 1);
      const diagnostic = reason === 'census_unmeasured' ? 'observer_census_unmeasured' : `dispatch_declaration_${reason}`;
      assert.match(refused.stderr, new RegExp(`reason=${diagnostic}`));
      assert.equal(fs.readFileSync(output, 'utf8'), '');
      assert.equal(fs.existsSync(receiptFile), false);
    }
    fs.writeFileSync(censusFile, JSON.stringify(census));
    fs.writeFileSync(parentFile, JSON.stringify({ ...parent, status: 'completed' }));
    const abandoned = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
    assert.equal(abandoned.status, 1);
    assert.match(abandoned.stderr, /reason=parent_ended/);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    assert.equal(fs.existsSync(receiptFile), false);

    fs.writeFileSync(parentFile, JSON.stringify({ ...parent, fixture_end_after_first_read: true }));
    const endedDuringRead = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
    assert.equal(endedDuringRead.status, 1);
    assert.match(endedDuringRead.stderr, /reason=parent_ended/);
    assert.equal(fs.readFileSync(output, 'utf8'), '');
    assert.equal(fs.existsSync(receiptFile), false);

    const ordinary = spawnSync(process.execPath, [entry], {
      env: { PATH: process.env.PATH, GITHUB_OUTPUT: output }, encoding: 'utf8',
    });
    assert.equal(ordinary.status, 0, ordinary.stderr);
    assert.equal(fs.readFileSync(output, 'utf8'), 'owned=false\n');
  } finally {
    fs.rmSync(scratch, { recursive: true });
  }
});
