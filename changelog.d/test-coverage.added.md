- `harn test --coverage` reports per-file line coverage for the Harn source a
  user test suite executes, and `--coverage-out <path>` writes an LCOV tracefile
  consumable by Codecov, `genhtml`, and the VS Code Coverage Gutters extension.
  Coverage reuses the per-instruction source-line table the VM already carries,
  so it needs no separate instrumentation pass; recording is opt-in and adds a
  single predictable branch to the dispatch loop when no session is active.
