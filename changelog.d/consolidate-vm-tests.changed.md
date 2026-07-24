- Tests: 36 of `harn-vm`'s 39 integration-test binaries are consolidated into
  one (`harn_vm`), cutting link steps and archive weight while keeping three
  process-isolation-sensitive suites separate. The 184-test set is unchanged.
  The behavior-artifact
  security filter (`binary(harn_vm)`) now also matches the consolidated
  integration binary — the security lane still executes only its
  name-scoped Landlock proof.
- The run/session view compatibility fixture moved into that consolidated test
  binary without changing its schema or snapshots.
