mod builtin_dispatch;
mod capability_roots;
mod compile;
mod errors;
#[allow(dead_code)]
#[path = "../../../support/native_vm.rs"]
mod harness;
mod inline_cache;
mod language;
mod lexical_scope;
mod optimizer;
mod sandbox_builtins;
mod slots;

pub(in crate::vm_tests) use harness::{
    run_harn, run_harn_at, run_harn_result_async, run_harn_result_display_with_options,
};
