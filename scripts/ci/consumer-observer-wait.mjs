import fs from 'node:fs';
import path from 'node:path';
import {observeConsumerProduct,observerContext, observerRefusal } from '../lib/consumer-observer.mjs';
import {consumerDeadlineReceipt,observerDeadlineFilename,observerDeadlineName,waitLegacyConsumerWindow} from '../lib/consumer-observer-window.mjs';

async function main(){
  const env=process.env;
  const context=observerContext(JSON.parse(env.OBSERVER_CONTEXT));
  const expected={repository:env.CANARY_REPOSITORY,workflow:`.github/workflows/${env.CANARY_WORKFLOW}`,
    pairedPull:env.CANARY_PAIRED_PULL||null};
  const result=await waitLegacyConsumerWindow(context.started_at,()=>observeConsumerProduct(env.GH_TOKEN,context,expected));
  if(result.verdict==='deadline'){
    const receipt=consumerDeadlineReceipt(context,result.observedAt,result.reading);
    const directory=fs.mkdtempSync(path.join(env.RUNNER_TEMP,'consumer-observer-deadline-'));
    const file=path.join(directory,observerDeadlineFilename);
    fs.writeFileSync(file,`${JSON.stringify(receipt)}\n`,{mode:0o600,flag:'wx'});
    if(env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT,
      `deadline_file=${file}\ndeadline_name=${observerDeadlineName(context)}\n`);
  }
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
  const {line} = observerRefusal(error, 'product_observation_unmeasured');
  console.error(`::error::${line}`);
  process.exitCode=1;
});
