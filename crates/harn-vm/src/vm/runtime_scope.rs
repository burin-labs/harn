//! Poll-local execution context supplied by the required runtime.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

/// Poll the continuation exactly once with this execution's context installed.
/// Context must be restored before returning, including during unwinding.
pub(crate) trait VmExecutionScope: Send {
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        continuation: &mut dyn FnMut(&mut Context<'_>) -> Poll<()>,
    ) -> Poll<()>;
}

pin_project_lite::pin_project! {
    pub(crate) struct RuntimeScoped<F> {
        #[pin]
        inner: F,
        scope: Box<dyn VmExecutionScope>,
    }
}

pub(crate) fn scope_runtime<F: Future>(
    scope: Box<dyn VmExecutionScope>,
    inner: F,
) -> RuntimeScoped<F> {
    RuntimeScoped { inner, scope }
}

impl<F: Future> Future for RuntimeScoped<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let mut inner = Some(this.inner);
        let mut output = None;
        let result = this.scope.poll(cx, &mut |cx| {
            inner
                .take()
                .expect("runtime scope polled its continuation twice")
                .poll(cx)
                .map(|value| {
                    output = Some(value);
                })
        });
        assert!(
            inner.is_none(),
            "runtime scope did not poll its continuation"
        );
        match (result, output) {
            (Poll::Pending, None) => Poll::Pending,
            (Poll::Ready(()), Some(value)) => Poll::Ready(value),
            _ => panic!("runtime scope changed its continuation's readiness"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    struct ForwardScope;

    impl VmExecutionScope for ForwardScope {
        fn poll(
            &mut self,
            cx: &mut Context<'_>,
            continuation: &mut dyn FnMut(&mut Context<'_>) -> Poll<()>,
        ) -> Poll<()> {
            continuation(cx)
        }
    }

    struct SkippedScope;

    impl VmExecutionScope for SkippedScope {
        fn poll(
            &mut self,
            _: &mut Context<'_>,
            _: &mut dyn FnMut(&mut Context<'_>) -> Poll<()>,
        ) -> Poll<()> {
            Poll::Ready(())
        }
    }

    #[test]
    fn scoped_future_preserves_pending_and_non_send_output_and_refuses_skipped_polls() {
        let polls = Cell::new(0);
        let output = Rc::new(7);
        let inner = std::future::poll_fn(|_| {
            polls.set(polls.get() + 1);
            if polls.get() == 1 {
                Poll::Pending
            } else {
                Poll::Ready(Rc::clone(&output))
            }
        });
        let mut future = std::pin::pin!(scope_runtime(Box::new(ForwardScope), inner));
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        let Poll::Ready(value) = future.as_mut().poll(&mut cx) else {
            panic!("the second poll must complete");
        };
        assert!(Rc::ptr_eq(&value, &output));
        assert_eq!(polls.get(), 2);

        let skipped = std::panic::catch_unwind(|| {
            let mut future =
                std::pin::pin!(scope_runtime(Box::new(SkippedScope), std::future::ready(7),));
            let mut cx = Context::from_waker(std::task::Waker::noop());
            future.as_mut().poll(&mut cx)
        });
        assert!(
            skipped.is_err(),
            "a scope cannot report success without polling"
        );
    }
}
