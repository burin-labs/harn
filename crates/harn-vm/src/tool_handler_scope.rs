//! Async-safe marker for model- or client-invoked Harn tool handlers.

use std::future::Future;

tokio::task_local! {
    static INSIDE_TOOL_HANDLER: ();
}

pub(crate) async fn scope<F: Future>(future: F) -> F::Output {
    INSIDE_TOOL_HANDLER.scope((), future).await
}

pub(crate) fn is_active() -> bool {
    INSIDE_TOOL_HANDLER.try_with(|()| true).unwrap_or(false)
}
