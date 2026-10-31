const assert = require('node:assert/strict');
const vm = require('node:vm');
// GitHub property identifiers allow dashes; JavaScript needs bracket syntax.
const condition = process.argv[1].replace(/\.([A-Za-z_][A-Za-z0-9_-]*-[A-Za-z0-9_-]+)\b/g, "['$1']");
const admits = event_name => vm.runInNewContext(condition, {
  github: {event_name, repository: 'burin-labs/harn', actor: 'burin-control[bot]',
    event: {pull_request: {head: {repo: {full_name: 'burin-labs/harn'}}}}},
  needs: {changes: {outputs: {consumer: 'true'}}, 'rust-cli': {result: 'success'},
    'consumer-pin': {result: 'success'}},
});
assert.equal(admits('pull_request'), true, 'positive consumer dispatch was not reached');
assert.equal(admits('workflow_dispatch'), false, 'manual CI dispatched a private consumer');
assert.equal(admits('push'), false, 'main push dispatched a private consumer');
