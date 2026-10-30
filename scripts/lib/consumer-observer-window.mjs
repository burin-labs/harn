import fs from 'node:fs';
import {consumerProductVerdict,observerContext,observerDeclarationStep,observerWindowSteps,qualifyObserverParent,qualifyObserverPublisher,
  githubDeclaration,githubRecord,githubParentJobs,githubArtifactCensus,githubArtifactJson,
  readConsumerContextDecision} from './consumer-observer.mjs';
export {observerWindowSteps} from './consumer-observer.mjs';

export const observerDeadlineStep='Publish consumer observation deadline';
// Use the bounded transport's existing single-file envelope; the authenticated
// artifact name and closed schema distinguish this from a declaration.
export const observerDeadlineFilename='consumer-observer.json';

export function observerDeadlineName(context){
  const observer=observerContext(context);
  return `consumer-observer-deadline-${observer.parent_run_attempt}-${observer.dispatch_nonce}`;
}

/** Reached deadline evidence, never a product verdict or private child ID. */
export function consumerDeadlineReceipt(context,observedAt,reading) {
  const observer=observerContext(context);
  if(!Number.isSafeInteger(observedAt)
    || observedAt-observer.started_at<legacyConsumerObservationPolicy().deadline_seconds
    || !reading || Object.keys(reading).sort().join(',')!=='custody_commitment,discovered,execution,owned,pending,product,refused'
    || reading.discovered!==1 || reading.owned!==1 || reading.pending!==1 || reading.refused!==0
    || typeof reading.custody_commitment!=='string'||!/^[0-9a-f]{64}$/.test(reading.custody_commitment)
    || reading.product!==null || ![null,'success'].includes(reading.execution)) {
    throw new Error('CONSUMER_OBSERVER reason=deadline_unmeasured');
  }
  return {schema:'harn.consumer_observer.deadline.v1',observer,observed_at:observedAt,
    reading:{discovered:1,owned:1,pending:1,refused:0,custody_commitment:reading.custody_commitment,execution:reading.execution,product:null}};
}

/** The caller supplies measured steps from the authenticated original job.
 * One canonical issuer and failed observer must precede successful publication. */
export function qualifyConsumerDeadline(input,context,declarationStep,observationStep,publicationStep) {
  if(!input || Object.keys(input).sort().join(',')!=='observed_at,observer,reading,schema'
    || input.schema!=='harn.consumer_observer.deadline.v1'
    || JSON.stringify(observerContext(input.observer))!==JSON.stringify(observerContext(context))
    || declarationStep?.name!==observerDeclarationStep
    || !observerWindowSteps.includes(observationStep?.name)
    || publicationStep?.name!==observerDeadlineStep
    || declarationStep.status!=='completed'||declarationStep.conclusion!=='success'
    || observationStep.status!=='completed'||observationStep.conclusion!=='failure'
    || publicationStep.status!=='completed'||publicationStep.conclusion!=='success'
    || ![declarationStep,observationStep,publicationStep].every(step=>Number.isSafeInteger(step.number)&&step.number>0)
    || declarationStep.number>=observationStep.number || observationStep.number>=publicationStep.number) {
    throw new Error('CONSUMER_OBSERVER reason=deadline_identity_unproven');
  }
  const receipt=consumerDeadlineReceipt(context,input.observed_at,input.reading);
  const intervals=[declarationStep,observationStep,publicationStep].map(step=>({
    start:Date.parse(step.started_at)/1000,end:Date.parse(step.completed_at)/1000,
  }));
  if(intervals.some(interval=>!Number.isFinite(interval.start)||!Number.isFinite(interval.end)||interval.end<interval.start)
    || intervals[0].end>intervals[1].start || intervals[1].end>intervals[2].start){
    throw new Error('CONSUMER_OBSERVER reason=deadline_producer_clock_unproven');
  }
  for(const [time,step] of [[receipt.observer.started_at,declarationStep],[receipt.observed_at,observationStep]]){
    const start=Date.parse(step.started_at)/1000,end=Date.parse(step.completed_at)/1000;
    if(time<Math.floor(start)||time>Math.ceil(end)){
      throw new Error('CONSUMER_OBSERVER reason=deadline_producer_clock_unproven');
    }
  }
  return receipt;
}

/** Inert recovery evidence reader. It grants no product verdict or cleanup
 * ownership; publication/profile/archive authorization remains at its owner. */
