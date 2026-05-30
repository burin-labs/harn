use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use super::Vm;

tokio::task_local! {
    /// The current async-builtin execution context, bound around every
    /// async-builtin future by [`run_async_builtin_with`] (the dispatch path)
    /// and [`scope_async_builtin`] (host adapters that need a VM context for
    /// nested closure invocation).
    ///
    /// This is a `tokio::task_local`, not a `thread_local!` stack, on purpose:
    /// the binding is scoped to the future, so a cancelled or panicked async
    /// builtin can never strand the child `Vm` it pins (the old stack relied
    /// on perfectly balanced manual push/pop and leaked a whole `Vm` — and the
    /// `Rc`-shared module graph + env snapshot it holds — on any unwind between
    /// push and pop). Task-locals also follow their task across worker threads,
    /// so this is correct under multi-thread runtimes where a `thread_local!`
    /// stack would silently read the wrong (or empty) context. Nesting is
    /// handled by nested `scope` calls: `with` sees the innermost binding,
    /// which restores the parent on unwind. See harn#2667.
    static ASYNC_BUILTIN_CTX: AsyncBuiltinCtx;
}

/// Shared handle to the parent VM's execution context for the duration of one
/// async-builtin call. Holds the "template" child VM that closure-invoking
/// host helpers clone via [`clone_async_builtin_child_vm`], and whose `output`
/// buffer collects text forwarded from VM-side closures via
/// [`forward_child_output_to_parent`]. Cheap to construct — everything heavy
/// inside the `Vm` is `Rc`/`Arc`-shared.
pub struct AsyncBuiltinCtx {
    child: Rc<RefCell<Vm>>,
}

impl AsyncBuiltinCtx {
    fn new(vm: Vm) -> Self {
        Self {
            child: Rc::new(RefCell::new(vm)),
        }
    }
}

/// Clone a fresh child VM from the current async-builtin context. Returns
/// `None` when called outside any async-builtin scope (e.g. top-level sync
/// code), matching the old empty-stack behaviour. The returned `Vm` shares the
/// parent's heavy state (env, builtins, bridge, module_cache) via `Arc`/`Rc`,
/// so each closure-invoking handler gets its own cheap execution context.
pub fn clone_async_builtin_child_vm() -> Option<Vm> {
    ASYNC_BUILTIN_CTX
        .try_with(|ctx| ctx.child.borrow().child_vm())
        .ok()
}

/// Forward captured output from a transient child VM (typically created via
/// [`clone_async_builtin_child_vm`] and used to invoke a closure) back into the
/// current async-builtin context's output buffer. The dispatch loop drains
/// that buffer back to the original parent VM after the async builtin returns.
///
/// Without this hook, `log()`/`__io_print()` calls inside `post_turn_callback`
/// closures, tool handlers, and other VM-side closures invoked from async
/// builtins would silently disappear because the transient child VM's output
/// buffer is dropped on scope exit. A no-op outside any async-builtin scope.
pub fn forward_child_output_to_parent(text: &str) {
    if text.is_empty() {
        return;
    }
    let _ = ASYNC_BUILTIN_CTX.try_with(|ctx| ctx.child.borrow_mut().append_output(text));
}

/// Run `fut` (an async builtin's future) with `child` installed as the current
/// async-builtin context, returning the future's output plus any output that
/// VM-side closures forwarded into the context. The dispatch loop appends the
/// captured output to the real parent VM. Cancel-safe: if `fut` is dropped, the
/// task-local binding and the child `Vm` are dropped with it — no strand.
pub(crate) fn run_async_builtin_with<F>(
    child: Vm,
    fut: F,
) -> impl Future<Output = (F::Output, String)>
where
    F: Future,
{
    // Build the context + scope synchronously so the by-value `child: Vm` moves
    // onto the heap (Rc) *before* any async state machine exists. If this were
    // an `async fn`, the future would reserve a Vm-sized slot for `child` up to
    // its first await, and that bloat propagates into every caller's stack
    // frame (tripping clippy::large_stack_frames on big dispatch fns). See
    // harn#2667.
    let ctx = AsyncBuiltinCtx::new(child);
    let sink = Rc::clone(&ctx.child);
    let scoped = ASYNC_BUILTIN_CTX.scope(ctx, fut);
    async move {
        let output = scoped.await;
        let captured = sink.borrow_mut().take_output();
        (output, captured)
    }
}

