//! Shared poll boundary for native and interpreter execution scopes.

use super::{AmbientExecutionScope, RestoreGuard, Scoped};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        this.scope
            .poll_scoped(*this.commit_on_ready, || this.inner.poll(cx))
    }
}

impl AmbientExecutionScope {
    pub(crate) fn poll_scoped<T>(
        &mut self,
        commit_on_ready: bool,
        poll: impl FnOnce() -> Poll<T>,
    ) -> Poll<T> {
        self.swap_in_place();
        let mut restore = RestoreGuard {
            scope: self,
            armed: true,
        };
        let result = poll();
        if result.is_ready() && commit_on_ready {
            restore.commit();
        }
        result
    }
}
