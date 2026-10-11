import fs from 'node:fs';
import { githubParentDeclarations, reconcileOwnedChildren } from '../lib/consumer-observer.mjs';

async function main() {
  const env = process.env;
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
  const configured = {
    repository: `${env.CANARY_OWNER}/${env.CANARY_NAME}`,
    workflow: `.github/workflows/${env.CANARY_WORKFLOW}`,
  };
  const total = { discovered: 0, owned: 0, terminal: 0, cancelled: 0, pending: 0, refused: 0 };
  for (const context of contexts) {
    const result = await reconcileOwnedChildren(env.GH_TOKEN, context, configured);
    for (const key of Object.keys(total)) total[key] += result[key];
  }
  console.log(`CONSUMER_OBSERVER cleanup=measured declarations=${contexts.length} ${Object.entries(total).map(([key, value]) => `${key}=${value}`).join(' ')}`);
  if (total.pending > 0 || total.refused > 0) process.exitCode = 1;
}

main().catch(error => {
  const reason = /^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1] ?? 'cleanup_unmeasured';
  // Private network errors can contain repository/run identities. Only the
  // closed reason is public, never arbitrary exception prose or a stack.
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode = 1;
});
