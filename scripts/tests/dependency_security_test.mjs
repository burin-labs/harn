import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {createHash} from 'node:crypto';
import {publisherContext, validatePolicy, validateProof, validateProducer, validateAttestation, requireDependencySecurity} from '../lib/dependency-security.mjs';

const policy = JSON.parse(fs.readFileSync(new URL('../lib/dependency-security-policy.json', import.meta.url)));
const source = 'a'.repeat(40), eventHead = 'b'.repeat(40), policyHead = 'c'.repeat(40);
const now = 1_800_000_000_000;
const repository = 'burin-labs/harn';
const publish = '.github/workflows/publish-release.yml', promote = '.github/workflows/promote-release.yml';
const environment = {GITHUB_ACTIONS:'true', GITHUB_REPOSITORY:repository, GITHUB_RUN_ID:'100', GITHUB_RUN_ATTEMPT:'1',
  GITHUB_SHA:eventHead, GITHUB_WORKFLOW_REF:`${repository}/${publish}@refs/heads/main`, GITHUB_EVENT_NAME:'workflow_dispatch', GH_TOKEN:'fixture'};

function fixture(fallback = false) {
  const env = fallback ? {...environment, GITHUB_SHA:source, GITHUB_EVENT_NAME:'push', GITHUB_REF_TYPE:'tag',
    GITHUB_WORKFLOW_REF:`${repository}/${publish}@refs/tags/v0.10.182`} : {...environment};
  const runId = fallback ? '90' : '100';
  const workflow = fallback ? promote : publish;
  const producerSource = fallback ? policyHead : eventHead;
  const producerEventHead = fallback ? source : eventHead;
  const proof = {schemaVersion:policy.proofSchema, sourceRevision:source, advisoryDatabaseRevision:'d'.repeat(40),
    workingDirectory:'.', issuedAtMs:now-1000, producer:{repository, runId, runAttempt:'1',
      workflowRef:`${repository}/${workflow}@refs/heads/main`, sourceDigest:producerSource, eventHeadSha:producerEventHead, workflowDigest:producerSource}};
  const run = {id:Number(runId), run_attempt:1, path:workflow, head_sha:producerEventHead, status:'in_progress', event:fallback?'workflow_run':'workflow_dispatch',
    created_at:new Date(now-2000).toISOString(),
    head_repository:{full_name:repository}, referenced_workflows:[{path:`${policy.signerWorkflow}@${policy.signerRevision}`, sha:policy.signerRevision}]};
  const current = {id:100, run_attempt:1, path:publish, head_sha:env.GITHUB_SHA, status:'in_progress', event:env.GITHUB_EVENT_NAME,
    head_repository:{full_name:repository}, created_at:new Date(now-2000).toISOString()};
  const jobs = [{id:77, run_id:Number(runId), run_attempt:1, head_sha:producerEventHead, name:'publication-security / Dependency security proof', status:'completed', conclusion:'success',
    started_at:new Date(now-2000).toISOString(), completed_at:new Date(now-500).toISOString(),
    steps:[{name:'Attest the measured proof', status:'completed', conclusion:'success', number:3},
      {name:'Publish immutable dependency security proof', status:'completed', conclusion:'success', number:4}]}];
  const bytes = Buffer.from(JSON.stringify(proof)+'\n');
  const artifact = {id:50, name:`dependency-security-proof-${source}-${runId}-1`, created_at:new Date(now-1000).toISOString()};
  const record = {verificationResult:{verifiedTimestamps:[{}], signature:{certificate:{
    issuer:'https://token.actions.githubusercontent.com', sourceRepositoryURI:`https://github.com/${repository}`,
    sourceRepositoryDigest:producerSource, sourceRepositoryRef:'refs/heads/main',
    buildSignerURI:`https://github.com/${policy.signerWorkflow}@${policy.signerRevision}`, buildSignerDigest:policy.signerRevision,
    runInvocationURI:`https://github.com/${repository}/actions/runs/${runId}/attempts/1`,
    buildConfigURI:`https://github.com/${proof.producer.workflowRef}`, buildConfigDigest:producerSource,
  }}, statement:{predicateType:policy.predicateType, predicate:proof, subject:[{name:'dependency-security-proof.json', digest:{sha256:createHash('sha256').update(bytes).digest('hex')}}]}}};
  return {env, proof, run, current, jobs, bytes, artifact, record};
}

