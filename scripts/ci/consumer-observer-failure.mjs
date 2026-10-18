import {writeConsumerFailure} from '../lib/consumer-observer-failure.mjs';

try {
  const receipt = await writeConsumerFailure(process.env, process.argv[2]);
  console.log(`CONSUMER_CANARY failure_receipt=recorded queue_depth=${receipt.queue.depth ?? 'unknown'} pending=${receipt.queue.pending_runs?.length ?? 'unknown'}`);
} catch {
  console.error('::error::CONSUMER_CANARY reason=failure_receipt_unwritten queue_depth=unknown pending=unknown');
  process.exitCode = 1;
}
