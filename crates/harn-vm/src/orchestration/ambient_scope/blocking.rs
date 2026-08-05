use std::future::Future;

use super::{AmbientExecutionScope, RestoreGuard};

/// Run a blocking host operation on Tokio's blocking pool with the complete
/// logical execution scope captured from the currently polling Harn task.
///
/// Host adapters must not execute process waits inline on the LocalSet: one
/// synchronous wait would otherwise starve every sibling in `parallel`. The
/// ambient scope still belongs to the Harn task, so install a copy around the
/// blocking closure and restore the worker thread before it is reused.
pub fn run_blocking_with_ambient<F, R>(
    f: F,
) -> impl Future<Output = Result<R, tokio::task::JoinError>>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let mut scope = AmbientExecutionScope::capture_for_inline_subtask();
    tokio::task::spawn_blocking(move || {
        scope.swap_in_place();
        let _restore = RestoreGuard {
            scope: &mut scope,
            armed: true,
        };
        f()
    })
}
