import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {isDeepStrictEqual} from 'node:util';
import {githubArtifactCensus, githubArtifactFile, githubRecord, githubRunJobs, githubWorkflowRuns} from './consumer-observer.mjs';

const repository = 'burin-labs/harn';
const filename = 'dependency-security-proof.json';
const publishWorkflow = '.github/workflows/publish-release.yml';
const promotionWorkflow = '.github/workflows/promote-release.yml';
const sha = /^[0-9a-f]{40}$/;
const id = /^[1-9][0-9]*$/;
const refuse = reason => { throw new Error(`dependency_security_${reason}`); };
export function validatePolicy(value) {
  if (value?.schemaVersion !== 'harn.dependency_security_policy.v1'
    || value.proofSchema !== 'harn.dependency_security.v1'
    || typeof value.predicateType !== 'string' || !value.predicateType.startsWith('https://')
    || typeof value.signerWorkflow !== 'string' || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/\.github\/workflows\/[A-Za-z0-9_.-]+\.yml$/.test(value.signerWorkflow)
    || !sha.test(value.signerRevision ?? '')
    || !Number.isSafeInteger(value.maximumAgeMs) || value.maximumAgeMs <= 0
    || !Number.isSafeInteger(value.discoveryWindowMs) || value.discoveryWindowMs < value.maximumAgeMs) refuse('policy_unmeasured');
  return value;
}
const policy = validatePolicy(JSON.parse(fs.readFileSync(new URL('./dependency-security-policy.json', import.meta.url), 'utf8')));

export function publisherContext(env) {
  if (env.GITHUB_ACTIONS !== 'true' || env.GITHUB_REPOSITORY !== repository
    || !id.test(env.GITHUB_RUN_ID ?? '') || !id.test(env.GITHUB_RUN_ATTEMPT ?? '')
    || !sha.test(env.GITHUB_SHA ?? '') || !['push', 'workflow_dispatch'].includes(env.GITHUB_EVENT_NAME)
    || !new RegExp(`^${repository.replace('.', '\\.')}/${publishWorkflow.replaceAll('.', '\\.')}@refs/(heads/main|tags/v[0-9]+\\.[0-9]+\\.[0-9]+)$`).test(env.GITHUB_WORKFLOW_REF ?? '')) {
    refuse('requires_hosted_publisher_use_maintainer_release_workflow');
  }
  return {runId:env.GITHUB_RUN_ID, attempt:Number(env.GITHUB_RUN_ATTEMPT), event:env.GITHUB_EVENT_NAME,
    workflowRef:env.GITHUB_WORKFLOW_REF, eventSha:env.GITHUB_SHA,
    tagPush:env.GITHUB_EVENT_NAME === 'push' && env.GITHUB_REF_TYPE === 'tag'};
}

export function validateProof(proof, source, now, configuration = policy) {
  if (!proof || proof.schemaVersion !== configuration.proofSchema || proof.sourceRevision !== source
    || !sha.test(source) || !sha.test(proof.advisoryDatabaseRevision ?? '') || proof.workingDirectory !== '.'
    || !Number.isSafeInteger(proof.issuedAtMs) || proof.issuedAtMs < 1
    || proof.issuedAtMs > now + 60_000 || now - proof.issuedAtMs > configuration.maximumAgeMs
    || proof.producer?.repository !== repository || !id.test(proof.producer?.runId ?? '')
    || !id.test(proof.producer?.runAttempt ?? '') || !sha.test(proof.producer?.sourceDigest ?? '')
    || !sha.test(proof.producer?.eventHeadSha ?? '') || !sha.test(proof.producer?.workflowDigest ?? '')) {
    refuse('proof_unmeasured_stale_or_mismatched');
  }
  return proof;
}

