use std::future::Future;
use std::sync::Arc;

use super::Vm;

/// Explicit handle to the parent VM's execution context for the duration of one
/// async-builtin call. Threaded into every async builtin by the dispatch loop
/// (and the `#[harn_builtin]` macro), so context can no longer be "lost across a
/// spawn boundary": a handler that needs VM access receives or clones this
/// handle deliberately instead of reading ambient state.
///
/// Holds the "template" child VM that closure-invoking host helpers clone via
/// [`AsyncBuiltinCtx::child_vm`], and whose `output` buffer collects text
/// forwarded from VM-side closures via [`AsyncBuiltinCtx::forward_output`]. The
/// dispatch loop drains that buffer back to the original parent VM after the
/// async builtin returns. Cheap to clone: it is an `Arc` handle and everything
/// heavy inside the `Vm` is shared.
#[derive(Clone)]
pub struct AsyncBuiltinCtx {
    child: Arc<parking_lot::Mutex<Vm>>,
}

impl AsyncBuiltinCtx {
    fn new(vm: Vm) -> Self {
        Self {
            child: Arc::new(parking_lot::Mutex::new(vm)),
        }
    }

    /// Construct a context around `vm` for host adapters that are not themselves
    /// async builtins but need to run VM-side closures.
    pub fn from_vm(vm: Vm) -> Self {
        Self::new(vm)
    }

    /// Construct a standalone ctx around `vm` for unit tests that drive an async
    /// builtin handler directly (outside the dispatch loop). Production code
    /// receives its ctx from the dispatch path, never this.
    #[cfg(test)]
    pub fn for_test(vm: Vm) -> Self {
        Self::new(vm)
    }

    /// Clone a fresh child VM from this context. The returned `Vm` shares the
    /// parent's heavy state, so each closure-invoking handler gets its own
    /// cheap execution context.
    pub fn child_vm(&self) -> Vm {
        self.child.lock().child_vm()
    }

    /// Pool tasks may execute on any Tokio worker thread, so pool lookup state
    /// is shared through the VM context rather than thread-local storage.
    pub(crate) fn pool_registry(&self) -> Arc<crate::stdlib::pool::PoolRegistry> {
        self.child.lock().pool_registry.clone()
    }

    /// Create an independent context rooted at a fresh child VM. Long-lived
    /// local tasks use this instead of sharing the parent builtin's output
    /// buffer after the parent future has returned.
    pub fn child_ctx(&self) -> Self {
        Self::new(self.child_vm())
    }

    /// Forward captured output from a transient child VM (typically created via
    /// [`AsyncBuiltinCtx::child_vm`] and used to invoke a closure) back into this
    /// context's output buffer. The dispatch loop drains that buffer back to the
    /// original parent VM after the async builtin returns.
    ///
    /// Without this hook, `log()`/`__io_print()` calls inside
    /// `post_turn_callback` closures, tool handlers, and other VM-side closures
    /// invoked from async builtins would silently disappear because the transient
    /// child VM's output buffer is dropped on scope exit.
    pub fn forward_output(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.child.lock().append_output(text);
    }
}

/// Run an async builtin's future with `child` installed as its explicit
/// [`AsyncBuiltinCtx`]. `make_fut` receives the ctx handle and returns the
/// handler's future; the ctx is moved into the future, so it lives exactly as
/// long as the call. Returns the future's output plus any output that VM-side
/// closures forwarded into the context, which the dispatch loop appends to the
/// real parent VM. Cancel-safe: if the returned future is dropped, the ctx +
/// child `Vm` are dropped with it.
pub(crate) fn run_async_builtin_with<F, M>(
    child: Vm,
    make_fut: M,
) -> impl Future<Output = (F::Output, String)>
where
    F: Future + Send,
    M: FnOnce(AsyncBuiltinCtx) -> F,
{
    // Build the context + scope synchronously so the by-value `child: Vm` moves
    // onto the heap *before* any async state machine exists. If this were
    // an `async fn`, the future would reserve a Vm-sized slot for `child` up to
    // its first await, and that bloat propagates into every caller's stack
    // frame, which can trip clippy::large_stack_frames in large dispatch
    // functions.
    let ctx = AsyncBuiltinCtx::new(child);
    let registry = ctx.pool_registry();
    let sink = Arc::clone(&ctx.child);
    let fut = make_fut(ctx);
    async move {
        let output = crate::stdlib::pool::with_pool_registry_scope(registry, fut).await;
        let captured = sink.lock().take_output();
        (output, captured)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vm;

    #[tokio::test]
    async fn explicit_ctx_mints_child_and_captures_forwarded_output() {
        let (present, captured) = run_async_builtin_with(Vm::new(), |ctx| async move {
            // The handler holds the explicit ctx — no ambient lookup needed.
            let _child = ctx.child_vm();
            ctx.forward_output("hello ");
            ctx.forward_output("world");
            true
        })
        .await;
        assert!(present);
        // `forward_output` appends into the same buffer the dispatch loop drains.
        assert_eq!(captured, "hello world");
    }

    #[tokio::test]
    async fn child_context_has_independent_output_buffer() {
        let (_result, captured) = run_async_builtin_with(Vm::new(), |ctx| async move {
            let child = ctx.child_ctx();
            child.forward_output("child");
            ctx.forward_output("parent");
        })
        .await;
        assert_eq!(captured, "parent");
    }

    #[tokio::test]
    async fn cancelled_scope_strands_nothing() {
        use std::future::pending;
        // Build a future that never completes, then drop it without polling to
        // completion. The ctx is owned by that future, so dropping it releases
        // the child VM without any ambient cleanup.
        let never = run_async_builtin_with(Vm::new(), |_ctx| pending::<()>());
        drop(never);
    }
}