export async function readConsumerDeadline(token,context,fetchRecord=fetch,extract) {
  const observer=observerContext(context);
  const parent=await githubRecord(token,observer.parent_repository,observer.parent_run_id,fetchRecord);
  qualifyObserverParent(observer,parent);
  if(parent.status!=='completed'||parent.conclusion!=='failure'){
    throw new Error('CONSUMER_OBSERVER reason=deadline_parent_unproven');
  }
  await githubDeclaration(token,observer,fetchRecord,extract);
  const jobs=await githubParentJobs(token,observer.parent_run_id,observer.parent_run_attempt,fetchRecord);
  const {job,witness}=qualifyObserverPublisher(observer,jobs);
  const publications=job.steps.filter(step=>step.name===observerDeadlineStep);
  if(publications.length!==1||publications[0].number<=witness.number) throw new Error('CONSUMER_OBSERVER reason=deadline_publication_unmeasured');
  const publication=publications[0];
  if(!Number.isSafeInteger(job.id)||job.id<1||String(job.run_id)!==observer.parent_run_id
    || job.run_attempt!==observer.parent_run_attempt||job.head_sha!==observer.parent_head_sha
    || job.status!=='completed'||job.conclusion!=='failure'){
    throw new Error('CONSUMER_OBSERVER reason=deadline_job_identity_unproven');
  }
  const declarations=job.steps.filter(step=>step.name===observerDeclarationStep);
  const observations=job.steps.filter(step=>observerWindowSteps.includes(step.name)&&step.conclusion==='failure');
  if(declarations.length!==1||observations.length!==1){
    throw new Error('CONSUMER_OBSERVER reason=deadline_producer_unmeasured');
  }
  const name=observerDeadlineName(observer);
  const artifacts=await githubArtifactCensus(token,observer.parent_repository,observer.parent_run_id,fetchRecord,name);
  if(artifacts.length!==1||artifacts[0].name!==name){
    throw new Error('CONSUMER_OBSERVER reason=deadline_artifact_unmeasured');
  }
  const input=await githubArtifactJson(token,observer.parent_repository,artifacts[0],{
    runId:observer.parent_run_id,headSha:observer.parent_head_sha,name,filename:observerDeadlineFilename,
  },fetchRecord,extract);
  const receipt=qualifyConsumerDeadline(input,observer,declarations[0],observations[0],publication);
  const finalParent=await githubRecord(token,observer.parent_repository,observer.parent_run_id,fetchRecord);
  qualifyObserverParent(observer,finalParent);
  if(finalParent.status!=='completed'||finalParent.conclusion!=='failure'||finalParent.path!==parent.path){
    throw new Error('CONSUMER_OBSERVER reason=deadline_parent_changed');
  }
  return receipt;
}

/** Recovery combines original reached deadline with a later current-source
 * pass through the same custody owner, without exporting child chronology. */
export async function readConsumerRecoveryDecision(token,context,expected,fetchRecord=fetch,extract) {
  const deadline=await readConsumerDeadline(token,context,fetchRecord,extract);
  const decision=await readConsumerContextDecision(token,context,{
    ...expected,requireCurrentDefault:true,completedAfter:deadline.observed_at,
    custodyCommitment:deadline.reading.custody_commitment,
  },fetchRecord,extract);
  return {...decision,deadline};
}

/** Immutable v1 receipt policy. Current workflow timing belongs to
 * consumer-observation-budget.mjs and cannot reinterpret this history. */
export function legacyConsumerObservationPolicy() {
  const policy=JSON.parse(fs.readFileSync(new URL('../ci/consumer_canary_policy.json',import.meta.url),'utf8'));
  if(Object.keys(policy).sort().join(',')!=='deadline_seconds,poll_seconds,window_seconds'
    || !Object.values(policy).every(value=>Number.isSafeInteger(value)&&value>0)
    || policy.poll_seconds>policy.window_seconds || policy.window_seconds>=policy.deadline_seconds){
    throw new Error('CONSUMER_OBSERVER reason=observation_budget_invalid');
  }
  return {...policy,max_windows:Math.ceil(policy.deadline_seconds/policy.window_seconds)};
}

/** Renewing credentials resumes the same context and aggregate deadline.
 * Observation owns custody and product authority; this loop owns time only. */
export async function waitLegacyConsumerWindow(startedAt, observe, {
  now=()=>Math.floor(Date.now()/1000),
  pause=seconds=>new Promise(resolve=>setTimeout(resolve,seconds*1000)),
  policy=legacyConsumerObservationPolicy(),
}={}) {
  const windowStarted=now();
  if(!Number.isSafeInteger(startedAt)||startedAt<0||startedAt>windowStarted){
    throw new Error('CONSUMER_OBSERVER reason=observation_start_invalid');
  }
  let lastReading=null;
  let previous=windowStarted;
  for(;;){
    const current=now();
    if(!Number.isSafeInteger(current)||current<previous){
      throw new Error('CONSUMER_OBSERVER reason=observation_clock_invalid');
    }
    // An expired renewal still obtains one fresh authenticated reading.
    // Prior-window absence cannot produce reached deadline evidence.
    lastReading=await observe();
    const afterRead=now();
    if(!Number.isSafeInteger(afterRead)||afterRead<current){
      throw new Error('CONSUMER_OBSERVER reason=observation_clock_invalid');
    }
    previous=afterRead;
    if(afterRead-startedAt>=policy.deadline_seconds) return {verdict:'deadline',wallSeconds:afterRead-startedAt,
      observedAt:afterRead,reading:lastReading};
    const verdict=consumerProductVerdict(lastReading);
    if(verdict!=='pending') return {verdict,wallSeconds:afterRead-startedAt,reading:lastReading};
    if(afterRead-windowStarted>=policy.window_seconds) return {verdict:'pending',wallSeconds:afterRead-startedAt,reading:lastReading};
    await pause(Math.min(policy.poll_seconds,policy.window_seconds-(afterRead-windowStarted),
      policy.deadline_seconds-(afterRead-startedAt)));
  }
}
