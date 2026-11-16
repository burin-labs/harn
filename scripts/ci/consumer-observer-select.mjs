import fs from 'node:fs';
import {githubConsumerSelection,readConsumerParentProduct, observerRefusal } from '../lib/consumer-observer.mjs';
import {consumerSourceDeclaration} from '../lib/consumer-source.mjs';

async function main(){
  const env=process.env;
  const declaration=consumerSourceDeclaration(env.PAIRING_TEXT, env.CONSUMER_PULL_REQUEST);
  const paired=declaration.pull;
  if(!/^[0-9a-f]{40}$/.test(env.SOURCE_REVISION??'')) throw new Error('CONSUMER_OBSERVER reason=source_identity_invalid');
  if(!env.REHEARSAL_PROMOTION_RUN_ID){
    if(declaration.kind==='composition') await githubConsumerSelection(env.GH_TOKEN,env.CANARY_REPOSITORY,declaration);
    fs.appendFileSync(env.GITHUB_OUTPUT,`reused=false\npaired_pull=${paired??''}\nsource_declaration=${JSON.stringify(declaration)}\n`);
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
  const {line} = observerRefusal(error, 'selection_unmeasured');
  console.error(`::error::${line}`);
  process.exitCode=1;
});
