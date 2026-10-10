//! Runtime tests for the VM, grouped by the behavior under test.
//!
//! `harness` owns the program runners; every other module is tests only.
//! The four runners re-exported below are also used by the sibling
//! `vm::tests_*` files, so they keep their original `vm`-wide visibility.

mod builtin_dispatch;
mod capability_roots;
mod compile;
mod concurrency;
mod errors;
mod harness;
mod inline_cache;
mod language;
mod lexical_scope;
mod optimizer;
mod process_sandbox;
mod sandbox_builtins;
mod slots;
mod smoke;

pub(in crate::vm) use harness::{
    run_harn, run_harn_at, run_harn_result_async, run_harn_result_display_with_options,
};
