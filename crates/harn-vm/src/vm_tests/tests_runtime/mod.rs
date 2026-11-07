//! Runtime tests that require private native-runtime hooks.

mod concurrency;
#[allow(dead_code)]
#[path = "../../../tests/support/native_vm.rs"]
mod harness;
mod process_sandbox;