export function validateProducer(proof, run, jobs, current, configuration = policy) {
  const own = proof.producer.runId === current.runId;
  const expectedPath = own ? publishWorkflow : promotionWorkflow;
  const expectedRef = own ? current.workflowRef : `${repository}/${expectedPath}@refs/heads/main`;
  if ((!own && !current.tagPush) || proof.producer.workflowRef !== expectedRef
    || String(run.id) !== proof.producer.runId || run.run_attempt !== Number(proof.producer.runAttempt)
    || run.path !== expectedPath || run.head_repository?.full_name !== repository
    || run.head_sha !== proof.producer.eventHeadSha
    || (run.event !== 'workflow_run' && run.head_sha !== proof.producer.sourceDigest)
    || !['in_progress', 'completed'].includes(run.status)
    || (own && run.run_attempt !== current.attempt)
    || !Array.isArray(run.referenced_workflows)
    || run.referenced_workflows.filter(ref => ref.path === `${configuration.signerWorkflow}@${configuration.signerRevision}`
      && ref.sha === configuration.signerRevision).length !== 1) refuse('producer_identity_unproven');
  const signers = jobs.filter(job => job.name === 'Dependency security proof' || job.name.endsWith(' / Dependency security proof'));
  if (signers.length !== 1 || signers[0].status !== 'completed' || signers[0].conclusion !== 'success'
    || !Number.isSafeInteger(signers[0].id) || signers[0].id < 1
    || String(signers[0].run_id) !== proof.producer.runId || signers[0].run_attempt !== Number(proof.producer.runAttempt)
    || signers[0].head_sha !== run.head_sha
    || !Array.isArray(signers[0].steps)
    || !['Attest the measured proof', 'Publish immutable dependency security proof'].every(name =>
      signers[0].steps.filter(step => step.name === name && step.status === 'completed' && step.conclusion === 'success').length === 1)) {
    refuse('signer_job_unmeasured');
  }
  const attestation = signers[0].steps.find(step => step.name === 'Attest the measured proof');
  const upload = signers[0].steps.find(step => step.name === 'Publish immutable dependency security proof');
  if (!Number.isSafeInteger(attestation.number) || !Number.isSafeInteger(upload.number)
    || attestation.number >= upload.number) refuse('signer_publication_order_unproven');
  const began = Date.parse(signers[0].started_at), ended = Date.parse(signers[0].completed_at);
  if (!Number.isFinite(began) || !Number.isFinite(ended) || ended < began
    || proof.issuedAtMs < began - 1000 || proof.issuedAtMs > ended + 1000) refuse('signer_time_unmeasured');
  return proof;
}

export function validateAttestation(records, proof, bytes, configuration = policy) {
  const digest = createHash('sha256').update(bytes).digest('hex');
  const signer = `https://github.com/${configuration.signerWorkflow}@${configuration.signerRevision}`;
  if (!Array.isArray(records) || !records.some(record => {
    const verified = record?.verificationResult;
    const certificate = verified?.signature?.certificate;
    const statement = verified?.statement;
    return Array.isArray(verified?.verifiedTimestamps) && verified.verifiedTimestamps.length > 0
      && certificate?.issuer === 'https://token.actions.githubusercontent.com'
      && certificate.sourceRepositoryURI === `https://github.com/${repository}`
      && certificate.sourceRepositoryDigest === proof.producer.sourceDigest
      && certificate.sourceRepositoryRef === proof.producer.workflowRef.split('@')[1]
      && certificate.buildSignerURI === signer && certificate.buildSignerDigest === configuration.signerRevision
      && certificate.buildConfigURI === `https://github.com/${proof.producer.workflowRef}`
      && certificate.buildConfigDigest === proof.producer.workflowDigest
      && certificate.runInvocationURI === `https://github.com/${repository}/actions/runs/${proof.producer.runId}/attempts/${proof.producer.runAttempt}`
      && statement?.predicateType === configuration.predicateType
      && Array.isArray(statement.subject) && statement.subject.some(subject => subject.name === filename && subject.digest?.sha256 === digest)
      && isDeepStrictEqual(statement.predicate, proof);
  })) refuse('attestation_identity_unproven');
  return proof;
}

function verifySignature(bytes, proof, configuration = policy) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'harn-dependency-security-'));
  try {
    const subject = path.join(scratch, filename);
    fs.writeFileSync(subject, bytes, {mode:0o600});
    const raw = execFileSync('gh', ['attestation', 'verify', subject, '--repo', repository,
      '--signer-workflow', configuration.signerWorkflow, '--signer-digest', configuration.signerRevision,
      '--source-ref', proof.producer.workflowRef.split('@')[1], '--predicate-type', configuration.predicateType,
      '--deny-self-hosted-runners', '--format', 'json'], {encoding:'utf8', maxBuffer:1_048_576, timeout:120_000, stdio:['ignore','pipe','pipe']});
    return validateAttestation(JSON.parse(raw), proof, bytes, configuration);
  } finally {
    fs.rmSync(scratch, {recursive:true});
  }
}

