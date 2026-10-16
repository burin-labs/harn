import fs from 'node:fs';
import {consumerPairing,readConsumerParentProduct} from '../lib/consumer-observer.mjs';

async function main(){
  const env=process.env;
  const paired=consumerPairing(env.PAIRING_TEXT, env.CONSUMER_PULL_REQUEST);
  if(!/^[0-9a-f]{40}$/.test(env.SOURCE_REVISION??'')) throw new Error('CONSUMER_OBSERVER reason=source_identity_invalid');
  if(!env.REHEARSAL_PROMOTION_RUN_ID){
    fs.appendFileSync(env.GITHUB_OUTPUT,`reused=false\npaired_pull=${paired??''}\n`);
    console.log('CONSUMER_OBSERVER selection=fresh');
    return;
  }
  if(paired) throw new Error('CONSUMER_OBSERVER reason=reused_pairing_unsupported');
  const result=await readConsumerParentProduct(env.GH_TOKEN,{
    parentRunId:env.REHEARSAL_PROMOTION_RUN_ID,role:'promotion',sourceRevision:env.SOURCE_REVISION,
    requireCurrentDefault:true,
  },{repository:env.CANARY_REPOSITORY,workflow:`.github/workflows/${env.CANARY_WORKFLOW}`});
  fs.appendFileSync(env.GITHUB_OUTPUT,`reused=true\nverdict=pass\ncontext=${JSON.stringify(result.context)}\n`);
  console.log('CONSUMER_OBSERVER selection=authenticated_reuse owned=false');
}

main().catch(error=>{
  const reason=/^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1]??'selection_unmeasured';
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode=1;
});
