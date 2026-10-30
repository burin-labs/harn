import {spawnSync} from 'node:child_process';
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
function sourceRuns(rows,source){
  if(rows.some(row=>row.head_sha!==source||typeof row.event!=='string'
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
async function manifest(token,repository,run,source,fetchRecord){
  const name=`candidate-manifest-${source}`;
  const artifacts=await githubArtifactCensus(token,repository,String(run.id),fetchRecord,name);
  const matches=artifacts.filter(artifact=>artifact.name===name);
  if(matches.length>1) refuse('candidate_manifest_ambiguous');
  const artifact=matches[0];
  if(!artifact)return false;
  if(typeof artifact.expired!=='boolean'||!Number.isSafeInteger(artifact.size_in_bytes)
    ||artifact.size_in_bytes<1||!/^sha256:[0-9a-f]{64}$/.test(artifact.digest)
    ||String(artifact.workflow_run?.id)!==String(run.id)||artifact.workflow_run?.head_sha!==source){
    refuse('candidate_manifest_unmeasured');
  }
  return !artifact.expired;
}

/** The general selector remains success-only; all pages must be measured. */
export async function successfulCandidate(token,repository,source,fetchRecord=fetch){
  for(const run of await candidates(token,repository,source,fetchRecord)){
    if(run.status==='completed'&&run.conclusion==='success'
      &&await manifest(token,repository,run,source,fetchRecord))return String(run.id);
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
    if(!observerRunIsLive(parent.status))continue;
    if(parent.conclusion!==null||parent.path!==observerParentWorkflows.promotion||parent.head_branch!=='main'
      ||!shaPattern.test(parent.head_sha)||parent.repository?.full_name!==repository
      ||parent.head_repository?.full_name!==repository){
      refuse('promotion_identity_unmeasured');
    }
    if(await successfulCandidate(token,repository,source,fetchRecord)){
      const finalParent=await githubRecord(token,repository,runId,fetchRecord);
      if(observerRunIsLive(finalParent.status)&&finalParent.run_attempt===parent.run_attempt
        &&finalParent.head_sha===parent.head_sha)result.promoting.push(runId);
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
    const parentJobs=await githubRunJobs(token,repository,runId,parent.run_attempt,fetchRecord);
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
          'archive_producer_artifacts_unproven'].includes(observerRefusal(error).reason))continue;
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
      if(!observerRunIsLive(finalParent.status)||finalParent.run_attempt!==parent.run_attempt
        ||finalParent.head_sha!==parent.head_sha)continue;
      result.promoting.push(runId);result.recoveries.push(recovery);
      result.pending_jobs.push(...parentJobs.filter(job=>observerRunIsLive(job.status)).map(job=>job.name));
      result.bad_jobs.push(...parentJobs.filter(job=>job.status==='completed'
        &&!['success','skipped'].includes(job.conclusion)).map(job=>job.name));
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
  const response=spawnSync('gh',['api',endpoint.pathname.slice(1)+endpoint.search,'--hostname','github.com'],{
    maxBuffer:16*1024*1024,env,
  });
  if(response.status!==0)refuse('github_read_unmeasured');
  return new Response(response.stdout,{status:200,headers:{'content-type':endpoint.pathname.endsWith('/zip')
    ?'application/octet-stream':'application/json'}});
}

if(process.argv[1]&&import.meta.url===pathToFileURL(realpathSync(process.argv[1])).href){
  try{
    const [operation,repository,sourceOrSelf]=process.argv.slice(2);
    const token=process.env.GH_TOKEN??process.env.GITHUB_TOKEN??cliAuthority;
    if(operation==='successful')console.log(await successfulCandidate(token,repository,sourceOrSelf,ghRead));
    else if(operation==='promotions')console.log(JSON.stringify(await activePromotions(token,
      process.env.RELEASE_RECOVERY_READ_TOKEN,repository,sourceOrSelf,{repository:process.env.CANARY_REPOSITORY,
        workflow:`.github/workflows/${process.env.CANARY_WORKFLOW}`},ghRead)));
    else refuse('operation_invalid');
  }catch(error){console.error(observerRefusal(error,'release_state_unmeasured').line);process.exitCode=1;}
}
