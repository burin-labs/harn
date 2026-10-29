import fs from 'node:fs';
import { githubParentDeclarations, reconcileOwnedChildren, reconcileStoppedObserver, sweepOwnedChildren, observerContext, parentHeadSha, observerRefusal } from '../lib/consumer-observer.mjs';

function reportPending(result) {
  if (result.pending > 0 || result.refused > 0) {
    console.log(`::warning::Consumer cleanup remains pending: pending=${result.pending} refused=${result.refused}. No unverified child was cancelled.`);
  }
  if (result.errors > 0) process.exitCode = 1;
}

async function main() {
  const env = process.env;
  const configured = {
    repository: `${env.CANARY_OWNER}/${env.CANARY_NAME}`,
    workflow: `.github/workflows/${env.CANARY_WORKFLOW}`,
  };
  if (['schedule', 'workflow_dispatch'].includes(env.GITHUB_EVENT_NAME)) {
    if (env.GITHUB_REPOSITORY !== 'burin-labs/harn') throw new Error('CONSUMER_OBSERVER reason=cleanup_invocation_unproven');
    const result = await sweepOwnedChildren(env.GH_TOKEN, configured);
    console.log(`CONSUMER_OBSERVER cleanup=sweep ${Object.entries(result).map(([key,value])=>`${key}=${value}`).join(' ')}`);
    reportPending(result);
    return;
  }
  if (process.argv[2] === '--observer') {
    const context = observerContext(JSON.parse(env.OBSERVER_CONTEXT));
    if (context.parent_repository !== env.GITHUB_REPOSITORY || context.parent_run_id !== env.GITHUB_RUN_ID
      || context.parent_run_attempt !== Number(env.GITHUB_RUN_ATTEMPT) || context.parent_head_sha !== parentHeadSha(env)) {
      throw new Error('CONSUMER_OBSERVER reason=observer_invocation_unproven');
    }
    const result = await reconcileStoppedObserver(env.GH_TOKEN, context, configured);
    console.log(`CONSUMER_OBSERVER cleanup=measured declarations=1 ${Object.entries(result).map(([key,value])=>`${key}=${value}`).join(' ')}`);
    if (result.pending > 0 || result.refused > 0) process.exitCode = 1;
    return;
  }
  const event = JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, 'utf8'));
  // The default-branch completion listener supplies GitHub's authenticated
  // workflow_run record. No arbitrary run-name or manual identity input acts
  // as cancellation authority.
  if (env.GITHUB_EVENT_NAME !== 'workflow_run' || env.GITHUB_REPOSITORY !== 'burin-labs/harn'
    || event?.repository?.full_name !== env.GITHUB_REPOSITORY
    || event?.action !== 'completed' || event?.workflow_run?.status !== 'completed') {
    throw new Error('CONSUMER_OBSERVER reason=completion_event_unproven');
  }
  const run = event.workflow_run;
  const contexts = await githubParentDeclarations(env.GH_TOKEN, String(run.id), run.run_attempt);
  if (contexts.length === 0) {
    console.log('CONSUMER_OBSERVER cleanup=ordinary declarations=0');
    return;
  }
  const total = { discovered: 0, owned: 0, terminal: 0, cancelled: 0, pending: 0, refused: 0, errors: 0 };
  for (const context of contexts) {
    const result = await reconcileOwnedChildren(env.GH_TOKEN, context, configured);
    for (const key of Object.keys(total)) total[key] += result[key];
  }
  console.log(`CONSUMER_OBSERVER cleanup=measured declarations=${contexts.length} ${Object.entries(total).map(([key, value]) => `${key}=${value}`).join(' ')}`);
  reportPending(total);
}

main().catch(error => {
  const {line} = observerRefusal(error, 'cleanup_unmeasured');
  // Private network errors can contain repository/run identities. Only the
  // closed reason is public, never arbitrary exception prose or a stack.
  console.error(`::error::${line}`);
  process.exitCode = 1;
});