/// Run `fut` with `vm` installed as the async-builtin context. Used by host
/// adapters (settlement loops, worker tasks, the trigger dispatcher, sub-agent
/// execution) that need VM-side closures invoked within `fut` to resolve a
/// [`clone_async_builtin_child_vm`] root. Replaces the old
/// `install_async_builtin_child_vm` RAII guard, which kept a `thread_local!`
/// stack entry alive for a lexical scope; a future scope is both cancel-safe
/// and multi-thread-correct.
pub fn scope_async_builtin<F>(vm: Vm, fut: F) -> impl Future<Output = F::Output>
where
    F: Future,
{
    // Sync wrapping (see `run_async_builtin_with`): `vm` moves onto the heap
    // before the future is constructed, so callers never carry it on the stack.
    ASYNC_BUILTIN_CTX.scope(AsyncBuiltinCtx::new(vm), fut)
}

/// Spawn a `!Send` local task that inherits the current async-builtin context.
///
/// `tokio::task_local` is task-scoped, so — unlike the old `thread_local!`
/// child-VM stack — it does NOT propagate across `spawn_local`. Any spawned
/// task that runs VM work (invokes closures, drives an agent loop, dispatches
/// triggers, services a pool) must therefore re-bind the context, or
/// `clone_async_builtin_child_vm()` returns `None` deep inside and the work
/// fails or hangs. This is the single sanctioned way to spawn such a task: it
/// snapshots the context VM *now*, while the caller still holds it, and
/// re-scopes it inside the task. A task spawned with no active context runs
/// without one (correct for genuinely context-free work). See harn#2667.
///
/// Spawns whose body does NOT touch VM state (pure I/O, receipt emission,
/// channel plumbing) should keep using `tokio::task::spawn_local` directly.
pub fn spawn_local_with_async_builtin_ctx<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let captured = clone_async_builtin_child_vm();
    tokio::task::spawn_local(async move {
        match captured {
            Some(vm) => scope_async_builtin(vm, fut).await,
            None => fut.await,
        }
    })
}

/// Run a synchronous closure with `vm` bound as the async-builtin context for
/// its dynamic extent. For synchronous harnesses (benchmarks, integration
/// scaffolding) that drive async work via `block_on` *inside* the closure and
/// need the context available throughout — `block_on` polls inline on the
/// calling thread, so the task-local set here is visible to those futures. See
/// harn#2667.
pub fn with_async_builtin_ctx_sync<R>(vm: Vm, f: impl FnOnce() -> R) -> R {
    ASYNC_BUILTIN_CTX.sync_scope(AsyncBuiltinCtx::new(vm), f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vm;

    #[tokio::test]
    async fn ctx_absent_outside_any_scope() {
        // No async-builtin scope is active: minting a child returns None and
        // forwarding output is a harmless no-op (must not panic).
        assert!(clone_async_builtin_child_vm().is_none());
        forward_child_output_to_parent("dropped");
    }

    #[tokio::test]
    async fn scope_binds_ctx_and_captures_forwarded_output() {
        let visible_inside = run_async_builtin_with(Vm::new(), async {
            let present = clone_async_builtin_child_vm().is_some();
            forward_child_output_to_parent("hello ");
            forward_child_output_to_parent("world");
            present
        })
        .await;
        assert_eq!(visible_inside, (true, "hello world".to_string()));
        // The binding is dropped with the future — no strand, context gone.
        assert!(clone_async_builtin_child_vm().is_none());
    }

    #[tokio::test]
    async fn nested_scopes_restore_the_parent_context() {
        run_async_builtin_with(Vm::new(), async {
            assert!(clone_async_builtin_child_vm().is_some());
            scope_async_builtin(Vm::new(), async {
                assert!(clone_async_builtin_child_vm().is_some());
            })
            .await;
            // Inner scope ended; the outer context is visible again.
            assert!(clone_async_builtin_child_vm().is_some());
        })
        .await;
        assert!(clone_async_builtin_child_vm().is_none());
    }

    #[tokio::test]
    async fn cancelled_scope_strands_nothing() {
        use std::future::pending;
        // Build a scoped future that never completes, then drop it without
        // polling to completion. With the old thread_local stack a manual
        // push with no matching pop would strand the child Vm; with a
        // task_local scope the binding only exists while the future is live.
        let never = run_async_builtin_with(Vm::new(), pending::<()>());
        drop(never);
        assert!(clone_async_builtin_child_vm().is_none());
    }
}
