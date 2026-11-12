import {githubRecord, githubRunJobs, observerConclusionIsKnown, observerRunIsLive} from './consumer-observer.mjs';

export const releaseWorkflows = Object.freeze({candidate:'build-release-binaries.yml', promotion:'promote-release.yml'});
export const releaseWaitMinutes = Object.freeze({candidate:210, promotion:120});
const sha = /^[0-9a-f]{40}$/;
const marker = number => `<!-- harn.release-on-demand.v1 pr=${number} -->`;
const refuse = reason => { throw new Error(reason); };

export async function releaseRequest(api, repository, number, dryRun=false) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository) || !Number.isSafeInteger(number) || number<1) refuse('request_identity_invalid');
  const pull = await api.read(`repos/${repository}/pulls/${number}`);
  if (pull.number!==number || pull.merged!==true || pull.state!=='closed' || pull.base?.ref!=='main'
    || pull.base?.repo?.full_name!==repository || !sha.test(pull.merge_commit_sha)
    || !Array.isArray(pull.labels)) refuse('merged_main_request_unmeasured');
  if (!dryRun && !pull.labels.some(label=>label.name==='release-needed')) refuse('release_needed_label_missing');
  const main = await api.read(`repos/${repository}/git/ref/heads/main`);
  if (!sha.test(main.object?.sha)) refuse('main_identity_unmeasured');
  await containsMerge(api,repository,pull.merge_commit_sha,main.object.sha);
  return {repository,pr:number,merge_sha:pull.merge_commit_sha,source_sha:main.object.sha};
}

async function containsMerge(api, repository, merge, source) {
  const comparison=await api.read(`repos/${repository}/compare/${merge}...${source}`);
  if (!['ahead','identical'].includes(comparison.status) || comparison.base_commit?.sha!==merge
    || comparison.merge_base_commit?.sha!==merge) refuse('release_source_does_not_contain_merge');
}

/** Read the exact attempt through the shared complete, unique job census. */
export async function releaseObservation(api, request, phase, id) {
  const run=await githubRecord(api.token,request.repository,String(id),api.fetch);
  if (run.id!==id || run.run_attempt!==1 || run.event!=='workflow_dispatch' || run.head_branch!=='main'
    || run.repository?.full_name!==request.repository || run.head_repository?.full_name!==request.repository
    || run.path!==`.github/workflows/${releaseWorkflows[phase]}` || !sha.test(run.head_sha)) refuse(`${phase}_identity_or_attempt_unmeasured`);
  if (phase==='candidate') {
    if (run.display_title!=='Build release binaries [source-candidate]') refuse('candidate_purpose_unmeasured');
    await containsMerge(api,request.repository,request.merge_sha,run.head_sha);
  } else if (run.display_title!==`Promote release ${request.source_sha}`) refuse('promotion_source_unmeasured');
  const jobs=await githubRunJobs(api.token,request.repository,String(id),1,api.fetch);
  if (jobs.some(job=>job.run_id!==id || job.run_attempt!==1 || job.head_sha!==run.head_sha
    || typeof job.name!=='string' || !job.name
    || !(observerRunIsLive(job.status) && job.conclusion===null
      || job.status==='completed' && observerConclusionIsKnown(job.conclusion)))) refuse(`${phase}_job_identity_or_state_unmeasured`);
  const pending=jobs.filter(job=>observerRunIsLive(job.status)).map(job=>job.name);
  const failing=jobs.filter(job=>job.status==='completed' && !['success','skipped'].includes(job.conclusion)).map(job=>job.name);
  const result={phase,run_id:id,source_sha:phase==='candidate'?run.head_sha:request.source_sha,
    status:run.status,conclusion:run.conclusion,observed_jobs:jobs.length,pending_jobs:pending,failing_jobs:failing};
  if (run.status==='completed') {
    if (!observerConclusionIsKnown(run.conclusion)) refuse(`${phase}_outcome_unmeasured`);
    if (!jobs.length) result.pending_jobs.push(`${phase} job census has no reported jobs`);
    result.verdict=run.conclusion==='success' && jobs.length>0 && pending.length===0 && failing.length===0
      && jobs.some(job=>job.conclusion==='success')?'passed':'blocked';
  } else {
    if (!observerRunIsLive(run.status) || run.conclusion!==null) refuse(`${phase}_outcome_unmeasured`);
    if (!jobs.length) result.pending_jobs.push(`${phase} jobs have not reported`);
    result.verdict=failing.length?'blocked':'pending';
  }
  return result;
}

