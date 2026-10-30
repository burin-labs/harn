#![recursion_limit = "256"]
#![allow(clippy::result_large_err, clippy::cloned_ref_to_slice_refs)]
#![cfg_attr(test, allow(clippy::disallowed_methods))]
//! Bytecode execution with required host services supplied by an execution backend.

#[path = "../build_support/compiler_sources.rs"]
mod compiler_sources;

pub mod atomic_io;
#[cfg(feature = "vm-bench-internals")]
#[doc(hidden)]
pub mod bench_internals;
pub mod builtin_profile;
pub mod builtin_support;
pub mod bytecode_cache;
pub mod cancellation;
pub mod canonical_json;
pub mod chunk;
pub mod compiler;
pub mod conditional_replace;
pub mod context_manifest;
pub mod coverage;
pub mod execution_scope;
pub mod limits;
pub mod linked_program;
pub mod module_artifact;
pub mod module_source;
pub mod op_interrupt;
pub mod prepared_module;
pub mod process_liveness;
pub mod runtime_effects;
pub mod runtime_guards;
pub mod runtime_limits;
pub mod runtime_stack;
pub mod schema;
pub mod shared_state;
pub mod source_compile;
pub mod stdlib_installers;
pub mod stdlib_modules;
pub mod synchronization;
pub mod text;
pub mod text_index;
pub mod typecheck;
pub mod user_dirs;
pub mod value;
pub mod vm;
pub mod wait_for_graph;
pub mod windows_path;
pub mod workspace_path;
pub use chunk::*;
pub use compiler::*;
pub use execution_scope::{
    current_execution_scope, enter_execution_scope, mint_execution_scope, ExecutionId,
    ExecutionScopeGuard, InvalidExecutionId,
};
pub use harn_kernel::BuiltinId;
pub use prepared_module::{
    PreparedModuleCache, PreparedModuleCacheStats, PreparedModuleGenerationStats,
};
pub use runtime_limits::{
    RuntimeLimitDescription, RuntimeLimitEntry, RuntimeLimits, RuntimeLimitsReport,
};
pub use runtime_stack::{on_vm_stack, RUNTIME_STACK_SIZE};
pub use source_compile::compile_source;
pub use value::*;
pub use vm::flight_recorder;
pub use vm::*;

pub mod observability {
    pub use crate::execution_scope;
}
