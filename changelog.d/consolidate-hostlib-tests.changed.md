- Tests: `harn-hostlib`'s 33 integration-test binaries are consolidated into
  one (`harn_hostlib`), cutting total link time and shrinking the behavior
  archive; the 829-test suite and its nextest concurrency semantics are
  unchanged.
