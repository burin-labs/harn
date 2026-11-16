import fs from 'node:fs';
import {githubConsumerSelection,observerRefusal} from '../lib/consumer-observer.mjs';
import {consumerSourceDeclaration} from '../lib/consumer-source.mjs';

try {
  if(process.argv.length!==2 || !process.env.GITHUB_OUTPUT) {
    throw new Error('CONSUMER_OBSERVER reason=consumer_pin_output_missing');
  }
  const declaration=consumerSourceDeclaration(process.env.PULL_REQUEST_BODY);
  const source=await githubConsumerSelection(process.env.GH_TOKEN,process.env.CONSUMER,declaration);
  fs.appendFileSync(process.env.GITHUB_OUTPUT,`sha=${source.headSha}\n`);
  console.log(`CONSUMER_PIN captured=true paired=${declaration.pull!==null} composition=${declaration.kind==='composition'}`);
} catch(error) {
  console.error(`::error::${observerRefusal(error,'consumer_source_unmeasured').line}`);
  process.exitCode=1;
}
