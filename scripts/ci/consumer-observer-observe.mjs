import fs from 'node:fs';
import {consumerProductVerdict, observeConsumerProduct, observerContext, observerRefusal } from '../lib/consumer-observer.mjs';

async function main() {
  const env = process.env;
  const context = observerContext(JSON.parse(env.OBSERVER_CONTEXT));
  const expected = {
    repository:`${env.CANARY_OWNER}/${env.CANARY_NAME}`,
    workflow:`.github/workflows/${env.CANARY_WORKFLOW}`,
    pairedPull:env.CANARY_PAIRED_PULL||null,
  };
  const reading = await observeConsumerProduct(env.GH_TOKEN, context, expected);
  // The closed reading contains counts and a public source decision only.
  // Neither private discovery IDs nor custody receipts cross this boundary.
  const verdict = consumerProductVerdict(reading);
  const productVerdict=verdict==='pass'||verdict==='fail'?`verdict=${verdict}\n`:'';
  fs.appendFileSync(env.GITHUB_OUTPUT, `${productVerdict}observation_outcome=${verdict}\nreading=${JSON.stringify(reading)}\n`);
  console.log(`CONSUMER_OBSERVER observe=measured verdict=${verdict} discovered=${reading.discovered} owned=${reading.owned} pending=${reading.pending} refused=${reading.refused}`);
}

main().catch(error => {
  const {line} = observerRefusal(error, 'product_observation_unmeasured');
  console.error(`::error::${line}`);
  process.exitCode = 1;
});
