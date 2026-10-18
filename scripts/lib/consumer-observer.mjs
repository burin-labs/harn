import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { setTimeout as pause } from 'node:timers/promises';

const parentRepository = 'burin-labs/harn';
export const observerDispatchActor = 'harn-release-bot[bot]';
// Workflow projections use this exact publication-step identity. Reusable
// callers prefix job names, but GitHub retains the owned step name unchanged.
export const observerDeclarationStep = 'Publish consumer observer declaration';
export const observerParentWorkflows = Object.freeze({
  canary:'.github/workflows/consumer-canary.yml',
  candidate:'.github/workflows/build-release-binaries.yml',
  promotion:'.github/workflows/promote-release.yml',
  pull_request:'.github/workflows/ci.yml',
});
const parentWorkflows = new Set(Object.values(observerParentWorkflows));
const sha = /^[0-9a-f]{40}$/;
const id = /^[1-9][0-9]*$/;
const nonce = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const liveStates = new Set(['queued', 'requested', 'waiting', 'pending', 'in_progress']);
const completedConclusions = new Set(['success', 'failure', 'cancelled', 'timed_out', 'action_required', 'neutral', 'skipped', 'stale']);
// Refuse an unqualified large census rather than spend unbounded API calls.
const censusLimit = 1000;
// A cancellation request is asynchronous. Bound completion observation to six
// authenticated reads (each already bounded to 30s), with 5s between them.
const cancellationReads = 6;
const cancellationPollMs = 5000;
const contextKeys = [
  'dispatch_nonce', 'parent_head_sha', 'parent_repository', 'parent_run_attempt',
  'parent_run_id', 'schema', 'source_revision', 'started_at',
];

/** @typedef {{schema: string, parent_repository: string, parent_run_id: string,
 * parent_run_attempt: number, parent_head_sha: string, source_revision: string,
 * dispatch_nonce: string, started_at: number}} ObserverContext */

/** @typedef {{source_revision: string, verdict: 'pass'|'fail', owed: number,
 * proven: number, pending: 0, failed: number, failed_suites: string[]}} ProductDecision */

function refuse(reason) {
  throw new Error(`CONSUMER_OBSERVER reason=${reason}`);
}

/** Normalize once. Unknown fields cannot silently alter the ownership contract.
 * @param {unknown} value
 * @returns {ObserverContext} */
export function observerContext(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)
    || Object.keys(value).sort().join(',') !== contextKeys.join(',')) {
    refuse('context_shape_invalid');
  }
  const context = /** @type {ObserverContext} */ (value);
  if (context.schema !== 'harn.consumer_observer.v2'
    || context.parent_repository !== parentRepository
    || typeof context.parent_run_id !== 'string' || !id.test(context.parent_run_id)
    || !Number.isSafeInteger(context.parent_run_attempt) || context.parent_run_attempt < 1
    || typeof context.parent_head_sha !== 'string' || !sha.test(context.parent_head_sha)
    || typeof context.source_revision !== 'string' || !sha.test(context.source_revision)
    || typeof context.dispatch_nonce !== 'string' || !nonce.test(context.dispatch_nonce)
    || !Number.isSafeInteger(context.started_at) || context.started_at < 1) {
    refuse('context_identity_invalid');
  }
  return Object.fromEntries(contextKeys.map(key => [key, context[key]]));
}

/** The head commit GitHub records on this run. A pull_request run records the
 * pull request's head while GITHUB_SHA names its test merge commit. */
export function parentHeadSha(env) {
  if (env.GITHUB_EVENT_NAME !== 'pull_request') return env.GITHUB_SHA;
  return JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, 'utf8')).pull_request?.head?.sha;
}

/** @param {{runId: string, attempt: number, headSha: string, sourceRevision: string}} parent */
export function createObserverContext(parent) {
  return observerContext({
    schema: 'harn.consumer_observer.v2',
    parent_repository: parentRepository,
    parent_run_id: parent.runId,
    parent_run_attempt: parent.attempt,
    parent_head_sha: parent.headSha,
    source_revision: parent.sourceRevision,
    dispatch_nonce: randomUUID(),
    // The original declaration owns one clock. Credential renewal cannot mint
    // or backdate a second start; declaration qualification/upload is charged.
    started_at: Math.floor(Date.now() / 1000),
  });
}

/** A display name is a discovery projection, never cancellation authority. */
export function observerRunName(context) {
  const observer = observerContext(context);
  return `Consumer observer ${observer.parent_run_id}.${observer.parent_run_attempt}/${observer.dispatch_nonce} @ ${observer.source_revision}`;
}

// GitHub chooses the run name before a runner can execute this owner. The
// consumer checks this projection mechanically against the owner contract.
export const observerRunNameExpression = "format('Consumer observer {0}.{1}/{2} @ {3}', fromJSON(inputs.observer).parent_run_id, fromJSON(inputs.observer).parent_run_attempt, fromJSON(inputs.observer).dispatch_nonce, fromJSON(inputs.observer).source_revision)";

/** Admission checks authenticated parent metadata before costly child work.
 * The workflow source and rehearsed source are distinct immutable identities.
 * @param {ObserverContext} input
 * @param {object} parent
 * @returns {ObserverContext} */
export function qualifyObserverParent(input, parent) {
  const context = observerContext(input);
  if (!parent || String(parent.id) !== context.parent_run_id
    || parent.head_repository?.full_name !== context.parent_repository
    || parent.head_sha !== context.parent_head_sha
    || !parentWorkflows.has(parent.path)) {
    refuse('parent_identity_unproven');
  }
  if (parent.run_attempt !== context.parent_run_attempt) refuse('parent_attempt_superseded');
  return context;
}

