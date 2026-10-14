//! Immutable transport observation for one execution, separate from durable
//! session subscribers and the top-of-stack loop observer.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

use super::{AgentEvent, AgentEventSink};

/// A prompt's originating transport. Empty is an explicit absence, never a
/// request to resolve a newer transport from a session-global registry.
#[derive(Clone, Default)]
pub struct AgentEventTransport(Option<Arc<dyn AgentEventSink>>);

thread_local! {
    static CURRENT_AGENT_EVENT_TRANSPORT: RefCell<AgentEventTransport> = const { RefCell::new(AgentEventTransport(None)) };
}

impl AgentEventTransport {
    pub fn new(sink: Arc<dyn AgentEventSink>) -> Self {
        Self(Some(sink))
    }

    /// Carry only observation ownership across polls, including cancellation
    /// and detached durable cleanup. No execution authority travels here.
    pub fn scope<F: Future>(&self, future: F) -> impl Future<Output = F::Output> {
        ScopedTransport {
            transport: self.clone(),
            inner: future,
        }
    }

    /// Run synchronous observation with this exact transport binding, restoring
    /// the caller's binding even when the action unwinds.
    pub fn with<T>(&self, action: impl FnOnce() -> T) -> T {
        struct Restore(AgentEventTransport);
        impl Drop for Restore {
            fn drop(&mut self) {
                swap(std::mem::take(&mut self.0));
            }
        }
        let _restore = Restore(swap(self.clone()));
        action()
    }
}

pub(crate) fn current() -> AgentEventTransport {
    CURRENT_AGENT_EVENT_TRANSPORT.with(|current| current.borrow().clone())
}

pub(crate) fn swap(next: AgentEventTransport) -> AgentEventTransport {
    CURRENT_AGENT_EVENT_TRANSPORT
        .with(|current| std::mem::replace(&mut *current.borrow_mut(), next))
}

pub(crate) fn emit(event: &AgentEvent) {
    if let Some(sink) = current().0 {
        sink.handle_event(event);
    }
}

pin_project! {
    struct ScopedTransport<F> {
        transport: AgentEventTransport,
        #[pin]
        inner: F,
    }
}

impl<F: Future> Future for ScopedTransport<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        this.transport.with(|| this.inner.poll(context))
    }
}

#[cfg(test)]
mod tests;
