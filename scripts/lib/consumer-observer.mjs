import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const parentRepository = 'burin-labs/harn';
export const observerDispatchActor = 'harn-release-bot[bot]';
const parentWorkflows = new Set([
  '.github/workflows/consumer-canary.yml',
  '.github/workflows/build-release-binaries.yml',
  '.github/workflows/promote-release.yml',
]);
const sha = /^[0-9a-f]{40}$/;
const id = /^[1-9][0-9]*$/;
const nonce = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const liveStates = new Set(['queued', 'requested', 'waiting', 'pending', 'in_progress']);
const contextKeys = [
  'dispatch_nonce', 'parent_head_sha', 'parent_repository', 'parent_run_attempt',
  'parent_run_id', 'schema', 'source_revision',
];

/** @typedef {{schema: string, parent_repository: string, parent_run_id: string,
 * parent_run_attempt: number, parent_head_sha: string, source_revision: string,
 * dispatch_nonce: string}} ObserverContext */

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
  if (context.schema !== 'harn.consumer_observer.v1'
    || context.parent_repository !== parentRepository
    || typeof context.parent_run_id !== 'string' || !id.test(context.parent_run_id)
    || !Number.isSafeInteger(context.parent_run_attempt) || context.parent_run_attempt < 1
    || typeof context.parent_head_sha !== 'string' || !sha.test(context.parent_head_sha)
    || typeof context.source_revision !== 'string' || !sha.test(context.source_revision)
    || typeof context.dispatch_nonce !== 'string' || !nonce.test(context.dispatch_nonce)) {
    refuse('context_identity_invalid');
  }
  return Object.fromEntries(contextKeys.map(key => [key, context[key]]));
}

