import fs from 'node:fs';
import path from 'node:path';
import {createHmac, timingSafeEqual} from 'node:crypto';

/** Private, observation-scoped retention. Product and cancellation owners never
 * consume this cache; terminal timing reauthenticates the original artifacts. */
export function consumerTimingCache(file, context, expected, key) {
  const scope = JSON.stringify([context, expected.repository, expected.workflow, expected.runId]);
  const invalid = () => { throw new Error('CONSUMER_OBSERVER reason=timing_cache_invalid'); };
  if (!/^[0-9a-f]{64}$/.test(key ?? '')) invalid();
  const sign = (scope, proof) => createHmac('sha256', Buffer.from(key, 'hex'))
    .update(scope).update('\0').update(JSON.stringify(proof)).digest();
  return {
    read() {
      let descriptor;
      try {
        descriptor = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
        const info = fs.fstatSync(descriptor);
        if (!info.isFile() || info.size > 131072 || (info.mode & 0o077)
          || (process.getuid && info.uid !== process.getuid())) invalid();
        const value = JSON.parse(fs.readFileSync(descriptor, 'utf8'));
        // A prior invocation or forged preseed has no current authority. It
        // must take the original artifact path before any ownership claim.
        if (typeof value.scope !== 'string' || !/^[0-9a-f]{64}$/.test(value.mac ?? '')
          || !value.proof || !timingSafeEqual(Buffer.from(value.mac, 'hex'), sign(value.scope, value.proof))) return null;
        if (Object.keys(value).sort().join(',') !== 'mac,proof,schema,scope'
          || value.schema !== 'harn.consumer_timing_cache.v1' || value.scope !== scope
          || !value.proof || Object.keys(value.proof).sort().join(',') !== 'declaration,receipt') invalid();
        return value.proof;
      } catch (error) {
        if (error.code === 'ENOENT') return null;
        invalid();
      } finally {
        if (descriptor !== undefined) fs.closeSync(descriptor);
      }
    },
    write(proof) {
      const directory = fs.mkdtempSync(path.join(path.dirname(file), '.consumer-timing-'));
      try {
        const temporary = path.join(directory, 'proof.json');
        fs.writeFileSync(temporary, JSON.stringify({schema:'harn.consumer_timing_cache.v1', scope, proof,
          mac:sign(scope, proof).toString('hex')}),
          {mode:0o600, flag:'wx'});
        fs.renameSync(temporary, file);
      } finally { fs.rmSync(directory, {recursive:true}); }
    },
    clear() { fs.rmSync(file, {force:true}); },
  };
}
