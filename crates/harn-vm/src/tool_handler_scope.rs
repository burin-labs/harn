//! Async-safe marker for model- or client-invoked Harn tool handlers.

use std::future::Future;
use std::pin::Pin;

tokio::task_local! {
    static INSIDE_TOOL_HANDLER: ();
}

pub(crate) fn scope<F: Future>(future: F) -> Pin<Box<impl Future<Output = F::Output>>> {
    Box::pin(INSIDE_TOOL_HANDLER.scope((), future))
}

pub(crate) fn is_active() -> bool {
    INSIDE_TOOL_HANDLER.try_with(|()| true).unwrap_or(false)
}
