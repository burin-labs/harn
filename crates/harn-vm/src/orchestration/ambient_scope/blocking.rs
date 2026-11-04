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
    // The host's secret provider is a Tokio task-local, not part of the
    // swapped thread state, so it is carried beside the scope the same way a
    // native subtask carries it. Without it a grant backed by the secret
    // store would fall back to the process-wide chain on the blocking pool.
    let provider = crate::secrets::active_secret_provider();
    move || {
        scope.swap_in_place();
        let _restore = RestoreGuard {
            scope: &mut scope,
            armed: true,
        };
        crate::secrets::with_secret_provider_poll(&provider, f)
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
