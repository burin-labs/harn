import fs from 'node:fs';
import {observeConsumerProduct, observerContext} from '../lib/consumer-observer.mjs';

async function main() {
  const env = process.env;
  const context = observerContext(JSON.parse(env.OBSERVER_CONTEXT));
  const expected = {
    repository:`${env.CANARY_OWNER}/${env.CANARY_NAME}`,
    workflow:`.github/workflows/${env.CANARY_WORKFLOW}`,
  };
  const reading = await observeConsumerProduct(env.GH_TOKEN, context, expected);
  // The closed reading contains counts and a public source decision only.
  // Neither private discovery IDs nor custody receipts cross this boundary.
  const verdict = reading.product !== null && reading.discovered === 1
    && reading.owned === 1 && reading.pending === 0 && reading.refused === 0 ? reading.product.verdict
    : reading.refused > 0 ? 'refused' : 'pending';
  fs.appendFileSync(env.GITHUB_OUTPUT, `verdict=${verdict}\nreading=${JSON.stringify(reading)}\n`);
  console.log(`CONSUMER_OBSERVER observe=measured verdict=${verdict} discovered=${reading.discovered} owned=${reading.owned} pending=${reading.pending} refused=${reading.refused}`);
}

main().catch(error => {
  const reason = /^CONSUMER_OBSERVER reason=([a-z_]+)$/.exec(error.message)?.[1] ?? 'product_observation_unmeasured';
  console.error(`::error::CONSUMER_OBSERVER reason=${reason}`);
  process.exitCode = 1;
});
