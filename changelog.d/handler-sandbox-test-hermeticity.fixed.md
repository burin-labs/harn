- **Handler-sandbox tests are hermetic against ambient `HARN_HANDLER_SANDBOX`
  configuration.** Under `cfg(test)` the sandbox fallback selector
  (`effective_fallback`) now reads `HARN_HANDLER_SANDBOX` through a
  thread-local override seam instead of the process environment, so a
  developer's or CI wrapper's exported `HARN_HANDLER_SANDBOX=off`/`enforce`
  can no longer flip the sandbox outcome of a test that never asked for it
  (previously the exec-path tests in `vm::tests_runtime` observed the ambient
  value directly). The five tests that need `enforce` now inject it through a
  `handler_sandbox_test_guard()` that clears the override on creation and drop,
  replacing the old manual `set_var`/restore dance; every other exec test sees
  the built-in `warn` default deterministically. The override is thread-keyed,
  matching the same-thread `new_current_thread` runtime those tests drive, so
  no cross-test lock is needed and the suite stays parallel-safe under both
  `cargo test` and nextest.
