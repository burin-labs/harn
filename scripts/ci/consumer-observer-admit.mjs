import fs from 'node:fs';
import path from 'node:path';
import { admitChild, admitObserver, githubDeclaration, githubRecord, observerContext } from '../lib/consumer-observer.mjs';

async function main() {
  const env = process.env;
  if (!env.OBSERVER_CONTEXT) {
    fs.appendFileSync(env.GITHUB_OUTPUT, 'owned=false\n');
    console.log('CONSUMER_OBSERVER admission=ordinary');
    return;
  }
  const context = observerContext(JSON.parse(env.OBSERVER_CONTEXT));
  const event = JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, 'utf8'));
  const workflowPrefix = `${env.GITHUB_REPOSITORY}/`;
  if (!env.GITHUB_WORKFLOW_REF?.startsWith(workflowPrefix)) {
    throw new Error('CONSUMER_OBSERVER reason=workflow_identity_unproven');
  }
  const parent = await githubRecord(env.GH_TOKEN, context.parent_repository, context.parent_run_id);
  admitObserver(context, parent);
  const declaration = await githubDeclaration(env.GH_TOKEN, context);
  const [finalParent, child] = await Promise.all([
    githubRecord(env.GH_TOKEN, context.parent_repository, context.parent_run_id),
    githubRecord(env.GH_TOKEN, env.GITHUB_REPOSITORY, env.GITHUB_RUN_ID),
  ]);
  const receipt = admitChild(context, finalParent, {
    repository: env.GITHUB_REPOSITORY,
    runId: env.GITHUB_RUN_ID,
    attempt: Number(env.GITHUB_RUN_ATTEMPT),
    headSha: env.GITHUB_SHA,
    workflow: env.GITHUB_WORKFLOW_REF.slice(workflowPrefix.length).split('@')[0],
    sourceRevision: env.SOURCE_REVISION,
  }, child, event, declaration);
  const file = path.join(env.RUNNER_TEMP, `consumer-observer-${context.dispatch_nonce}.json`);
  fs.writeFileSync(file, `${JSON.stringify(receipt)}\n`, { flag: 'wx', mode: 0o600 });
  fs.appendFileSync(env.GITHUB_OUTPUT, `owned=true\nreceipt=${file}\nkey=${context.dispatch_nonce}\n`);
  console.log('CONSUMER_OBSERVER admission=owned parent=live attempt=current');
}

main().catch(error => {
  const reason = /^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1] ?? 'admission_unmeasured';
  // Network and JSON errors can contain a private URL or input. Emit only the
  // closed diagnostic name, never an arbitrary error message or stack trace.
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode = 1;
});
