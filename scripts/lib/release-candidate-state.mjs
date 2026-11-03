import {execFile} from 'node:child_process';
import {realpathSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
import {
  admitObserver, archiveProducerBinding, githubArchiveProducer, githubArtifactCensus,
  githubArtifactJson, githubChildCensus, githubRecord, githubRunJobs, githubWorkflowRuns,
  observeConsumerTiming, observerConclusionIsKnown, observerContext, observerDeclarationName,
  observerParentRepository, observerParentWorkflows, observerRefusal, observerRunIsLive,
  observerRunName, qualifyObserverPublisher,
} from './consumer-observer.mjs';

const shaPattern=/^[0-9a-f]{40}$/;
const cliAuthority='gh-cli-authenticated-read';
function refuse(reason){throw new Error(`CONSUMER_OBSERVER reason=${reason}`);}
function livePromotion(parent,repository,runId,expected){
  if(String(parent.id)!==runId||parent.path!==observerParentWorkflows.promotion||parent.head_branch!=='main'
    ||!shaPattern.test(parent.head_sha)||parent.repository?.full_name!==repository
    ||parent.head_repository?.full_name!==repository
    ||expected&&(parent.run_attempt!==expected.run_attempt||parent.head_sha!==expected.head_sha)){
    refuse('promotion_identity_unmeasured');
  }
  if(parent.status==='completed'&&observerConclusionIsKnown(parent.conclusion))return false;
  if(observerRunIsLive(parent.status)&&parent.conclusion===null)return true;
  refuse('promotion_state_unmeasured');
}
function reportJobs(result,jobs){
  if(jobs.some(job=>typeof job.name!=='string'||!job.name||!(observerRunIsLive(job.status)&&job.conclusion===null
    ||job.status==='completed'&&observerConclusionIsKnown(job.conclusion))))refuse('promotion_job_state_unmeasured');
  result.pending_jobs.push(...jobs.filter(job=>observerRunIsLive(job.status)).map(job=>job.name));
  result.bad_jobs.push(...jobs.filter(job=>job.status==='completed'
    &&!['success','skipped'].includes(job.conclusion)).map(job=>job.name));
}
function sourceRuns(rows,source){
  if(rows.some(row=>row.head_sha!==source||typeof row.event!=='string'||!row.event
    ||typeof row.head_branch!=='string'||!row.head_branch
    ||!Number.isSafeInteger(row.run_attempt)||row.run_attempt<1
    ||!(observerRunIsLive(row.status)&&row.conclusion===null
      ||row.status==='completed'&&observerConclusionIsKnown(row.conclusion)))) refuse('source_census_unmeasured');
  return rows;
}
async function candidates(token,repository,source,fetchRecord){
  if(!shaPattern.test(source)) refuse('source_identity_invalid');
  return sourceRuns(await githubWorkflowRuns(token,repository,'build-release-binaries.yml',
    {head_sha:source},fetchRecord),source);
}
/** The general selector remains success-only; all pages must be measured. */
export async function successfulCandidateRuns(token,repository,source,fetchRecord=fetch){
  return (await candidates(token,repository,source,fetchRecord))
    .filter(run=>run.status==='completed'&&run.conclusion==='success');
}

// One settled run and its complete artifact inventory form the shared boundary
// for scheduling reuse and for accepting a consumer rehearsal.
async function settledProducer(token,repository,runId,source,fetchRecord,expected=null){
  const run=await githubRecord(token,repository,runId,fetchRecord);
  const matches=value=>String(value.id)===runId&&value.head_sha===source
    &&value.run_attempt===run.run_attempt&&value.status==='completed'&&value.conclusion==='success'
    &&value.event===run.event&&value.head_branch===run.head_branch
    &&value.path==='.github/workflows/build-release-binaries.yml'
    &&value.repository?.full_name===repository&&value.head_repository?.full_name===repository
    &&['push','schedule','workflow_dispatch','merge_group'].includes(value.event);
  if(!matches(run)||expected&&!matches(expected))refuse('source_candidate_attempt_changed');
  const attempt=await githubRecord(token,repository,runId,fetchRecord,run.run_attempt);
  if(!matches(attempt))refuse('source_candidate_attempt_unmeasured');
  const artifacts=await githubArtifactCensus(token,repository,runId,fetchRecord);
  const identity=rows=>JSON.stringify(rows.map(artifact=>({id:artifact.id,name:artifact.name,
    expired:artifact.expired,size:artifact.size_in_bytes,digest:artifact.digest,
    run:artifact.workflow_run?.id,head:artifact.workflow_run?.head_sha})).sort((a,b)=>a.id-b.id));
  return {run,artifacts,async recheck(){
    if(!matches(await githubRecord(token,repository,runId,fetchRecord)))refuse('source_candidate_attempt_changed');
    const after=await githubArtifactCensus(token,repository,runId,fetchRecord);
    if(identity(after)!==identity(artifacts))refuse('source_candidate_artifacts_changed');
    if(!matches(await githubRecord(token,repository,runId,fetchRecord)))refuse('source_candidate_attempt_changed');
  }};
}

function measuredArtifact(artifacts,name,runId,source){
  const matches=artifacts.filter(artifact=>artifact.name===name);
  const artifact=matches[0];
  if(matches.length!==1||!Number.isSafeInteger(artifact.id)||artifact.id<1
    ||artifact.expired!==false||!Number.isSafeInteger(artifact.size_in_bytes)||artifact.size_in_bytes<1
    ||!/^sha256:[0-9a-f]{64}$/.test(artifact.digest)
    ||String(artifact.workflow_run?.id)!==runId||artifact.workflow_run?.head_sha!==source){
    refuse('source_candidate_artifacts_unmeasured');
  }
  return artifact;
}

async function candidateManifest(token,repository,runId,source,snapshot,fetchRecord){
  const name=`candidate-manifest-${source}`;
  const artifact=measuredArtifact(snapshot.artifacts,name,runId,source);
  const manifest=await githubArtifactJson(token,repository,artifact,{runId,headSha:source,
    name,filename:'candidate-manifest.json'},fetchRecord);
  if(manifest?.schemaVersion!=='burin-labs.candidate_manifest.v1'||manifest.repository!==repository
    ||manifest.sourceCommit!==source||manifest.runId!==runId
    ||manifest.runAttempt!==String(snapshot.run.run_attempt))refuse('source_candidate_manifest_unmeasured');
  return {artifact,manifest};
}

/** Capture the exact completed producer before a fresh consumer rehearsal. */
export async function qualifiedProducerIdentity(token,repository,runId,source,fetchRecord=fetch){
  if(!shaPattern.test(source))refuse('source_identity_invalid');
  const snapshot=await settledProducer(token,repository,runId,source,fetchRecord);
  if(snapshot.run.head_branch!=='main')refuse('source_candidate_identity_unmeasured');
  const {artifact}=await candidateManifest(token,repository,runId,source,snapshot,fetchRecord);
  await snapshot.recheck();
  return {producer_attempt:snapshot.run.run_attempt,manifest_id:artifact.id,manifest_digest:artifact.digest};
}

/** Source reuse is a decision about one settled producer attempt, not a run ID. */
export async function reusableSourceCandidate(token,repository,source,targets,fetchRecord=fetch){
  if(!Array.isArray(targets)||targets.length===0||new Set(targets).size!==targets.length
    ||targets.some(target=>typeof target!=='string'||!/^[a-z0-9_-]+$/.test(target))){
    refuse('source_candidate_targets_unmeasured');
  }
  const names=[...targets.map(target=>`harn-${target}`),`candidate-manifest-${source}`,'harn-release-files'];
  const requiredJobs=[...targets.map(target=>`Build ${target}`),'Assemble candidate manifest'];
  for(const listed of await successfulCandidateRuns(token,repository,source,fetchRecord)){
    if(listed.head_branch!=='main')continue;
    const runId=String(listed.id);
    const snapshot=await settledProducer(token,repository,runId,source,fetchRecord,listed);
    if(snapshot.run.head_branch!=='main'||snapshot.run.event!==listed.event){
      refuse('source_candidate_identity_unmeasured');
    }
    const {artifacts}=snapshot;
    const selected=names.map(name=>artifacts.filter(artifact=>artifact.name===name));
    if(selected.every(group=>group.length===0)){
      await snapshot.recheck();
      continue;
    }
    const jobs=await githubRunJobs(token,repository,runId,listed.run_attempt,fetchRecord);
    if(jobs.length===0||requiredJobs.some(name=>{
      const matches=jobs.filter(job=>job.name===name);
      return matches.length!==1||String(matches[0].run_id)!==runId
        ||matches[0].run_attempt!==listed.run_attempt||matches[0].head_sha!==source
        ||matches[0].status!=='completed'||matches[0].conclusion!=='success';
    })){
      refuse('source_candidate_jobs_unmeasured');
    }
    for(const name of names)measuredArtifact(artifacts,name,runId,source);
    const {manifest}=await candidateManifest(token,repository,runId,source,snapshot,fetchRecord);
    if(!Array.isArray(manifest.artifacts)
      ||manifest.artifacts.filter(artifact=>artifact.kind==='archive').length!==targets.length
      ||targets.some(target=>manifest.artifacts.filter(artifact=>
        artifact.kind==='archive'&&artifact.target===target&&artifact.artifact===`harn-${target}`
        &&/^[0-9a-f]{64}$/.test(artifact.sha256)).length!==1)
      ||!manifest.artifacts.some(artifact=>artifact.artifact==='harn-release-files'
        &&/^[0-9a-f]{64}$/.test(artifact.sha256))){
      refuse('source_candidate_manifest_unmeasured');
    }
    await snapshot.recheck();
    return runId;
  }
  return '';
}

/** A rehearsal is bound to the exact settled producer, manifest and attempt. */
export async function rehearsalBinding(token,repository,runId,source,fetchRecord=fetch){
  if(!shaPattern.test(source))refuse('source_identity_invalid');
  const snapshot=await settledProducer(token,repository,runId,source,fetchRecord);
  const {run,artifacts}=snapshot;
  const jobs=await githubRunJobs(token,repository,runId,run.run_attempt,fetchRecord);
  const consumers=jobs.filter(job=>job.name==='Consumer release rehearsal / Consumer canary');
  if(consumers.length!==1||String(consumers[0].run_id)!==runId
    ||consumers[0].run_attempt!==run.run_attempt||consumers[0].head_sha!==source
    ||consumers[0].status!=='completed'||consumers[0].conclusion!=='success'){
    refuse('rehearsal_job_unmeasured');
  }
  const bindingName=`consumer-rehearsal-binding-${run.run_attempt}`;
  const bindingArtifact=measuredArtifact(artifacts,bindingName,runId,source);
  const {artifact:manifestArtifact}=await candidateManifest(token,repository,runId,source,snapshot,fetchRecord);
  const binding=await githubArtifactJson(token,repository,bindingArtifact,{runId,headSha:source,
    name:bindingName,filename:'consumer-rehearsal-binding.json'},fetchRecord);
  if(binding?.schema!=='harn.release.consumer_rehearsal_binding.v1'||binding.verdict!=='pass'
    ||binding.source_sha!==source||binding.producer_run!==runId||binding.producer_attempt!==run.run_attempt
    ||binding.manifest_digest!==manifestArtifact.digest){
    refuse('rehearsal_binding_identity_mismatch');
  }
  await snapshot.recheck();
  return {...binding,evidence:{producer_attempt:run.run_attempt,
    manifest:{id:manifestArtifact.id,digest:manifestArtifact.digest},
    binding:{id:bindingArtifact.id,digest:bindingArtifact.digest}}};
}
export async function successfulCandidate(token,repository,source,fetchRecord=fetch){
  for(const run of await successfulCandidateRuns(token,repository,source,fetchRecord)){
    const runId=String(run.id);
    const snapshot=await settledProducer(token,repository,runId,source,fetchRecord,run);
    if(!snapshot.artifacts.some(artifact=>artifact.name===`candidate-manifest-${source}`)){
      await snapshot.recheck();
      continue;
    }
    await candidateManifest(token,repository,runId,source,snapshot,fetchRecord);
    await snapshot.recheck();
    return runId;
  }
  return '';
}

/** Live recovery is scheduling evidence, never publication authorization. */
export async function activePromotions(token,consumerToken,repository,selfRun,consumer,fetchRecord=fetch,
  now=()=>Math.floor(Date.now()/1000)){
  if(!/^(?:0|[1-9][0-9]*)$/.test(selfRun))refuse('reconciliation_identity_invalid');
  const inventory=new Map();
  for(const status of ['queued','in_progress','waiting','requested','pending']){
    for(const run of await githubWorkflowRuns(token,repository,'promote-release.yml',{status},fetchRecord)){
      inventory.set(String(run.id),run);
    }
  }
  const result={schema:'harn.release.active_promotions.v1',observed:inventory.size,
    promoting:[],recoveries:[],pending_jobs:[],bad_jobs:[]};
  for(const [runId,listed] of inventory){
    if(runId===selfRun)continue;
    const selected=/^Promote release ([0-9a-f]{40})$/.exec(listed.display_title??'');
    if(!selected)refuse('promotion_source_unmeasured');
    const source=selected[1];
    const parent=await githubRecord(token,repository,runId,fetchRecord);
    if(!livePromotion(parent,repository,runId))continue;
    const parentJobs=await githubRunJobs(token,repository,runId,parent.run_attempt,fetchRecord);
    reportJobs(result,parentJobs);
    if(await successfulCandidate(token,repository,source,fetchRecord)){
      const finalParent=await githubRecord(token,repository,runId,fetchRecord);
      if(livePromotion(finalParent,repository,runId,parent))result.promoting.push(runId);
      continue;
    }
    if(repository!==observerParentRepository)continue;
    const artifacts=await githubArtifactCensus(token,repository,runId,fetchRecord);
    const declarations=artifacts.filter(artifact=>artifact.name?.startsWith(`consumer-observer-${parent.run_attempt}-`));
    if(declarations.length===0)refuse('promotion_declaration_unmeasured');
    if(declarations.length!==1)refuse('promotion_declaration_ambiguous');
    const artifact=declarations[0];
    const context=observerContext(await githubArtifactJson(token,repository,artifact,{
      runId,headSha:parent.head_sha,name:artifact.name,filename:'consumer-observer.json',
    },fetchRecord));
    if(context.source_revision!==source||artifact.name!==observerDeclarationName(context)){
      refuse('promotion_source_mismatch');
    }
    admitObserver(context,parent);
    if(parentJobs.some(job=>!Array.isArray(job.steps)))refuse('promotion_step_census_unmeasured');
    qualifyObserverPublisher(context,parentJobs);
    if(!consumerToken||!consumer?.repository||!consumer?.workflow)refuse('consumer_read_authority_unmeasured');
    const children=(await githubChildCensus(consumerToken,consumer.repository,consumer.workflow,
      parent.created_at,fetchRecord)).filter(child=>child.display_title===observerRunName(context));
    if(children.length!==1)refuse('recovery_consumer_unmeasured');
    const child=await githubRecord(consumerToken,consumer.repository,String(children[0].id),fetchRecord);
    if(child.status==='completed'&&child.conclusion!=='success')continue;
    let recovery;
    for(const producer of await candidates(token,repository,source,fetchRecord)){
      if(producer.status!=='completed'||!['success','failure'].includes(producer.conclusion))continue;
      let proof;
      try{
        proof=await githubArchiveProducer(token,{sourceRevision:source,runId:String(producer.id),
          attempt:producer.run_attempt},fetchRecord);
      }catch(error){
        if(['archive_producer_identity_unproven','archive_producer_builds_unproven',
          'archive_producer_artifacts_unproven'].includes(observerRefusal(error).reason)){
          const jobs=await githubRunJobs(token,repository,String(producer.id),producer.run_attempt,fetchRecord);
          reportJobs(result,jobs);
          continue;
        }
        throw error;
      }
      try{
        const timing=await observeConsumerTiming(consumerToken,context,{...consumer,
          runId:String(child.id),headSha:child.head_sha,branch:child.head_branch,
          producerBinding:archiveProducerBinding(proof)},now,fetchRecord);
        if(timing.owned!==1||timing.discovered!==1||timing.reason!==null){
          refuse('recovery_observation_unmeasured');
        }
        recovery={promotion_run:runId,producer_run:proof.producer_run,
          producer_attempt:proof.producer_attempt,source_sha:source,owned_consumers:1,
          observed_consumer_jobs:timing.observed_jobs,pending_consumer_jobs:timing.queued_jobs+timing.executing_jobs};
        break;
      }catch(error){
        if(observerRefusal(error).reason==='child_artifact_identity_unproven')continue;
        throw error;
      }
    }
    if(recovery){
      const finalParent=await githubRecord(token,repository,runId,fetchRecord);
      if(!livePromotion(finalParent,repository,runId,parent))continue;
      result.promoting.push(runId);result.recoveries.push(recovery);
    }
  }
  return result;
}

/** gh owns credentials, host selection, and the existing API budget wrapper. */
export async function ghRead(url,options={}){
  const endpoint=new URL(url);
  if(endpoint.origin!=='https://api.github.com'||options.method&&options.method!=='GET')refuse('read_endpoint_invalid');
  const token=options.headers?.Authorization?.replace(/^Bearer /,'');
  const env={...process.env};
  if(token&&token!==cliAuthority){env.GH_TOKEN=token;delete env.GITHUB_TOKEN;}
  const bytes=await new Promise((resolve,reject)=>{
    execFile('gh',['api',endpoint.pathname.slice(1)+endpoint.search,'--hostname','github.com'],{
      maxBuffer:16*1024*1024,env,encoding:'buffer',signal:options.signal,
    },(error,stdout)=>error?reject(new Error('CONSUMER_OBSERVER reason=github_read_unmeasured')):resolve(stdout));
  });
  return new Response(bytes,{status:200,headers:{'content-type':endpoint.pathname.endsWith('/zip')
    ?'application/octet-stream':'application/json'}});
}

if(process.argv[1]&&import.meta.url===pathToFileURL(realpathSync(process.argv[1])).href){
  try{
    const [operation,repository,sourceOrSelf]=process.argv.slice(2);
    const token=process.env.GH_TOKEN??process.env.GITHUB_TOKEN??cliAuthority;
    if(operation==='successful')console.log(await successfulCandidate(token,repository,sourceOrSelf,ghRead));
    else if(operation==='source-reuse'){
      const matrix=JSON.parse(process.argv[5]??'null');
      if(!Array.isArray(matrix)||matrix.some(row=>!row||typeof row.target!=='string')){
        refuse('source_candidate_targets_unmeasured');
      }
      console.log(await reusableSourceCandidate(token,repository,sourceOrSelf,matrix.map(row=>row.target),ghRead));
    }
    else if(operation==='producer-identity')console.log(JSON.stringify(await qualifiedProducerIdentity(token,repository,
      sourceOrSelf,process.argv[5],ghRead)));
    else if(operation==='rehearsal-binding')console.log(JSON.stringify(await rehearsalBinding(token,repository,
      sourceOrSelf,process.argv[5],ghRead)));
    else if(operation==='promotions')console.log(JSON.stringify(await activePromotions(token,
      process.env.RELEASE_RECOVERY_READ_TOKEN,repository,sourceOrSelf,{repository:process.env.CANARY_REPOSITORY,
        workflow:`.github/workflows/${process.env.CANARY_WORKFLOW}`},ghRead)));
    else refuse('operation_invalid');
  }catch(error){console.error(observerRefusal(error,'release_state_unmeasured').line);process.exitCode=1;}
}
