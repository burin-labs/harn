const assert = require('node:assert/strict');
const vm = require('node:vm');

const condition = process.argv[1];
const admits = (ref, login = 'harn-release-bot[bot]', repository = 'burin-labs/harn', event = 'pull_request_target') =>
  vm.runInNewContext(condition, {
    startsWith: (value, prefix) => value.startsWith(prefix),
    github: {repository, event_name: event, event: {pull_request: {user: {login}, head: {ref}}}},
  });

for (const ref of ['release/v1.2.4', 'release-attempt/v1.2.4/' + 'a'.repeat(40), 'automation/development-1.2.5-dev']) {
  assert.equal(admits(ref), true, `version-only checker was not reached for ${ref}`);
  assert.equal(admits(ref, 'unrelated[bot]'), false);
  assert.equal(admits(ref, 'harn-release-bot[bot]', 'unrelated/fork'), false);
}
assert.equal(admits('unrelated/branch'), false);
assert.equal(admits('unrelated/branch', '', 'burin-labs/harn', 'workflow_dispatch'), true);