/** This caller schedules owners; their existing gates alone authorize publication. */
export async function runRequestedRelease(api, request, {dryRun=false,now=Date.now,
  sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms)),progress=async()=>{}}={}) {
  const state={schema:'harn.release_on_demand.v1',...request,dry_run:dryRun,verdict:'pending',
    phase:'candidate',observed_jobs:0,pending_jobs:['candidate dispatch'],failing_jobs:[]};
  await progress(state);
  if (dryRun) {
    state.verdict='dry-run';state.pending_jobs=['candidate and promotion are planned, not dispatched'];
    state.dispatch_plan=[{workflow:releaseWorkflows.candidate,ref:'main',inputs:{source_candidate:true}},
      {workflow:releaseWorkflows.promotion,ref:'main',inputs:{candidate_sha:request.source_sha,candidate_run_id:'<returned candidate run ID>'}}];
    await progress(state);return state;
  }
  try {
    for (const phase of ['candidate','promotion']) {
      state.phase=phase;state.verdict='pending';state.pending_jobs=[`${phase} dispatch`];state.failing_jobs=[];
      const inputs=phase==='candidate'?{source_candidate:true}:
        {candidate_sha:state.source_sha,candidate_run_id:String(state.candidate_run_id)};
      const dispatched=await api.dispatch(releaseWorkflows[phase],inputs);
      const id=dispatched?.workflow_run_id;
      if (!Number.isSafeInteger(id) || id<1
        || dispatched.run_url!==`https://api.github.com/repos/${request.repository}/actions/runs/${id}`
        || dispatched.html_url!==`https://github.com/${request.repository}/actions/runs/${id}`) refuse(`${phase}_dispatch_identity_unmeasured`);
      state[`${phase}_run_id`]=id;
      await progress(state);
      const deadline=now()+releaseWaitMinutes[phase]*60000;
      for (;;) {
        Object.assign(state,await releaseObservation(api,{...request,source_sha:state.source_sha},phase,id));
        await progress(state);
        if (state.verdict==='blocked') refuse(`${phase}_did_not_pass`);
        if (state.verdict==='passed') break;
        if (now()>=deadline) refuse(`${phase}_deadline_exceeded`);
        await sleep(Math.min(60000,deadline-now()));
      }
      if (phase==='candidate') {
        // The established producer owner rechecks the manifest and rehearsal's
        // exact attempt. A successful partial rerun never becomes a new producer.
        await api.qualify(String(id),state.source_sha);
      }
    }
    state.verdict='complete';state.pending_jobs=[];
  } catch (error) {
    state.verdict='blocked';state.reason=error.message;
    if (!state.pending_jobs.length && !state.failing_jobs.length) state.pending_jobs=[`${state.phase} qualification is unmeasured`];
  }
  await progress(state);
  return state;
}

export function releaseReport(state, controllerUrl) {
  return `${marker(state.pr)}\nRelease request: **${state.verdict}** (${state.phase}).\n\n`
    +`Source: \`${state.source_sha??'unmeasured'}\`. [Controller](${controllerUrl}).\n\n`
    +['candidate','promotion'].filter(phase=>state[`${phase}_run_id`]).map(phase=>
      `${phase}: https://github.com/${state.repository}/actions/runs/${state[`${phase}_run_id`]}`).join('\n')
    +`\n\nObserved jobs: ${state.observed_jobs??0}. Pending (${state.pending_jobs.length}): ${state.pending_jobs.join(', ')||'none'}.`
    +` Failing (${state.failing_jobs.length}): ${state.failing_jobs.join(', ')||'none'}.`
    +(state.reason?`\n\nReason: \`${state.reason}\`.`:'')
    +(state.verdict==='blocked'?'\n\nInspect the named run and repair its cause. Start a fresh release request after repair; do not partially rerun the candidate.':'');
}

/** One issue per merged request, updated across failures rather than per poll. */
export async function reportRequestedRelease(api, state, controllerUrl) {
  const body=releaseReport(state,controllerUrl);
  const comment=async()=>{
    const comments=(await api.list(`repos/${state.repository}/issues/${state.pr}/comments`))
      .filter(row=>row.user?.login==='github-actions[bot]' && row.body?.startsWith(marker(state.pr)));
    if (comments.length>1) refuse('request_comment_ambiguous');
    await api.write(comments.length?`repos/${state.repository}/issues/comments/${comments[0].id}`:
      `repos/${state.repository}/issues/${state.pr}/comments`,{body},comments.length?'PATCH':'POST');
  };
  const issue=async()=>{
    const issues=(await api.list(`repos/${state.repository}/issues?state=all&labels=release-blocked`))
      .filter(row=>!row.pull_request && row.user?.login==='github-actions[bot]' && row.body?.startsWith(marker(state.pr)));
    if (issues.length>1) refuse('release_blocked_issue_ambiguous');
    if (state.verdict==='blocked') {
      await api.ensureBlockedLabel();
      await api.write(issues.length?`repos/${state.repository}/issues/${issues[0].number}`:`repos/${state.repository}/issues`,
        {title:`Release blocked for #${state.pr}`,body,labels:['release-blocked'],state:'open'},issues.length?'PATCH':'POST');
    } else if (state.verdict==='complete' && issues.length) {
      await api.write(`repos/${state.repository}/issues/${issues[0].number}`,{state:'closed'},'PATCH');
    }
  };
  const results=await Promise.allSettled([comment(),issue()]);
  if(results.some(result=>result.status==='rejected'))refuse('release_notification_incomplete');
}
