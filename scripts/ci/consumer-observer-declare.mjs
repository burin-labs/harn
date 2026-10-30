import fs from 'node:fs';
import path from 'node:path';
import { admitObserver, createObserverContext, githubRecord, githubDeclaration, observerContext, observerDeclarationName, parentHeadSha, observerRefusal } from '../lib/consumer-observer.mjs';

async function main() {
  const env = process.env;
  if (env.GITHUB_REPOSITORY !== 'burin-labs/harn') {
    throw new Error('CONSUMER_OBSERVER reason=parent_identity_unproven');
  }
  const identity = {
    runId: env.GITHUB_RUN_ID, attempt: Number(env.GITHUB_RUN_ATTEMPT),
    headSha: parentHeadSha(env), sourceRevision: env.SOURCE_REVISION,
  };
  const witnessing = process.argv[2] === '--witness';
  const context = witnessing ? observerContext(JSON.parse(env.OBSERVER_CONTEXT)) : createObserverContext(identity);
  if (context.parent_run_id !== identity.runId || context.parent_run_attempt !== identity.attempt
    || context.parent_head_sha !== identity.headSha || context.source_revision !== identity.sourceRevision) {
    throw new Error('CONSUMER_OBSERVER reason=observer_invocation_unproven');
  }
  const parent = await githubRecord(env.GH_TOKEN, context.parent_repository, context.parent_run_id);
  admitObserver(context, parent);
  if (witnessing) {
    await githubDeclaration(env.GH_TOKEN, context);
    console.log('CONSUMER_OBSERVER publication=authenticated parent=live attempt=current');
    return;
  }
  const directory = fs.mkdtempSync(path.join(env.RUNNER_TEMP, 'consumer-observer-public-'));
  const file = path.join(directory, 'consumer-observer.json');
  fs.writeFileSync(file, `${JSON.stringify(context)}\n`, { mode: 0o600, flag: 'wx' });
  fs.appendFileSync(env.GITHUB_OUTPUT,
    `file=${file}\nname=${observerDeclarationName(context)}\ncontext=${JSON.stringify(context)}\n`);
  console.log('CONSUMER_OBSERVER declaration=prepared parent=live attempt=current');
}

main().catch(error => {
  const {line} = observerRefusal(error, 'declaration_unmeasured');
  console.error(`::error::${line}`);
  process.exitCode = 1;
});
