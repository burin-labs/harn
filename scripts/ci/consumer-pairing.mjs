import fs from 'node:fs';
import {consumerPairing} from '../lib/consumer-observer.mjs';

try {
  process.stdout.write(consumerPairing(fs.readFileSync(0,'utf8'), process.argv[2])??'');
} catch(error) {
  const reason=error.message==='CONSUMER_OBSERVER reason=consumer_pairing_ambiguous'
    ? 'pairing_ambiguous' : 'pairing_unrecognized';
  console.error(`::error::CONSUMER_CANARY reason=${reason}`);
  process.exitCode=1;
}
