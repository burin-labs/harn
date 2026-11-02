//! Async-safe marker for model- or client-invoked Harn tool handlers.

use std::future::Future;
use std::pin::Pin;

tokio::task_local! {
    static INSIDE_TOOL_HANDLER: bool;
}

pub(crate) fn scope<F: Future>(future: F) -> Pin<Box<impl Future<Output = F::Output>>> {
    Box::pin(INSIDE_TOOL_HANDLER.scope(true, future))
}

/// Retain the registering context rather than inheriting the firing tool's context.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CallbackScope(bool);

impl CallbackScope {
    pub(crate) fn capture() -> Self {
        Self(is_active())
    }

    pub(crate) fn scope<F: Future>(self, future: F) -> Pin<Box<impl Future<Output = F::Output>>> {
        Box::pin(INSIDE_TOOL_HANDLER.scope(self.0, future))
    }
}

pub(crate) fn is_active() -> bool {
    INSIDE_TOOL_HANDLER
        .try_with(|active| *active)
        .unwrap_or(false)
}
