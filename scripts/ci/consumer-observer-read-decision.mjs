import fs from 'node:fs';
import {observerContext,readConsumerContextDecision,readConsumerRepairDecision, observerRefusal } from '../lib/consumer-observer.mjs';
import {readConsumerDeadline,readConsumerRecoveryDecision} from '../lib/consumer-observer-window.mjs';

async function main(){
  const env=process.env;
  const args=process.argv.slice(2);
  if(args.length>1||(args.length===1&&!['--deadline','--recover','--repair'].includes(args[0]))){
    throw new Error('CONSUMER_OBSERVER reason=parent_selection_unproven');
  }
  if(env.REQUIRE_CURRENT_DEFAULT!=='true'&&env.REQUIRE_CURRENT_DEFAULT!=='false'){
    throw new Error('CONSUMER_OBSERVER reason=parent_selection_unproven');
  }
  if(args[0]==='--repair'){
    if(env.REQUIRE_CURRENT_DEFAULT!=='false') throw new Error('CONSUMER_OBSERVER reason=consumer_repair_request_unproven');
    const request=JSON.parse(env.OBSERVER_CONTEXT??'');
    const result=await readConsumerRepairDecision(env.GH_TOKEN,request,{
      repository:env.CANARY_REPOSITORY,workflow:`.github/workflows/${env.CANARY_WORKFLOW}`,
      sourceRevision:env.SOURCE_REVISION,
    });
    if(env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT,`repair_request=${JSON.stringify(request)}\n`);
    console.log(JSON.stringify(result));
    return;
  }
  const context=observerContext(JSON.parse(env.OBSERVER_CONTEXT??''));
  if(context.source_revision!==env.SOURCE_REVISION){
    throw new Error('CONSUMER_OBSERVER reason=parent_selection_source_unproven');
  }
  if(args[0]==='--deadline'){
    // This operation authenticates the original historical deadline only.
    // Current consumer-source eligibility belongs to the later product read.
    if(env.REQUIRE_CURRENT_DEFAULT!=='false'){
      throw new Error('CONSUMER_OBSERVER reason=parent_selection_unproven');
    }
    const deadline=await readConsumerDeadline(env.GH_TOKEN,context);
    console.log(JSON.stringify({context,deadline,owned:false}));
    return;
  }
  if(args[0]==='--recover'&&env.REQUIRE_CURRENT_DEFAULT!=='true'){
    throw new Error('CONSUMER_OBSERVER reason=parent_selection_unproven');
  }
  const reader=args[0]==='--recover'?readConsumerRecoveryDecision:readConsumerContextDecision;
  const result=await reader(env.GH_TOKEN,context,{
    repository:env.CANARY_REPOSITORY,workflow:`.github/workflows/${env.CANARY_WORKFLOW}`,
    requireCurrentDefault:env.REQUIRE_CURRENT_DEFAULT==='true',
  });
  if(env.GITHUB_OUTPUT){
    fs.appendFileSync(env.GITHUB_OUTPUT,`context=${JSON.stringify(result.context)}\nreading=${JSON.stringify(result.reading)}\nowned=false\n`);
  }
  console.log(JSON.stringify(result));
}

main().catch(error=>{
  const {line} = observerRefusal(error, 'context_product_unmeasured');
  console.error(`::error::${line}`);
  process.exitCode=1;
});