/** Admission requires the latest live attempt. Cleanup qualifies the exact
 * historical attempt with the same identity owner before applying its policy. */
export function admitObserver(input, parent) {
  const context = qualifyObserverParent(input, parent);
  if (!liveStates.has(parent.status)) {
    if (parent.status === 'completed') refuse('parent_ended');
    refuse('parent_state_unmeasured');
  }
  return context;
}

function declaredObserver(context, declaration) {
  const observer = observerContext(context);
  if (JSON.stringify(observerContext(declaration)) !== JSON.stringify(observer)) {
    refuse('dispatch_declaration_mismatch');
  }
  return observer;
}

function qualifyChildRun(observer, expected, child) {
  if (String(child?.id) !== expected.runId || child.run_attempt !== expected.attempt
    || child.head_sha !== expected.headSha || child.head_repository?.full_name !== expected.repository
    || child.path !== expected.workflow || child.event !== 'workflow_dispatch'
    || child.actor?.login !== observerDispatchActor || child.actor?.type !== 'Bot'
    || child.display_title !== observerRunName(observer)) {
    refuse('child_dispatch_identity_unproven');
  }
  if (!liveStates.has(child.status) && (child.status !== 'completed'
    || !completedConclusions.has(child.conclusion))) {
    refuse('child_state_unmeasured');
  }
}

/** Authenticate a child-owned receipt without granting authority from a name.
 * @param {unknown} input
 * @param {ObserverContext} context
 * @param {object} parent Exact historical parent attempt, not a newer rerun
 * @param {object} child Fresh authenticated child run
 * @param {{repository: string, workflow: string}} expected
 * @param {ObserverContext} declaration Authenticated exact parent artifact */
export function qualifyOwnedChild(input, context, parent, child, expected, declaration) {
  const observer = declaredObserver(qualifyObserverParent(context, parent), declaration);
  if (!input || typeof input !== 'object' || Array.isArray(input)
    || Object.keys(input).sort().join(',') !== 'child,observer,schema'
    || input.schema !== 'harn.consumer_observer_receipt.v2'
    || JSON.stringify(observerContext(input.observer)) !== JSON.stringify(observer)) {
    refuse('child_receipt_shape_or_context_unproven');
  }
  const receipt = input.child;
  if (!receipt || typeof receipt !== 'object' || Array.isArray(receipt)
    || Object.keys(receipt).sort().join(',') !== 'dispatch_actor,head_sha,receipt_nonce,repository,run_attempt,run_id,workflow'
    || receipt.repository !== expected.repository || receipt.workflow !== expected.workflow
    || typeof receipt.run_id !== 'string' || !id.test(receipt.run_id)
    || !Number.isSafeInteger(receipt.run_attempt) || receipt.run_attempt < 1
    || typeof receipt.head_sha !== 'string' || !sha.test(receipt.head_sha)
    || receipt.dispatch_actor !== observerDispatchActor
    || typeof receipt.receipt_nonce !== 'string' || !nonce.test(receipt.receipt_nonce)) {
    refuse('child_receipt_identity_unproven');
  }
  qualifyChildRun(observer, {
    repository: receipt.repository, runId: receipt.run_id, attempt: receipt.run_attempt,
    headSha: receipt.head_sha, workflow: receipt.workflow,
  }, child);
  if ((expected.headSha !== undefined && (!sha.test(expected.headSha) || child.head_sha !== expected.headSha))
    || (expected.branch !== undefined && (typeof expected.branch !== 'string' || !expected.branch
      || child.head_branch !== expected.branch))) {
    refuse('child_source_no_longer_eligible');
  }
  return { schema: input.schema, observer, child: {
    repository:receipt.repository, workflow:receipt.workflow,
    run_id:receipt.run_id, run_attempt:receipt.run_attempt,
    head_sha:receipt.head_sha, dispatch_actor:receipt.dispatch_actor, receipt_nonce:receipt.receipt_nonce,
  } };
}

/** Only normalized authenticated custody reaches this private commitment.
 * Its admission-owned random nonce prevents guessing private run identities. */
function custodyCommitment(receipt) {
  return createHash('sha256').update(JSON.stringify(receipt)).digest('hex');
}

/** The consumer's canonical decision owns its proof counts. This transport
 * validates a reached, complete decision without knowing private job names.
 * @param {unknown} input
 * @param {ObserverContext} context
 * @returns {ProductDecision} */
function terminalProduct(input, context) {
  if (!input || typeof input !== 'object' || Array.isArray(input)
    || Object.keys(input).sort().join(',') !== 'failed,failed_suites,owed,pending,proven,source_revision,verdict') {
    refuse('product_decision_unmeasured');
  }
  const product = /** @type {ProductDecision} */ (input);
  if (product.source_revision !== context.source_revision || !['pass','fail'].includes(product.verdict)
    || !Number.isSafeInteger(product.owed) || product.owed < 1
    || !Number.isSafeInteger(product.proven) || product.proven < 0
    || !Number.isSafeInteger(product.failed) || product.failed < 0
    || product.proven + product.failed !== product.owed || product.pending !== 0
    || !Array.isArray(product.failed_suites)
    || product.failed_suites.some(suite => typeof suite !== 'string' || !/^[a-z][a-z0-9-]*$/.test(suite))
    || new Set(product.failed_suites).size !== product.failed_suites.length
    || product.failed_suites.length !== product.failed
    || (product.verdict === 'pass' ? product.failed !== 0 : product.failed === 0)) {
    refuse('product_decision_unmeasured');
  }
  return {
    source_revision: product.source_revision, verdict: product.verdict,
    owed: product.owed, proven: product.proven, pending: 0, failed: product.failed,
    failed_suites: [...product.failed_suites].sort(),
  };
}

