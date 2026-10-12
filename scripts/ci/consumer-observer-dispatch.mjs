import {dispatchObserverConsumer,observerContext} from '../lib/consumer-observer.mjs';

async function main() {
  const env=process.env;
  const context=observerContext(JSON.parse(env.OBSERVER_CONTEXT));
  if(context.parent_repository!==env.GITHUB_REPOSITORY || context.parent_run_id!==env.GITHUB_RUN_ID
    || context.parent_run_attempt!==Number(env.GITHUB_RUN_ATTEMPT) || context.parent_head_sha!==env.GITHUB_SHA){
    throw new Error('CONSUMER_OBSERVER reason=dispatch_invocation_unproven');
  }
  const result=await dispatchObserverConsumer(env.PARENT_READ_TOKEN, env.GH_TOKEN, context, {
    repository:env.CANARY_REPOSITORY, workflow:env.CANARY_WORKFLOW,
    ref:env.CANARY_REF, target:env.CANARY_TARGET,
  });
  console.log(`CONSUMER_OBSERVER dispatched=${result.dispatched} child_identity=private`);
}

main().catch(error=>{
  const reason=/^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1]??'dispatch_unmeasured';
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode=1;
});
