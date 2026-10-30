use super::{AmbientExecutionScope, RestoreGuard};

/// Wrap `f` so it runs under the complete logical execution scope captured
/// now, from the currently polling Harn task, on whatever thread calls it.
///
/// The scope belongs to the Harn task, not the thread, so the wrapper installs
/// a copy around `f` and restores the thread before it is reused.
pub(crate) fn carry_ambient<F, R>(f: F) -> impl FnOnce() -> R + Send
where
    F: FnOnce() -> R + Send,
{
    let mut scope = AmbientExecutionScope::capture_for_inline_subtask();
    move || {
        scope.swap_in_place();
        let _restore = RestoreGuard {
            scope: &mut scope,
            armed: true,
        };
        f()
    }
}

/// Run a blocking host operation on Tokio's blocking pool under the spawner's
/// ambient scope.
///
/// Host adapters must not execute process waits inline on the LocalSet: one
/// synchronous wait would otherwise starve every sibling in `parallel`.
pub fn run_blocking_with_ambient<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    #[allow(clippy::disallowed_methods)] // The scoped blocking spawn itself.
    tokio::task::spawn_blocking(carry_ambient(f))
}
