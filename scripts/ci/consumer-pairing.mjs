import fs from 'node:fs';
import {consumerPairing} from '../lib/consumer-observer.mjs';
import {consumerSourceDeclaration} from '../lib/consumer-source.mjs';

try {
  const declaration=process.argv[2]==='--declaration';
  const text=fs.readFileSync(0,'utf8');
  process.stdout.write(declaration
    ? JSON.stringify(consumerSourceDeclaration(text,process.argv[3]))
    : consumerPairing(text,process.argv[2])??'');
} catch(error) {
  const ambiguous=error.message==='CONSUMER_OBSERVER reason=consumer_pairing_ambiguous';
  const pulls=ambiguous && Array.isArray(error.pulls) ? ` pulls=${error.pulls.join(',')}` : '';
  console.error(`::error::CONSUMER_CANARY reason=${ambiguous ? 'pairing_ambiguous' : 'pairing_unrecognized'}${pulls}`);
  process.exitCode=1;
}
