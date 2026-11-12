import fs from 'node:fs';
import {execFile} from 'node:child_process';
import {realpathSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
import {ghRead,rehearsalBinding} from './lib/release-candidate-state.mjs';
import {releaseRequest,runRequestedRelease,reportRequestedRelease,releaseObservation} from './lib/release-on-demand.mjs';

export function releaseApi(repository) {
  const token=process.env.GH_TOKEN??process.env.GITHUB_TOKEN??'gh-cli-authenticated-read';
  const write=(endpoint,body,method='POST')=>new Promise((resolve,reject)=>{
    const child=execFile('gh',['api',endpoint,'--method',method,'--input','-',
      '-H','X-GitHub-Api-Version: 2026-03-10'],{encoding:'utf8',maxBuffer:4*1024*1024,timeout:30000},
    (error,stdout)=>{
      if(error)return reject(new Error('github_write_unmeasured'));
      try {resolve(stdout?JSON.parse(stdout):null);} catch {reject(new Error('github_write_response_unmeasured'));}
    });
    child.stdin.end(JSON.stringify(body));
  });
  const read=async endpoint=>(await ghRead(`https://api.github.com/${endpoint}`,{signal:AbortSignal.timeout(30000)})).json();
  return {token,fetch:ghRead,read,write,
    dispatch:(workflow,inputs)=>write(`repos/${repository}/actions/workflows/${workflow}/dispatches`,
      {ref:'main',inputs,return_run_details:true}),
    qualify:(id,source)=>rehearsalBinding(token,repository,id,source,ghRead),
    async ensureBlockedLabel(){
      const labels=await this.list(`repos/${repository}/labels`);
      if (!labels.some(label=>label.name==='release-blocked')) await write(`repos/${repository}/labels`,
        {name:'release-blocked',color:'B60205',description:'A requested release needs intervention'});
    },
    async list(endpoint){
      const rows=[];
      for (let page=1;page<=10;page++) {
        const values=await read(`${endpoint}${endpoint.includes('?')?'&':'?'}per_page=100&page=${page}`);
        if (!Array.isArray(values) || values.length>100 || values.some(row=>!Number.isSafeInteger(row.id)||row.id<1)) throw new Error('github_list_unmeasured');
        rows.push(...values);
        if(new Set(rows.map(row=>row.id)).size!==rows.length)throw new Error('github_list_duplicate');
        if(values.length<100)return rows;
      }
      throw new Error('github_list_budget_exhausted');
    }};
}

async function main() {
  const repository=process.env.GITHUB_REPOSITORY;
  const number=Number(process.env.RELEASE_REQUEST_PR);
  const dryRun=process.env.RELEASE_REQUEST_DRY_RUN==='true';
  const filename=process.env.RELEASE_REQUEST_REPORT??'release-request.json';
  const api=releaseApi(repository);
  const controller=`https://github.com/${repository}/actions/runs/${process.env.GITHUB_RUN_ID??'0'}`;
  if (process.argv[2]==='report') {
    let state;
    try {state=JSON.parse(fs.readFileSync(filename,'utf8'));} catch { /* A missing receipt is unmeasured. */ }
    if (!state || state.schema!=='harn.release_on_demand.v1' || state.pr!==number || state.repository!==repository
      || !Array.isArray(state.pending_jobs) || !Array.isArray(state.failing_jobs)) {
      state={schema:'harn.release_on_demand.v1',repository,pr:number,verdict:'blocked',phase:'controller',
        reason:'controller_failed_or_timed_out',observed_jobs:0,pending_jobs:['candidate/promotion census unavailable'],
        failing_jobs:['Release request controller']};
    }
    if (!Number.isSafeInteger(state.pr)||state.pr<1||state.repository!==repository)throw new Error('report_identity_invalid');
    if (!['complete','dry-run','blocked'].includes(state.verdict)) {
      state.verdict='blocked';state.reason='controller_failed_or_timed_out';
      if (!state.pending_jobs.length)state.pending_jobs=['candidate/promotion completion is unmeasured'];
    }
    if (!dryRun)await reportRequestedRelease(api,state,controller);
    console.log(JSON.stringify(state));return;
  }
  let state;
  const progress=async value=>{
    state=structuredClone(value);
    fs.writeFileSync(filename+'.tmp',JSON.stringify(state,null,2)+'\n');fs.renameSync(filename+'.tmp',filename);
  };
  try {
    const request=await releaseRequest(api,repository,number,dryRun);
    state=await runRequestedRelease(api,request,{dryRun,progress});
    if (dryRun && process.env.RELEASE_REQUEST_INSPECT_RUN) {
      state.live_observation=await releaseObservation(api,request,'candidate',Number(process.env.RELEASE_REQUEST_INSPECT_RUN));
      await progress(state);
    }
  } catch(error) {
    await progress({...state,schema:'harn.release_on_demand.v1',repository,pr:number,verdict:'blocked',phase:state?.phase??'request',
      reason:error.message,observed_jobs:state?.observed_jobs??0,pending_jobs:state?.pending_jobs??['release request unmeasured'],
      failing_jobs:state?.failing_jobs??[]});
  }
  console.log(JSON.stringify(state));
  if(state.verdict==='blocked')process.exitCode=1;
}

if(process.argv[1]&&import.meta.url===pathToFileURL(realpathSync(process.argv[1])).href) {
  main().catch(()=>{console.error('Release request failed; inspect the controller report.');process.exitCode=1;});
}
