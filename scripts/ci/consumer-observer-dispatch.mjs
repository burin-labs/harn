import {admitObserverDispatch,dispatchObserverConsumer,githubConsumerSource,observerContext} from '../lib/consumer-observer.mjs';

async function main() {
  const env=process.env;
  const context=observerContext(JSON.parse(env.OBSERVER_CONTEXT));
  if(context.parent_repository!==env.GITHUB_REPOSITORY || context.parent_run_id!==env.GITHUB_RUN_ID
    || context.parent_run_attempt!==Number(env.GITHUB_RUN_ATTEMPT) || context.parent_head_sha!==env.GITHUB_SHA
    || (env.SOURCE_REVISION && context.source_revision!==env.SOURCE_REVISION)){
    throw new Error('CONSUMER_OBSERVER reason=dispatch_invocation_unproven');
  }
  const admissionOnly=process.argv.length===3 && process.argv[2]==='--admit';
  if(process.argv.length!==2 && !admissionOnly) throw new Error('CONSUMER_OBSERVER reason=dispatch_arguments_invalid');
  const source=admissionOnly ? {branch:env.CANARY_REF}
    : await githubConsumerSource(env.GH_TOKEN,env.CANARY_REPOSITORY,env.CANARY_PAIRED_PULL||null);
  const target={
    repository:env.CANARY_REPOSITORY, workflow:env.CANARY_WORKFLOW,
    ref:source.branch, target:env.CANARY_TARGET,
  };
  if(admissionOnly){
    const admitted=await admitObserverDispatch(env.PARENT_READ_TOKEN,env.GH_TOKEN,context,target);
    process.stdout.write(JSON.stringify(admitted));
    return;
  }
  const result=await dispatchObserverConsumer(env.PARENT_READ_TOKEN, env.GH_TOKEN, context,target);
  if(env.GITHUB_OUTPUT){
    const fs=await import('node:fs');
    fs.appendFileSync(env.GITHUB_OUTPUT,`context=${JSON.stringify(context)}\n`);
  }
  console.log(`CONSUMER_OBSERVER dispatched=${result.dispatched} child_identity=private`);
}

main().catch(error=>{
  const reason=/^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1]??'dispatch_unmeasured';
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode=1;
});
