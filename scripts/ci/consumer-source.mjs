import fs from 'node:fs';
import {githubConsumerSelection, observerRefusal} from '../lib/consumer-observer.mjs';
import {consumerSourceDeclaration, consumerSourceEnvironment} from '../lib/consumer-source.mjs';

try {
  const env = process.env;
  const declaration = process.argv[2] === '--selected'
    ? consumerSourceEnvironment(env)
    : consumerSourceDeclaration(fs.readFileSync(0, 'utf8'), env.CONSUMER_PULL_REQUEST || '');
  if (process.argv.length !== (process.argv[2] === '--selected' ? 3 : 2)) {
    throw new Error('CONSUMER_OBSERVER reason=consumer_source_invocation_invalid');
  }
  const source = declaration.kind === 'composition'
    ? await githubConsumerSelection(env.GH_TOKEN, env.CANARY_REPOSITORY, declaration) : null;
  // This stream is private transport. Callers must capture it, never log it.
  process.stdout.write(JSON.stringify({declaration, source}));
} catch (error) {
  console.error(`::error::${observerRefusal(error, 'consumer_source_unmeasured').line}`);
  process.exitCode = 1;
}
