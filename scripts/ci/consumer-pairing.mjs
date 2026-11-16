import fs from 'node:fs';
import {consumerPairingClaim} from '../lib/consumer-observer.mjs';

// Prints the pull to rehearse, then the superseded pulls, comma-separated.
// The canary proves containment; this parser holds no credentials.
try {
  const claim=consumerPairingClaim(fs.readFileSync(0,'utf8'), process.argv[2]);
  process.stdout.write(`${claim.pull??''}\n${claim.superseded.join(',')}\n`);
} catch(error) {
  const ambiguous=error.message==='CONSUMER_OBSERVER reason=consumer_pairing_ambiguous';
  const pulls=ambiguous && Array.isArray(error.pulls) ? ` pulls=${error.pulls.join(',')}` : '';
  console.error(`::error::CONSUMER_CANARY reason=${ambiguous ? 'pairing_ambiguous' : 'pairing_unrecognized'}${pulls}`);
  process.exitCode=1;
}
