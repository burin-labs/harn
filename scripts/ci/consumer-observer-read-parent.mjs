import fs from 'node:fs';
import {readConsumerParentDecision,readConsumerParentProduct} from '../lib/consumer-observer.mjs';

async function main(){
  const env=process.env;
  const args=process.argv.slice(2);
  if(args.length>1 || (args.length===1 && args[0]!=='--decision')){
    throw new Error('CONSUMER_OBSERVER reason=parent_selection_unproven');
  }
  if(env.REQUIRE_CURRENT_DEFAULT!=='true' && env.REQUIRE_CURRENT_DEFAULT!=='false'){
    throw new Error('CONSUMER_OBSERVER reason=parent_selection_unproven');
  }
  const read=args.length===1?readConsumerParentDecision:readConsumerParentProduct;
  const result=await read(env.GH_TOKEN,{
    parentRunId:env.PARENT_RUN_ID,role:env.PARENT_ROLE,sourceRevision:env.SOURCE_REVISION,
    requireCurrentDefault:env.REQUIRE_CURRENT_DEFAULT==='true',
  },{repository:env.CANARY_REPOSITORY,workflow:`.github/workflows/${env.CANARY_WORKFLOW}`});
  if(env.GITHUB_OUTPUT){
    fs.appendFileSync(env.GITHUB_OUTPUT,`context=${JSON.stringify(result.context)}\nreading=${JSON.stringify(result.reading)}\nowned=false\n`);
  }
  console.log(JSON.stringify(result));
}

main().catch(error=>{
  const reason=/^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1]??'parent_product_unmeasured';
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode=1;
});