/** A private producer records the actual canonical product decision while
 * its job is live. Recording it does not authorize a completed verdict. */
export function createTerminalReceipt(ownership, context, parent, child, expected, declaration, product) {
  const receipt = qualifyOwnedChild(ownership, context, parent, child, expected, declaration);
  return {
    schema: 'harn.consumer_observer_terminal.v2', observer: receipt.observer,
    child: receipt.child, product: terminalProduct(product, receipt.observer),
  };
}

/** A fresh completed run must agree with the reached private decision.
 * Execution failure alone never manufactures a reached product failure. */
export function qualifyTerminalReceipt(input, context, parent, child, expected, declaration) {
  if (!input || typeof input !== 'object' || Array.isArray(input)
    || Object.keys(input).sort().join(',') !== 'child,observer,product,schema'
    || input.schema !== 'harn.consumer_observer_terminal.v2') {
    refuse('terminal_receipt_shape_unproven');
  }
  const ownership = {
    schema: 'harn.consumer_observer_receipt.v2', observer: input.observer, child: input.child,
  };
  const receipt = createTerminalReceipt(ownership, context, parent, child, expected, declaration, input.product);
  const conclusion = receipt.product.verdict === 'pass' ? 'success' : 'failure';
  if (child.status !== 'completed' || child.conclusion !== conclusion) refuse('product_run_conclusion_unproven');
  return receipt;
}

/** Bind admission to the child's actual event and authenticated run metadata.
 * Rehearsals with no observer remain ordinary, unowned runs.
 * @param {ObserverContext} context
 * @param {object} parent
 * @param {{repository: string, runId: string, attempt: number, headSha: string,
 * workflow: string, sourceRevision: string}} expected
 * @param {object} child
 * @param {object} event */
export function admitChild(context, parent, expected, child, event, declaration) {
  const observer = declaredObserver(admitObserver(context, parent), declaration);
  if (!liveStates.has(child?.status)) refuse('child_state_unmeasured_or_ended');
  qualifyChildRun(observer, expected, child);
  if (expected.sourceRevision !== observer.source_revision
    || event?.inputs?.source_revision !== observer.source_revision
    || JSON.stringify(observerContext(JSON.parse(event?.inputs?.observer ?? 'null'))) !== JSON.stringify(observer)) {
    refuse('child_dispatch_identity_unproven');
  }
  return {
    schema: 'harn.consumer_observer_receipt.v2',
    observer,
    child: {
      repository: expected.repository,
      run_id: expected.runId,
      run_attempt: expected.attempt,
      head_sha: expected.headSha,
      workflow: expected.workflow,
      dispatch_actor: observerDispatchActor,
      receipt_nonce: randomUUID(),
    },
  };
}

/** Immutable public declaration: no consumer identity belongs in this artifact. */
export function observerDeclarationName(context) {
  const observer = observerContext(context);
  return `consumer-observer-${observer.parent_run_attempt}-${observer.dispatch_nonce}`;
}

function artifactJsonFromZip(bytes, filename) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-observer-declaration-'));
  try {
    const archive = path.join(scratch, 'declaration.zip');
    fs.writeFileSync(archive, bytes, { mode: 0o600 });
    const options = {
      env: { PATH: process.env.PATH, LC_ALL: 'C' },
      encoding: 'utf8', timeout: 10_000, maxBuffer: 16_384,
    };
    if (execFileSync('unzip', ['-Z1', archive], options).trim() !== filename) {
      refuse('dispatch_declaration_archive_invalid');
    }
    return JSON.parse(execFileSync('unzip', ['-p', archive, filename], options));
  } finally {
    fs.rmSync(scratch, { recursive: true });
  }
}

/** Authenticate the declaration to the exact parent run before claiming custody.
 * Missing or ambiguous artifacts are unmeasured, never inferred from run names. */
export async function githubDeclaration(token, context, fetchRecord = fetch, extract = artifactJsonFromZip) {
  const observer = observerContext(context);
  if (!token) refuse('read_authority_or_identity_missing');
  const census = await githubArtifactCensus(token, observer.parent_repository,
    observer.parent_run_id, fetchRecord, observerDeclarationName(observer));
  const matches = census.filter(artifact => artifact.name === observerDeclarationName(observer));
  if (matches.length !== 1) refuse('dispatch_declaration_missing_or_ambiguous');
  const declaration = observerContext(await githubArtifactJson(token, observer.parent_repository, matches[0], {
    runId: observer.parent_run_id, headSha: observer.parent_head_sha,
    name: observerDeclarationName(observer), filename: 'consumer-observer.json',
  }, fetchRecord, extract));
  if (JSON.stringify(declaration) !== JSON.stringify(observer)) refuse('dispatch_declaration_mismatch');
  return declaration;
}

