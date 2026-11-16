import assert from 'node:assert/strict';
import test from 'node:test';
import {consumerCompositionSchema, consumerPairing, consumerSourceDeclaration, decodeConsumerCompositionManifest} from '../lib/consumer-source.mjs';

const context = {repository: 'fixture/private-consumer', requiredPulls: ['7', '8'], compositionPull: '9'};
const source = () => ({
  schema: consumerCompositionSchema,
  repository: context.repository,
  base_sha: 'a'.repeat(40),
  adaptations: [
    {pull_request: 8, head_sha: 'c'.repeat(40)},
    {pull_request: 7, head_sha: 'b'.repeat(40)},
  ],
  repairs: [{pull_request: 12, head_sha: 'd'.repeat(40)}],
});

test('a closed manifest covers every source adaptation and keeps repair identities separate', () => {
  const manifest = decodeConsumerCompositionManifest(source(), context);
  assert.deepEqual(manifest.adaptations.map(component => component.pull_request), [7, 8]);
  assert.deepEqual(manifest.repairs, [{pull_request: 12, head_sha: 'd'.repeat(40)}]);
  assert.equal(manifest.base_sha, 'a'.repeat(40));
  assert.equal(Object.isFrozen(manifest), true);
  assert.equal(Object.isFrozen(manifest.adaptations), true);
  assert.equal(Object.isFrozen(manifest.adaptations[0]), true);
});

test('normalization copies component identities before asynchronous authentication', () => {
  const input = source();
  const manifest = decodeConsumerCompositionManifest(input, context);
  input.adaptations[1].head_sha = 'e'.repeat(40);
  input.repairs.length = 0;
  assert.equal(manifest.adaptations[0].head_sha, 'b'.repeat(40));
  assert.equal(manifest.repairs.length, 1);
});

const changes = [
  ['missing manifest', () => null],
  ['unknown schema', manifest => ({...manifest, schema: 'other'})],
  ['foreign repository', manifest => ({...manifest, repository: 'foreign/fork'})],
  ['unmeasured base', manifest => ({...manifest, base_sha: ''})],
  ['raw document payload', manifest => ({...manifest, document: {private: 'not allowed'}})],
  ['missing repair role', manifest => { delete manifest.repairs; return manifest; }],
  ['missing adaptation', manifest => ({...manifest, adaptations: manifest.adaptations.slice(1)})],
  ['extra adaptation', manifest => ({...manifest, adaptations: [...manifest.adaptations, {pull_request: 13, head_sha: 'e'.repeat(40)}]})],
  ['duplicate adaptation', manifest => ({...manifest, adaptations: [manifest.adaptations[0], manifest.adaptations[0]]})],
  ['repair replacing an adaptation', manifest => ({...manifest, adaptations: manifest.adaptations.slice(1), repairs: [manifest.adaptations[0]]})],
  ['duplicate role identity', manifest => ({...manifest, repairs: [manifest.adaptations[0]]})],
  ['self-referential composition', manifest => ({...manifest, repairs: [{pull_request: 9, head_sha: 'd'.repeat(40)}]})],
  ['unmeasured component head', manifest => ({...manifest, repairs: [{pull_request: 12, head_sha: null}]})],
  ['fractional pull number', manifest => ({...manifest, repairs: [{pull_request: 12.5, head_sha: 'd'.repeat(40)}]})],
  ['string pull number', manifest => ({...manifest, repairs: [{pull_request: '12', head_sha: 'd'.repeat(40)}]})],
  ['unknown component field', manifest => ({...manifest, repairs: [{...manifest.repairs[0], branch: 'untrusted'}]})],
];
for (const [name, change] of changes) {
  test(`composition manifest refuses ${name}`, () => {
    assert.throws(() => decodeConsumerCompositionManifest(change(source()), context),
      /reason=consumer_composition_manifest_unqualified$/);
  });
}

for (const requiredPulls of [[], ['7', '7'], ['7', 'unknown'], [7, 8]]) {
  test(`source pairing census refuses ${JSON.stringify(requiredPulls)}`, () => {
    assert.throws(() => decodeConsumerCompositionManifest(source(), {...context, requiredPulls}),
      /reason=consumer_composition_manifest_unqualified$/);
  });
}

test('ordinary no-pair and one-pair declarations retain their scalar decisions', () => {
  assert.equal(consumerPairing(''), null);
  assert.equal(consumerPairing('Pairs-with: consumer#7'), '7');
  assert.equal(consumerPairing('Pairs-with: consumer#7\nPairs-with: consumer#7', '7'), '7');
  assert.equal(consumerPairing('', '7'), '7');
  assert.throws(() => consumerPairing('Pairs-with: consumer#7', '8'), /consumer_pairing_ambiguous$/);
});

test('bare multiple adaptations still refuse rather than choosing an arbitrary pull', () => {
  assert.throws(() => consumerPairing('Pairs-with: consumer#7\nPairs-with: consumer#8'), error => {
    assert.deepEqual(error.pulls, ['7', '8']);
    return /consumer_pairing_ambiguous$/.test(error.message);
  });
});

test('explicit composition intent is tagged and cannot escape through the scalar adapter', () => {
  const text = 'Pairs-with: consumer#8\nPairs-with: consumer#7\nConsumer-composition: consumer#9';
  assert.deepEqual(consumerSourceDeclaration(text), {kind: 'composition', pull: '9', requiredPulls: ['7', '8']});
  assert.throws(() => consumerPairing(text), /consumer_composition_authentication_required$/);
  assert.deepEqual(consumerSourceDeclaration(`${text}\nConsumer-composition: consumer#9`, '9'),
    consumerSourceDeclaration(text));
});

for (const text of [
  'Consumer-composition: consumer#9',
  'Pairs-with: consumer#7\nConsumer-composition: consumer#7',
  'Pairs-with: consumer#7\nConsumer-composition: consumer#unknown',
  'Pairs-with: consumer#7\nConsumer-composition: consumer#0',
  'Pairs-with: consumer#7\nConsumer-composition: consumer#9\nConsumer-composition: consumer#10',
]) {
  test(`unqualified composition declaration refuses: ${JSON.stringify(text)}`, () => {
    assert.throws(() => consumerSourceDeclaration(text), /consumer_composition_declaration_(invalid|ambiguous)$/);
  });
}
