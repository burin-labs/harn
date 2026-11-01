import fs from 'node:fs';
import {consumerPairing} from '../lib/consumer-observer.mjs';

try {
  process.stdout.write(consumerPairing(fs.readFileSync(0,'utf8'), process.argv[2])??'');
} catch(error) {
  const ambiguous=error.message==='CONSUMER_OBSERVER reason=consumer_pairing_ambiguous';
  const pulls=ambiguous && Array.isArray(error.pulls) ? ` pulls=${error.pulls.join(',')}` : '';
  console.error(`::error::CONSUMER_CANARY reason=${ambiguous ? 'pairing_ambiguous' : 'pairing_unrecognized'}${pulls}`);
  process.exitCode=1;
}