async function githubCensus(token, url, field, fetchRecord) {
  if (!token) refuse('read_authority_or_identity_missing');
  const records = [];
  let total;
  for (let page = 1; total === undefined || records.length < total; page += 1) {
    const endpoint = new URL(url);
    endpoint.searchParams.set('per_page', '100');
    endpoint.searchParams.set('page', String(page));
    const response = await fetchRecord(endpoint.toString(), {
      headers: { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json' },
      signal: AbortSignal.timeout(30_000),
    });
    if (!response.ok) refuse('observer_census_read_refused');
    const census = await response.json();
    if (!Number.isSafeInteger(census?.total_count) || census.total_count < 0
      || census.total_count > censusLimit || !Array.isArray(census[field])
      || (total !== undefined && total !== census.total_count)) {
      refuse('observer_census_unmeasured');
    }
    total = census.total_count;
    if (census[field].length !== Math.min(100, total - records.length)
      || census[field].some(record => !Number.isSafeInteger(record?.id) || record.id < 1)) {
      refuse('observer_census_unmeasured');
    }
    records.push(...census[field]);
  }
  if (new Set(records.map(record => record.id)).size !== records.length) refuse('observer_census_unmeasured');
  return records;
}

/** A measured empty array is data; callers still decide ordinary or pending.
 * Partial, duplicate, changing and over-budget reads are never a zero. */
export async function githubArtifactCensus(token, repository, runId, fetchRecord = fetch, name = null) {
  if (!/^[^/]+\/[^/]+$/.test(repository) || !id.test(runId)
    || (name !== null && (typeof name !== 'string' || !name))) {
    refuse('artifact_read_identity_invalid');
  }
  const query = name === null ? '' : `?name=${encodeURIComponent(name)}`;
  return githubCensus(token, `https://api.github.com/repos/${repository}/actions/runs/${runId}/artifacts${query}`,
    'artifacts', fetchRecord);
}

/** Run names narrow discovery only. The private receipt and fresh run record
 * must still qualify every discovered child before observation or cleanup. */
export async function githubChildCensus(token, repository, workflow, since, fetchRecord = fetch) {
  if (!/^[^/]+\/[^/]+$/.test(repository)
    || !/^\.github\/workflows\/[A-Za-z0-9_.-]+\.ya?ml$/.test(workflow)
    || typeof since !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(since)
    || !Number.isFinite(Date.parse(since))) {
    refuse('child_discovery_identity_invalid');
  }
  const filename = encodeURIComponent(workflow.slice('.github/workflows/'.length));
  const query = `event=workflow_dispatch&created=${encodeURIComponent(`>=${since}`)}`;
  return githubCensus(token, `https://api.github.com/repos/${repository}/actions/workflows/${filename}/runs?${query}`,
    'workflow_runs', fetchRecord);
}

/** Read declarations for an exact completed parent attempt. Legacy runs have
 * no declaration step; a successful declaration step without its artifact is
 * unmeasured rather than silently treated as an ordinary historical run. */
export async function githubParentDeclarations(token, runId, attempt, fetchRecord = fetch, extract = artifactJsonFromZip) {
  const parent = await githubRecord(token, parentRepository, runId, fetchRecord, attempt);
  if (parent.head_repository?.full_name !== parentRepository || !parentWorkflows.has(parent.path)
    || !sha.test(parent.head_sha) || parent.status !== 'completed' || !completedConclusions.has(parent.conclusion)) {
    refuse('completed_parent_identity_unproven');
  }
  const artifacts = await githubArtifactCensus(token, parentRepository, runId, fetchRecord);
  const jobs = await githubParentJobs(token,runId,attempt,fetchRecord);
  const prefix = `consumer-observer-${attempt}-`;
  const declarations = artifacts.filter(artifact => artifact.name?.startsWith(prefix));
  const publication = jobs.flatMap(job => job.steps).filter(step => step.name === observerDeclarationStep);
  // One context can own multiple dispatched children. Multiple publishers
  // cannot be paired to artifacts by count alone, so their custody is unknown.
  if (publication.length > 1) refuse('declaration_publication_ambiguous');
  if (publication.some(step => step.status !== 'completed' || !completedConclusions.has(step.conclusion))) {
    refuse('declaration_step_census_unmeasured');
  }
  const published = publication.filter(step => step.conclusion === 'success').length;
  if (declarations.length < published) {
    refuse('successful_declaration_missing');
  }
  // A failed upload can leave an artifact behind. It does not prove that the
  // caller reached the successful publication boundary or dispatched it.
  if (declarations.length !== published) refuse('declaration_publication_unmeasured');
  const contexts = [];
  for (const artifact of declarations) {
    const context = observerContext(await githubArtifactJson(token, parentRepository, artifact, {
      runId, headSha: parent.head_sha, name: artifact.name, filename: 'consumer-observer.json',
    }, fetchRecord, extract));
    qualifyObserverParent(context, parent);
    if (artifact.name !== observerDeclarationName(context)) refuse('dispatch_declaration_mismatch');
    contexts.push(context);
  }
  if (new Set(contexts.map(context => context.dispatch_nonce)).size !== contexts.length) {
    refuse('dispatch_declaration_missing_or_ambiguous');
  }
  return contexts;
}

/** The exact public attempt owns one complete measured job/step census. */
export async function githubParentJobs(token,runId,attempt,fetchRecord=fetch) {
  const jobs=await githubRunJobs(token,parentRepository,runId,attempt,fetchRecord);
  if(jobs.some(job=>!Array.isArray(job.steps)||(job.conclusion==='success'&&job.steps.length===0))){
    refuse('declaration_step_census_unmeasured');
  }
  return jobs;
}

/** One bounded exact-attempt census serves public custody and private timing. */
export async function githubRunJobs(token,repository,runId,attempt,fetchRecord=fetch) {
  if(typeof repository!=='string'||!/^[^/]+\/[^/]+$/.test(repository)) refuse('parent_identity_unproven');
  if(typeof runId!=='string'||!id.test(runId)||!Number.isSafeInteger(attempt)||attempt<1){
    refuse('parent_identity_unproven');
  }
  return githubCensus(token,
    `https://api.github.com/repos/${repository}/actions/runs/${runId}/attempts/${attempt}/jobs`,
    'jobs',fetchRecord);
}

/** One authenticated, bounded archive reader for public declarations and
 * private child receipts. Transport never prints a repository or artifact. */
export async function githubArtifactJson(token, repository, artifact, expected, fetchRecord = fetch, extract = artifactJsonFromZip) {
  if (!token || !/^[^/]+\/[^/]+$/.test(repository)
    || !id.test(expected.runId) || !sha.test(expected.headSha)
    || typeof expected.name !== 'string' || !expected.name
    || !/^consumer-observer(?:-[0-9a-f-]+)?\.json$/.test(expected.filename)) {
    refuse('artifact_read_identity_invalid');
  }
  if (!Number.isSafeInteger(artifact.id) || artifact.id < 1 || artifact.expired !== false
    || artifact.name !== expected.name
    || String(artifact.workflow_run?.id) !== expected.runId
    || artifact.workflow_run?.head_sha !== expected.headSha
    || !/^sha256:[0-9a-f]{64}$/.test(artifact.digest)
    || !Number.isSafeInteger(artifact.size_in_bytes) || artifact.size_in_bytes < 1
    || artifact.size_in_bytes > 65_536) {
    refuse('dispatch_declaration_identity_unproven');
  }
  const headers = { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json' };
  const base = `https://api.github.com/repos/${repository}/actions`;
  const archive = await fetchRecord(`${base}/artifacts/${artifact.id}/zip`, {
    headers, signal: AbortSignal.timeout(30_000),
  });
  if (!archive.ok) refuse('dispatch_declaration_read_refused');
  const bytes = Buffer.from(await archive.arrayBuffer());
  if (bytes.length > 65_536
    || `sha256:${createHash('sha256').update(bytes).digest('hex')}` !== artifact.digest) {
    refuse('dispatch_declaration_digest_mismatch');
  }
  return extract(bytes, expected.filename);
}

/** Response validation belongs here so empty API responses remain unknown. */
export async function githubRecord(token, repository, runId, fetchRecord = fetch, attempt = null) {
  if (!token || !/^[^/]+\/[^/]+$/.test(repository) || !id.test(runId)
    || (attempt !== null && (!Number.isSafeInteger(attempt) || attempt < 1))) {
    refuse('read_authority_or_identity_missing');
  }
  const suffix = attempt === null ? '' : `/attempts/${attempt}`;
  const response = await fetchRecord(`https://api.github.com/repos/${repository}/actions/runs/${runId}${suffix}`, {
    headers: { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json' },
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) refuse('run_read_refused');
  const record = await response.json();
  if (!record || String(record.id) !== runId || typeof record.status !== 'string'
    || !Number.isSafeInteger(record.run_attempt) || record.run_attempt < 1
    || (attempt !== null && record.run_attempt !== attempt)) {
    refuse('run_read_unmeasured');
  }
  return record;
}

export function observerTerminalName(context) {
  return `consumer-observer-product-${observerContext(context).dispatch_nonce}`;
}

/** Both dispatch adapters require the same immutable custody and fresh parent. */
export async function admitObserverDispatch(parentToken, consumerToken, context, target, fetchRecord = fetch, extract = artifactJsonFromZip) {
  const observer = observerContext(context);
  if (!target || Object.keys(target).sort().join(',') !== 'ref,repository,target,workflow'
    || typeof target.repository !== 'string' || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(target.repository)
    || typeof target.workflow !== 'string' || !/^[A-Za-z0-9_.-]+\.ya?ml$/.test(target.workflow)
    || typeof target.ref !== 'string' || !target.ref || /[\r\n\x00]/.test(target.ref)
    || typeof target.target !== 'string' || !/^v\d+\.\d+\.\d+$/.test(target.target)) {
    refuse('dispatch_target_invalid');
  }
  if (!consumerToken) refuse('dispatch_authority_missing');
  await githubDeclaration(parentToken, observer, fetchRecord, extract);
  const parent = await githubRecord(parentToken, observer.parent_repository, observer.parent_run_id, fetchRecord);
  admitObserver(observer, parent);
  return observer;
}

/** Dispatch only after immutable custody and a final live-parent read. The
 * response carries no child identity into the public caller. */
export async function dispatchObserverConsumer(parentToken, consumerToken, context, target, fetchRecord = fetch, extract = artifactJsonFromZip) {
  const observer = await admitObserverDispatch(parentToken, consumerToken, context, target, fetchRecord, extract);
  const response = await fetchRecord(`https://api.github.com/repos/${target.repository}/actions/workflows/${encodeURIComponent(target.workflow)}/dispatches`, {
    method:'POST', headers:{Authorization:`Bearer ${consumerToken}`, Accept:'application/vnd.github+json', 'Content-Type':'application/json'},
    body:JSON.stringify({ref:target.ref, inputs:{target:target.target, source_revision:observer.source_revision, legs:'clean', observer:JSON.stringify(observer)}}),
    signal:AbortSignal.timeout(30_000),
  });
  if (response.status !== 204) refuse('dispatch_refused');
  return {dispatched:true};
}

/** Pairing is declared in public source; private branch identity stays inside
 * the authenticated source selector and dispatcher. */
export function consumerPairing(text, requestedPull = '') {
  const lines=String(text??'').split(/\r?\n/).filter(line=>/^\s*Pairs-with:/.test(line));
  const pairs=new Set();
  if(requestedPull !== '') {
    if(typeof requestedPull !== 'string' || !id.test(requestedPull)) refuse('consumer_pairing_invalid');
    pairs.add(requestedPull);
  }
  for(const line of lines){
    const match=/^\s*Pairs-with:\s*consumer#([1-9][0-9]*)\s*$/.exec(line);
    if(!match) refuse('consumer_pairing_invalid');
    pairs.add(match[1]);
  }
  if(pairs.size>1) refuse('consumer_pairing_ambiguous');
  return pairs.size===1?[...pairs][0]:null;
}

export async function githubConsumerSource(token, repository, pairedPull = null, fetchRecord = fetch) {
  const read=async suffix=>{
    if (!token || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) refuse('read_authority_or_identity_missing');
    const response=await fetchRecord(`https://api.github.com/repos/${repository}${suffix}`, {
      headers:{Authorization:`Bearer ${token}`,Accept:'application/vnd.github+json'},signal:AbortSignal.timeout(30_000),
    });
    if (!response.ok) refuse('consumer_source_read_refused');
    return response.json();
  };
  if(pairedPull!==null){
    if(typeof pairedPull!=='string'||!id.test(pairedPull)) refuse('consumer_pairing_invalid');
    const pull=await read(`/pulls/${pairedPull}`);
    if(pull?.state!=='open'||pull.head?.repo?.full_name!==repository
      ||typeof pull.head?.ref!=='string'||!pull.head.ref||!sha.test(pull.head?.sha??'')){
      refuse('consumer_pairing_unqualified');
    }
    return {headSha:pull.head.sha,branch:pull.head.ref};
  }
  const repositoryRecord=await read('');
  if(repositoryRecord?.full_name!==repository || typeof repositoryRecord.default_branch!=='string'
    || !repositoryRecord.default_branch) refuse('consumer_source_unmeasured');
  const commit=await read(`/commits/${encodeURIComponent(repositoryRecord.default_branch)}`);
  if(typeof commit?.sha!=='string' || !sha.test(commit.sha)) refuse('consumer_source_unmeasured');
  return {headSha:commit.sha,branch:repositoryRecord.default_branch};
}

/** A public parent selector discovers custody; it does not grant it. Reuse
 * returns only the original authenticated public context and reached decision,
 * with no new declaration or cancellation ownership. */
export async function readConsumerParentDecision(token, selection, expected, fetchRecord = fetch, extract = artifactJsonFromZip) {
  if(!selection || Object.keys(selection).sort().join(',')!=='parentRunId,requireCurrentDefault,role,sourceRevision'
    || typeof selection.parentRunId!=='string' || !id.test(selection.parentRunId)
    || !Object.hasOwn(observerParentWorkflows,selection.role) || !sha.test(selection.sourceRevision)
    || typeof selection.requireCurrentDefault!=='boolean') refuse('parent_selection_unproven');
  const parent=await githubRecord(token,parentRepository,selection.parentRunId,fetchRecord);
  if(parent.head_repository?.full_name!==parentRepository || parent.path!==observerParentWorkflows[selection.role]
    || parent.status!=='completed' || !completedConclusions.has(parent.conclusion)) refuse('parent_selection_unproven');
  const declarations=await githubParentDeclarations(token,selection.parentRunId,parent.run_attempt,fetchRecord,extract);
  if(declarations.length!==1) refuse('dispatch_declaration_missing_or_ambiguous');
  const context=declarations[0];
  qualifyObserverParent(context,parent);
  if(context.source_revision!==selection.sourceRevision) refuse('parent_selection_source_unproven');
  const source=selection.requireCurrentDefault?await githubConsumerSource(token,expected.repository,null,fetchRecord):{};
  const reading=await readConsumerProduct(token,context,{...expected,...source},fetchRecord,extract,async()=>{
    const finalParent=await githubRecord(token,parentRepository,selection.parentRunId,fetchRecord);
    qualifyObserverParent(context,finalParent);
    if(finalParent.status!=='completed' || finalParent.conclusion!==parent.conclusion) refuse('parent_selection_unproven');
    if(selection.requireCurrentDefault){
      const finalSource=await githubConsumerSource(token,expected.repository,null,fetchRecord);
      if(finalSource.headSha!==source.headSha || finalSource.branch!==source.branch) refuse('child_source_no_longer_eligible');
    }
  });
  if(reading.discovered!==1 || reading.owned!==1 || reading.pending!==0 || reading.refused!==0
    || reading.product===null){
    refuse('product_decision_unmeasured');
  }
  return {context,reading,owned:false};
}

/** Historical reuse accepts only a reached pass. Retirement and publication
 * read the same authenticated decision owner rather than inspecting child logs. */
export async function readConsumerParentProduct(token, selection, expected, fetchRecord = fetch, extract = artifactJsonFromZip) {
  const result=await readConsumerParentDecision(token,selection,expected,fetchRecord,extract);
  if(result.reading.product.verdict!=='pass') refuse('product_decision_unmeasured');
  return result;
}

/** A supplied public context selects its already-issued custody. It grants no
 * new dispatch or cleanup ownership. Both live and completed parents require
 * final identity and source eligibility before accepting a reached decision. */
export async function readConsumerContextDecision(token, context, expected, fetchRecord = fetch, extract = artifactJsonFromZip) {
  const observer=observerContext(context);
  if(typeof expected.requireCurrentDefault!=='boolean') refuse('parent_selection_unproven');
  if(expected.completedAfter!==undefined&&(!Number.isSafeInteger(expected.completedAfter)||expected.completedAfter<1)){
    refuse('completion_deadline_unproven');
  }
  const parent=await githubRecord(token,observer.parent_repository,observer.parent_run_id,fetchRecord);
  qualifyObserverParent(observer,parent);
  const completed=parent.status==='completed';
  if(completed?!completedConclusions.has(parent.conclusion):!liveStates.has(parent.status)){
    refuse('parent_state_unmeasured');
  }
  const source=expected.requireCurrentDefault?await githubConsumerSource(token,expected.repository,null,fetchRecord):{};
  const reading=await readConsumerProduct(token,observer,{...expected,...source},fetchRecord,extract,async()=>{
    const finalParent=await githubRecord(token,observer.parent_repository,observer.parent_run_id,fetchRecord);
    qualifyObserverParent(observer,finalParent);
    if(completed?(finalParent.status!=='completed'||finalParent.conclusion!==parent.conclusion):!liveStates.has(finalParent.status)){
      refuse('parent_state_changed');
    }
    if(expected.requireCurrentDefault){
      const finalSource=await githubConsumerSource(token,expected.repository,null,fetchRecord);
      if(finalSource.headSha!==source.headSha||finalSource.branch!==source.branch) refuse('child_source_no_longer_eligible');
    }
  });
  if(reading.discovered!==1||reading.owned!==1||reading.pending!==0||reading.refused!==0||reading.product===null){
    refuse('product_decision_unmeasured');
  }
  return {context:observer,reading,owned:false};
}

/** Authenticated private custody is shared by observation and cleanup. */
async function readOwnedChild(token, observer, parent, expected, candidate, declaration, fetchRecord, extract) {
  const runId = String(candidate.id);
  const name = `consumer-observer-${observer.dispatch_nonce}`;
  const artifacts = await githubArtifactCensus(token, expected.repository, runId, fetchRecord, name);
  if (artifacts.length !== 1 || artifacts[0].name !== name) return null;
  const receipt = await githubArtifactJson(token, expected.repository, artifacts[0], {
    runId, headSha: candidate.head_sha, name, filename: `${name}.json`,
  }, fetchRecord, extract);
  const child = await githubRecord(token, expected.repository, runId, fetchRecord);
  const qualified = qualifyOwnedChild(receipt, observer, parent, child, expected, declaration);
  return {receipt:qualified, child};
}

/** Read a reached private decision without exporting its child identity.
 * Missing, partial or ambiguous discovery stays pending; it never manufactures
 * zero product counts. The final reads narrow, but cannot atomically eliminate,
 * a dispatch or rerun racing the observation. */
export async function observeConsumerProduct(token, context, expected, fetchRecord = fetch, extract = artifactJsonFromZip) {
  const observer=observerContext(context);
  const source=await githubConsumerSource(token,expected.repository,expected.pairedPull??null,fetchRecord);
  return readConsumerProduct(token,observer,{...expected,...source},fetchRecord,extract,async()=>{
    const parent=await githubRecord(token,observer.parent_repository,observer.parent_run_id,fetchRecord);
    qualifyObserverParent(observer,parent);
    if(!liveStates.has(parent.status)) refuse('parent_no_longer_live');
    const finalSource=await githubConsumerSource(token,expected.repository,expected.pairedPull??null,fetchRecord);
    if(finalSource.branch!==source.branch||finalSource.headSha!==source.headSha) refuse('child_source_no_longer_eligible');
  });
}

/** Project only this owner's authenticated reading. Execution failure stops
 * waiting, but remains separate from a reached product failure. */
export function consumerProductVerdict(reading) {
  if (reading.refused > 0) return 'refused';
  if (reading.discovered !== 1 || reading.owned !== 1 || reading.pending !== 0) return 'pending';
  if (reading.product !== null) return reading.product.verdict;
  return reading.execution !== null && reading.execution !== 'success' ? 'execution_failed' : 'pending';
}

// Reuse's eligibility reads run before the final child census and status read,
// so selecting the current default source cannot leave an old child snapshot
// at the end of its authority decision. The callback is private to this owner.
async function readConsumerProduct(token, context, expected, fetchRecord, extract, beforeFinalRead = null) {
  const observer = observerContext(context);
  const parent = await githubRecord(token, observer.parent_repository, observer.parent_run_id,
    fetchRecord, observer.parent_run_attempt);
  qualifyObserverParent(observer, parent);
  const declaration = await githubDeclaration(token, observer, fetchRecord, extract);
  const runs = await githubChildCensus(token, expected.repository, expected.workflow,
    parent.created_at, fetchRecord);
  const matching = runs.filter(run => run.display_title === observerRunName(observer));
  /** @type {{discovered:number, owned:number, pending:number, refused:number, custody_commitment:string|null, execution:string|null, product:ProductDecision|null}} */
  const result = {discovered:matching.length, owned:0, pending:1, refused:0, custody_commitment:null, execution:null, product:null};
  if (matching.length !== 1) return result;
  try {
    const candidate = matching[0];
    const owned = await readOwnedChild(token, observer, parent, expected, candidate, declaration, fetchRecord, extract);
    if (!owned) return result;
    result.owned = 1;
    result.custody_commitment = custodyCommitment(owned.receipt);
    if(expected.custodyCommitment!==undefined && expected.custodyCommitment!==result.custody_commitment){
      refuse('deadline_child_custody_unproven');
    }
    const name = observerTerminalName(observer);
    const artifacts = owned.child.status === 'completed'
      ? await githubArtifactCensus(token, expected.repository, String(candidate.id), fetchRecord, name) : [];
    if (artifacts.length > 1 || (artifacts.length === 1 && artifacts[0].name !== name)) {
      refuse('terminal_receipt_ambiguous');
    }
    const terminal = artifacts.length === 0 ? null : await githubArtifactJson(token, expected.repository, artifacts[0], {
      runId:String(candidate.id), headSha:candidate.head_sha, name,
      filename:`consumer-observer-${observer.dispatch_nonce}.json`,
    }, fetchRecord, extract);
    const chronology=expected.completedAfter===undefined?null:await githubRunJobs(token,expected.repository,
      String(candidate.id),owned.child.run_attempt,fetchRecord);
    if(beforeFinalRead) await beforeFinalRead();
    const finalRuns = await githubChildCensus(token, expected.repository, expected.workflow,
      parent.created_at, fetchRecord);
    const finalMatching = finalRuns.filter(run => run.display_title === observerRunName(observer));
    result.discovered = finalMatching.length;
    if (finalMatching.length !== 1 || finalMatching[0].id !== candidate.id) {
      // Discovery changed after authentication. Earlier custody does not
      // measure any child in this final census, even one valid replacement.
      result.owned = 0;
      result.custody_commitment = null;
      return result;
    }
    const finalChild = await githubRecord(token, expected.repository, String(candidate.id), fetchRecord);
    // Missing product evidence still requires fresh custody and a complete
    // final discovery read before an execution failure can end observation.
    qualifyOwnedChild(owned.receipt, observer, parent, finalChild, expected, declaration);
    result.execution = finalChild.status === 'completed' ? finalChild.conclusion : null;
    if (terminal === null) {
      if (result.execution !== null && result.execution !== 'success') result.pending = 0;
      return result;
    }
    const qualified = qualifyTerminalReceipt(terminal, observer, parent, finalChild, expected, declaration);
    // Both artifacts must describe the exact same independently proven custody.
    if (JSON.stringify(qualified.child) !== JSON.stringify(owned.receipt.child)) refuse('terminal_custody_unproven');
    if(chronology!==null){
      if(qualified.product.verdict!=='pass'||finalChild.conclusion!=='success'||chronology.length===0
        || chronology.some(job=>!Number.isSafeInteger(job.id)||job.id<1||String(job.run_id)!==String(finalChild.id)
          || job.run_attempt!==finalChild.run_attempt||job.head_sha!==finalChild.head_sha
          || job.status!=='completed'||!completedConclusions.has(job.conclusion))
        || !chronology.some(job=>job.conclusion!=='skipped'&&typeof job.completed_at==='string'
          && Number.isFinite(Date.parse(job.completed_at))&&Date.parse(job.completed_at)/1000>expected.completedAfter)){
        refuse('completion_after_deadline_unproven');
      }
    }
    result.product = qualified.product;
    result.pending = 0;
  } catch {
    result.refused = 1;
  }
  return result;
}

/** One reconciliation pass. Discovery alone cannot grant cancellation authority.
 * Only the authenticated private receipt binds a child, including its attempt.
 * Counts are safe to publish; private run identities never leave this owner.
 * GitHub's cancellation endpoint has no atomic attempt precondition: the final
 * read narrows, but cannot eliminate, the read/cancel race. */
export async function reconcileOwnedChildren(token, context, expected, fetchRecord = fetch, extract = artifactJsonFromZip, wait = pause) {
  const observer = observerContext(context);
  const parent = await githubRecord(token, observer.parent_repository, observer.parent_run_id,
    fetchRecord, observer.parent_run_attempt);
  qualifyObserverParent(observer, parent);
  if (!liveStates.has(parent.status) && (parent.status !== 'completed' || !completedConclusions.has(parent.conclusion))) {
    refuse('parent_state_unmeasured');
  }
  const declaration = await githubDeclaration(token, observer, fetchRecord, extract);
  const runs = await githubChildCensus(token, expected.repository, expected.workflow,
    parent.created_at, fetchRecord);
  const matching = runs.filter(run => run.display_title === observerRunName(observer));
  const result = { discovered: matching.length, owned: 0, terminal: 0, cancelled: 0, pending: matching.length === 0 ? 1 : 0, refused: 0 };
  for (const candidate of matching) {
    try {
      const runId = String(candidate.id);
      const owned = await readOwnedChild(token, observer, parent, expected, candidate, declaration, fetchRecord, extract);
      if (!owned) {
        result.pending += 1;
        continue;
      }
      // This is the latest child, not /attempts/N: a rerun must refuse the old
      // receipt rather than cancelling the newer attempt under historical data.
      const {receipt, child} = owned;
      result.owned += 1;
      if (child.status === 'completed') {
        result.terminal += 1;
        continue;
      }
      if (parent.status !== 'completed') {
        result.pending += 1;
        continue;
      }
      const response = await fetchRecord(`https://api.github.com/repos/${expected.repository}/actions/runs/${runId}/cancel`, {
        method: 'POST', headers: { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json' },
        signal: AbortSignal.timeout(30_000),
      });
      if (response.status !== 202) {
        result.refused += 1;
        continue;
      }
      result.cancelled += 1;
      let settled = false;
      for (let reading = 0; reading < cancellationReads; reading += 1) {
        if (reading > 0) await wait(cancellationPollMs);
        const finalChild = await githubRecord(token, expected.repository, runId, fetchRecord);
        qualifyOwnedChild(receipt, observer, parent, finalChild, expected, declaration);
        if (finalChild.status === 'completed') {
          settled = true;
          result.terminal += 1;
          break;
        }
      }
      if (!settled) result.pending += 1;
    } catch {
      // A failed read, invalid custody, or superseded attempt is visible as a
      // refusal. It cannot turn a partial ownership reading into a safe zero.
      result.refused += 1;
    }
  }
  // Dispatch can have been in flight when the parent ended. A fresh complete
  // discovery read must not silently omit a child that appeared during cleanup.
  const finalRuns = await githubChildCensus(token, expected.repository, expected.workflow,
    parent.created_at, fetchRecord);
  const known = new Set(matching.map(run => run.id));
  const appeared = finalRuns.filter(run => run.display_title === observerRunName(observer) && !known.has(run.id));
  result.discovered += appeared.length;
  result.pending += appeared.length;
  return result;
}
