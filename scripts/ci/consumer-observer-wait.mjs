import fs from 'node:fs';
import {observeConsumerProduct,observerContext} from '../lib/consumer-observer.mjs';
import {waitConsumerWindow} from '../lib/consumer-observer-window.mjs';

async function main(){
  const env=process.env;
  const context=observerContext(JSON.parse(env.OBSERVER_CONTEXT));
  if(!/^[0-9]+$/.test(env.CANARY_STARTED_AT??'')) throw new Error('CONSUMER_OBSERVER reason=observation_start_invalid');
  const expected={repository:env.CANARY_REPOSITORY,workflow:`.github/workflows/${env.CANARY_WORKFLOW}`,
    pairedPull:env.CANARY_PAIRED_PULL||null};
  const result=await waitConsumerWindow(Number(env.CANARY_STARTED_AT),()=>observeConsumerProduct(env.GH_TOKEN,context,expected));
  if(env.GITHUB_OUTPUT){
    // Only an actually reached product judgment is a release verdict.
    const verdict=result.verdict==='pass'||result.verdict==='fail'?`verdict=${result.verdict}\n`:'';
    fs.appendFileSync(env.GITHUB_OUTPUT,`${verdict}observation_outcome=${result.verdict}\nwall_seconds=${result.wallSeconds}\n`);
  }
  const reading=result.reading;
  console.log(`CONSUMER_OBSERVER outcome=${result.verdict} wall_seconds=${result.wallSeconds} discovered=${reading?.discovered??'unmeasured'} owned=${reading?.owned??'unmeasured'} pending=${reading?.pending??'unmeasured'} refused=${reading?.refused??'unmeasured'}`);
  if(result.verdict!=='pass'&&result.verdict!=='pending') process.exitCode=1;
}

main().catch(error=>{
  const reason=/^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1]??'product_observation_unmeasured';
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode=1;
});