/** @param {{runId: string, attempt: number, headSha: string, sourceRevision: string}} parent */
export function createObserverContext(parent) {
  return observerContext({
    schema: 'harn.consumer_observer.v1',
    parent_repository: parentRepository,
    parent_run_id: parent.runId,
    parent_run_attempt: parent.attempt,
    parent_head_sha: parent.headSha,
    source_revision: parent.sourceRevision,
    dispatch_nonce: randomUUID(),
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
export function admitObserver(input, parent) {
  const context = observerContext(input);
  if (!parent || String(parent.id) !== context.parent_run_id
    || parent.head_repository?.full_name !== context.parent_repository
    || parent.head_sha !== context.parent_head_sha
    || !parentWorkflows.has(parent.path)) {
    refuse('parent_identity_unproven');
  }
  if (parent.run_attempt !== context.parent_run_attempt) refuse('parent_attempt_superseded');
  if (!liveStates.has(parent.status)) {
    if (parent.status === 'completed') refuse('parent_ended');
    refuse('parent_state_unmeasured');
  }
  return context;
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
  const observer = admitObserver(context, parent);
  if (JSON.stringify(observerContext(declaration)) !== JSON.stringify(observer)) {
    refuse('dispatch_declaration_mismatch');
  }
  if (!liveStates.has(child?.status)) refuse('child_state_unmeasured_or_ended');
  if (String(child?.id) !== expected.runId || child.run_attempt !== expected.attempt
    || child.head_sha !== expected.headSha || child.head_repository?.full_name !== expected.repository
    || child.path !== expected.workflow || child.event !== 'workflow_dispatch'
    || child.actor?.login !== observerDispatchActor || child.actor?.type !== 'Bot'
    || expected.sourceRevision !== observer.source_revision
    || event?.inputs?.source_revision !== observer.source_revision
    || JSON.stringify(observerContext(JSON.parse(event?.inputs?.observer ?? 'null'))) !== JSON.stringify(observer)
    || child.display_title !== observerRunName(observer)) {
    refuse('child_dispatch_identity_unproven');
  }
  return {
    schema: 'harn.consumer_observer_receipt.v1',
    observer,
    child: {
      repository: expected.repository,
      run_id: expected.runId,
      run_attempt: expected.attempt,
      head_sha: expected.headSha,
      workflow: expected.workflow,
      dispatch_actor: observerDispatchActor,
    },
  };
}

/** Immutable public declaration: no consumer identity belongs in this artifact. */
export function observerDeclarationName(context) {
  const observer = observerContext(context);
  return `consumer-observer-${observer.parent_run_attempt}-${observer.dispatch_nonce}`;
}

function declarationFromZip(bytes) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-observer-declaration-'));
  try {
    const archive = path.join(scratch, 'declaration.zip');
    fs.writeFileSync(archive, bytes, { mode: 0o600 });
    const options = {
      env: { PATH: process.env.PATH, LC_ALL: 'C' },
      encoding: 'utf8', timeout: 10_000, maxBuffer: 16_384,
    };
    if (execFileSync('unzip', ['-Z1', archive], options).trim() !== 'consumer-observer.json') {
      refuse('dispatch_declaration_archive_invalid');
    }
    return JSON.parse(execFileSync('unzip', ['-p', archive, 'consumer-observer.json'], options));
  } finally {
    fs.rmSync(scratch, { recursive: true });
  }
}

/** Authenticate the declaration to the exact parent run before claiming custody.
 * Missing or ambiguous artifacts are unmeasured, never inferred from run names. */
export async function githubDeclaration(token, context, fetchRecord = fetch, extract = declarationFromZip) {
  const observer = observerContext(context);
  if (!token) refuse('read_authority_or_identity_missing');
  const headers = { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json' };
  const base = `https://api.github.com/repos/${observer.parent_repository}/actions`;
  const declarationName = encodeURIComponent(observerDeclarationName(observer));
  const response = await fetchRecord(`${base}/runs/${observer.parent_run_id}/artifacts?per_page=100&name=${declarationName}`, {
    headers, signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) refuse('dispatch_declaration_read_refused');
  const census = await response.json();
  if (!Number.isSafeInteger(census?.total_count) || census.total_count < 0
    || !Array.isArray(census.artifacts) || census.total_count !== census.artifacts.length) {
    refuse('dispatch_declaration_census_unmeasured');
  }
  const matches = census.artifacts.filter(artifact => artifact.name === observerDeclarationName(observer));
  if (matches.length !== 1) refuse('dispatch_declaration_missing_or_ambiguous');
  const artifact = matches[0];
  if (!Number.isSafeInteger(artifact.id) || artifact.id < 1 || artifact.expired !== false
    || String(artifact.workflow_run?.id) !== observer.parent_run_id
    || artifact.workflow_run?.head_sha !== observer.parent_head_sha
    || !/^sha256:[0-9a-f]{64}$/.test(artifact.digest)
    || !Number.isSafeInteger(artifact.size_in_bytes) || artifact.size_in_bytes < 1
    || artifact.size_in_bytes > 65_536) {
    refuse('dispatch_declaration_identity_unproven');
  }
  const archive = await fetchRecord(`${base}/artifacts/${artifact.id}/zip`, {
    headers, signal: AbortSignal.timeout(30_000),
  });
  if (!archive.ok) refuse('dispatch_declaration_read_refused');
  const bytes = Buffer.from(await archive.arrayBuffer());
  if (bytes.length > 65_536
    || `sha256:${createHash('sha256').update(bytes).digest('hex')}` !== artifact.digest) {
    refuse('dispatch_declaration_digest_mismatch');
  }
  const declaration = observerContext(extract(bytes));
  if (JSON.stringify(declaration) !== JSON.stringify(observer)) refuse('dispatch_declaration_mismatch');
  return declaration;
}

/** Response validation belongs here so empty API responses remain unknown. */
export async function githubRecord(token, repository, runId, fetchRecord = fetch) {
  if (!token || !/^[^/]+\/[^/]+$/.test(repository) || !id.test(runId)) {
    refuse('read_authority_or_identity_missing');
  }
  const response = await fetchRecord(`https://api.github.com/repos/${repository}/actions/runs/${runId}`, {
    headers: { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json' },
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) refuse('run_read_refused');
  const record = await response.json();
  if (!record || String(record.id) !== runId || typeof record.status !== 'string'
    || !Number.isSafeInteger(record.run_attempt) || record.run_attempt < 1) {
    refuse('run_read_unmeasured');
  }
  return record;
}