test('bare local publishing refuses before any source or registry work', async () => {
  assert.throws(()=>publisherContext({}), /requires_hosted_publisher/);
  let reached = false;
  await assert.rejects(requireDependencySecurity({}, {readRun:()=>{reached=true;}}), /requires_hosted_publisher/);
  assert.equal(reached, false);
});

test('an absent freshness or signer policy is not an unbounded permission', () => {
  for (const patch of [{maximumAgeMs:undefined}, {maximumAgeMs:0}, {discoveryWindowMs:undefined}, {signerRevision:''}]) {
    assert.throws(()=>validatePolicy({...policy,...patch}), /policy_unmeasured/);
  }
});

test('a real measured source differs safely from the workflow policy and event heads', () => {
  const f=fixture();
  assert.equal(validateProof(f.proof, source, now).sourceRevision, source);
  assert.equal(validateProducer(f.proof, f.run, f.jobs, publisherContext(f.env)), f.proof);
  assert.equal(validateAttestation([f.record], f.proof, f.bytes), f.proof);
});

test('missing, stale, future, subworkspace, or mismatched scan measurements refuse', () => {
  const f=fixture();
  for (const patch of [{sourceRevision:''}, {sourceRevision:eventHead}, {advisoryDatabaseRevision:''},
    {workingDirectory:'crates/harn-vm'}, {issuedAtMs:undefined}, {issuedAtMs:now-policy.maximumAgeMs-1}, {issuedAtMs:now+60_001}]) {
    assert.throws(()=>validateProof({...f.proof,...patch},source,now), /proof_unmeasured/);
  }
});

test('JSON claims do not replace the actual producer, job, owner revision, or run attempt', () => {
  const f=fixture();
  for (const patch of [{path:promote}, {run_attempt:2}, {head_sha:source}, {referenced_workflows:[]}, {head_repository:{full_name:'fork/harn'}}]) {
    assert.throws(()=>validateProducer(f.proof,{...f.run,...patch},f.jobs,publisherContext(f.env)), /producer_identity/);
  }
  for (const patch of [{status:'in_progress'}, {conclusion:'skipped'}, {run_attempt:2}, {head_sha:source}, {steps:[]}]) {
    assert.throws(()=>validateProducer(f.proof,f.run,[{...f.jobs[0],...patch}],publisherContext(f.env)), /signer_job/);
  }
});

test('absent, forged, wrong-subject, or unsigned verification never counts as a signature', () => {
  const f=fixture();
  assert.throws(()=>validateAttestation([],f.proof,f.bytes), /attestation_identity/);
  for (const change of [r=>r.verificationResult.verifiedTimestamps=[],
    r=>r.verificationResult.signature.certificate.buildSignerDigest=source,
    r=>r.verificationResult.signature.certificate.runInvocationURI='https://github.com/other/run',
    r=>r.verificationResult.statement.subject=[], r=>r.verificationResult.statement.predicateType='other',
    r=>r.verificationResult.statement.predicate.workingDirectory='nested']) {
    const record=structuredClone(f.record); change(record);
    assert.throws(()=>validateAttestation([record],f.proof,f.bytes), /attestation_identity/);
  }
});

async function exercise(f, changes={}) {
  let verified=0;
  const adapters={source, now, readRun:async run=>String(run)===String(f.run.id)?f.run:f.current,
    artifacts:async run=>String(run)===String(f.run.id)?[f.artifact]:[],
    promotions:async()=>[f.run], readArtifact:async()=>f.bytes, jobs:async()=>f.jobs,
    verify:async(bytes,proof)=>{verified++; return validateAttestation([f.record],proof,bytes);}, ...changes};
  const result=await requireDependencySecurity(f.env,adapters);
  assert.equal(verified,1);
  assert.equal(result.verdict,'success');
  assert.equal(result.sourceRevision,source);
  assert.equal(result.candidatesMeasured,1);
  assert.equal(result.refusedCount,0);
  return result;
}

test('hosted manual recovery reaches its own fresh proof and verified signer', async()=> {
  await exercise(fixture());
});

test('a frozen tag workflow reaches promotion proof without new write permissions', async()=> {
  await exercise(fixture(true));
});

test('manual recovery cannot borrow another run proof or accept a missing archive', async()=> {
  const f=fixture();
  await assert.rejects(exercise(f,{artifacts:async()=>[]}),/proof_missing/);
  await assert.rejects(exercise(f,{readArtifact:async()=>Buffer.alloc(0)}),/no_admitted_proof/);
  await assert.rejects(exercise(f,{verify:async()=>{throw new Error('bad signature');}}),/no_admitted_proof/);
});
