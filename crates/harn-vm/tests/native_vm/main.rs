#![recursion_limit = "256"]
#![allow(clippy::disallowed_methods)]
//! Interpreter behavior through the assembled native runtime.

use harn_vm::*;
use harn_vm_core::compiler;

mod test_support {
    pub(crate) use harn_clock::test_support::current_thread_runtime;
}

mod vm_tests;
