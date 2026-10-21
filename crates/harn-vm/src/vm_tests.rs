//! Interpreter behavior exercised with the native execution runtime.

use crate::*;
mod compiler_native_contracts;
mod stdlib_cache;
mod task_cleanup;
mod timed_execution;

mod baseline_native;
mod modules_native;
mod modules_tool_registry;

mod depth_regression_tests;
mod tests_debug;
mod tests_lazy_callable;
mod tests_lexical_block;
mod tests_lexical_capture;
mod tests_package_runtime;
mod tests_runtime;
mod tests_runtime_process_exit;
mod tests_typed_op_fallback;
mod tests_value_builders;
mod tests_value_calls;
