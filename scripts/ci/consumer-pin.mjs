import fs from 'node:fs';
import {githubPullPairing,githubConsumerSource,observerRefusal} from '../lib/consumer-observer.mjs';

try {
  if(process.argv.length!==2 || !process.env.GITHUB_OUTPUT) {
    throw new Error('CONSUMER_OBSERVER reason=consumer_pin_output_missing');
  }
  const paired=await githubPullPairing(process.env.PARENT_READ_TOKEN,process.env.GITHUB_REPOSITORY,
    process.env.PULL_REQUEST_NUMBER,process.env.PULL_REQUEST_HEAD_SHA);
  const source=await githubConsumerSource(process.env.GH_TOKEN,process.env.CONSUMER,paired);
  fs.appendFileSync(process.env.GITHUB_OUTPUT,`sha=${source.headSha}\npaired_pull=${paired??''}\n`);
  console.log(`CONSUMER_PIN captured=true paired=${paired!==null}`);
} catch(error) {
  console.error(`::error::${observerRefusal(error,'consumer_source_unmeasured').line}`);
  process.exitCode=1;
}
