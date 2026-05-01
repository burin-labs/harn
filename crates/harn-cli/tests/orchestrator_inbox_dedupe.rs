// This test validates dedupe persistence across an orchestrator
// process crash (via `HARN_TEST_CRON_FAIL_AFTER_EMIT=1`, which calls
// `std::process::exit(86)` after emitting a cron tick — see
// `crates/harn-vm/src/triggers/dispatcher/mod.rs:3857`). Process exit
// semantics are subprocess-only: an in-process harness running this
// hook would terminate the test runner itself. Migration tracked
// under issue #1069 (slow E2E/smoke job).
#![cfg(any())]
