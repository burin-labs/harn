- Tests: `harn-vm`'s 39 integration-test binaries are consolidated into one
  (`harn_vm`), cutting link steps and archive weight; the 184-test set and
  all nextest concurrency semantics are unchanged. The behavior-artifact
  security filter (`binary(harn_vm)`) now also matches the consolidated
  integration binary — the security lane still executes only its
  name-scoped Landlock proof.
