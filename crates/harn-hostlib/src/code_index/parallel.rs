//! Scoped parallelism shared by the code index's build, restore, and compaction.
//!
//! Threads come from [`harn_parser::runtime_stack::scope`], which gives them
//! the runtime stack size every crate that can reach the parser must use.

pub(super) use harn_parser::runtime_stack::scope;

/// Join a scoped thread, re-raising its panic on the caller.
pub(super) fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}