export async function requireDependencySecurity(env = process.env, adapters = {}) {
  const current = publisherContext(env);
  const source = adapters.source ?? execFileSync('git', ['rev-parse', 'HEAD'], {encoding:'utf8'}).trim();
  if (!sha.test(source)) refuse('published_source_unmeasured');
  if (adapters.source === undefined && execFileSync('git', ['status', '--porcelain', '--untracked-files=no'], {encoding:'utf8'}).trim()) {
    refuse('published_source_dirty');
  }
  const token = env.GH_TOKEN ?? env.GITHUB_TOKEN;
  if (!token) refuse('read_authority_missing');
  const fetchRecord = adapters.fetch ?? fetch;
  const readRun = adapters.readRun ?? ((run, attempt) => githubRecord(token, repository, String(run), fetchRecord, attempt));
  const currentRun = await readRun(current.runId, current.attempt);
  if (currentRun.path !== publishWorkflow || currentRun.head_sha !== current.eventSha
    || currentRun.head_repository?.full_name !== repository || currentRun.event !== current.event
    || currentRun.status !== 'in_progress' || !Number.isFinite(Date.parse(currentRun.created_at))
    || (current.tagPush && source !== current.eventSha)) refuse('publisher_identity_unproven');
  const artifactsFor = adapters.artifacts ?? (run => githubArtifactCensus(token, repository, String(run), fetchRecord));
  const own = await artifactsFor(current.runId);
  const prefix = `dependency-security-proof-${source}-`;
  let candidates = own.filter(artifact => artifact.name.startsWith(prefix)).map(artifact => ({artifact, run:currentRun}));
  if (candidates.length === 0 && current.tagPush) {
    const since = new Date(Date.parse(currentRun.created_at) - policy.discoveryWindowMs).toISOString().replace('.000Z', 'Z');
    const discover = adapters.promotions ?? (() => githubWorkflowRuns(token, repository, 'promote-release.yml', {created:`>=${since}`}, fetchRecord));
    for (const run of await discover()) {
      const artifacts = await artifactsFor(run.id);
      candidates.push(...artifacts.filter(artifact => artifact.name.startsWith(prefix)).map(artifact => ({artifact, run})));
    }
  }
  if (candidates.length === 0) refuse('proof_missing');
  const now = adapters.now ?? Date.now();
  const failures = [];
  for (const {artifact, run:listedRun} of candidates.sort((a,b) => Date.parse(b.artifact.created_at) - Date.parse(a.artifact.created_at))) {
    try {
      const readArtifact = adapters.readArtifact ?? ((artifact, expected) => githubArtifactFile(token, repository, artifact, expected, fetchRecord));
      const bytes = await readArtifact(artifact, {runId:String(listedRun.id), headSha:listedRun.head_sha, name:artifact.name, filename});
      if (!Buffer.isBuffer(bytes) || bytes.length < 1 || bytes.length > 16_384) refuse('proof_archive_unmeasured');
      const proof = validateProof(JSON.parse(bytes.toString('utf8')), source, now);
      const expectedName = `${prefix}${proof.producer.runId}-${proof.producer.runAttempt}`;
      if (artifact.name !== expectedName || String(listedRun.id) !== proof.producer.runId) refuse('artifact_identity_mismatched');
      const run = await readRun(proof.producer.runId, Number(proof.producer.runAttempt));
      const readJobs = adapters.jobs ?? ((run, attempt) => githubRunJobs(token, repository, run, attempt, fetchRecord));
      validateProducer(proof, run, await readJobs(proof.producer.runId, Number(proof.producer.runAttempt)), current);
      const verify = adapters.verify ?? verifySignature;
      await verify(bytes, proof);
      validateProof(proof, source, adapters.now ?? Date.now());
      return {verdict:'success', candidatesMeasured:candidates.length, refusedCount:failures.length,
        sourceRevision:proof.sourceRevision, advisoryDatabaseRevision:proof.advisoryDatabaseRevision,
        producerRunId:proof.producer.runId, producerRunAttempt:proof.producer.runAttempt, artifactId:artifact.id};
    } catch (error) {
      failures.push({artifactId:artifact.id, reason:error.message.startsWith('dependency_security_')
        ? error.message : 'dependency_security_proof_read_or_signature_refused'});
    }
  }
  const error = new Error('dependency_security_no_admitted_proof');
  error.measurement = {verdict:'refused', candidatesMeasured:candidates.length, refusedCount:failures.length, failures};
  throw error;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    console.log(JSON.stringify(await requireDependencySecurity()));
  } catch (error) {
    console.error(JSON.stringify(error.measurement ?? {verdict:'refused', reason:error.message}));
    process.exitCode = 1;
  }
}
