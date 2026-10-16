//! The running task's execution context, shared by policy and process adapters.

use std::cell::RefCell;
use std::path::PathBuf;

use super::RunExecutionRecord;

thread_local! {
    static VM_EXECUTION_CONTEXT: RefCell<Option<RunExecutionRecord>> = const { RefCell::new(None) };
}

pub fn set_thread_execution_context(context: Option<RunExecutionRecord>) {
    VM_EXECUTION_CONTEXT.with(|current| *current.borrow_mut() = context);
}

pub(crate) fn current_execution_context() -> Option<RunExecutionRecord> {
    VM_EXECUTION_CONTEXT.with(|current| current.borrow().clone())
}

/// Swap the context at every ambient task boundary. A worker holding its cwd,
/// environment and capability workspace across an await must not inherit a
/// cooperatively scheduled sibling's context.
pub(crate) fn swap_thread_execution_context(
    next: Option<RunExecutionRecord>,
) -> Option<RunExecutionRecord> {
    VM_EXECUTION_CONTEXT.with(|current| std::mem::replace(&mut *current.borrow_mut(), next))
}

pub fn execution_root_path() -> PathBuf {
    current_execution_context()
        .and_then(|context| context.cwd.map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}
