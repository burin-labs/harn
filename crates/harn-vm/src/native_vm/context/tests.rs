use super::*;
use crate::stdlib::pool::{current_pool_registry, new_pool_registry, with_pool_registry_poll};
use crate::vm::run_async_builtin_with;
use crate::NativeBuiltinCtxExt;
use std::{
    future::{poll_fn, Future},
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

#[tokio::test]
async fn explicit_ctx_mints_child_and_captures_forwarded_output() {
    let (present, captured) = run_async_builtin_with(crate::new_vm(), |ctx| async move {
        let _child = ctx.child_vm();
        ctx.forward_output("hello ");
        ctx.forward_output("world");
        true
    })
    .await;
    assert!(present);
    assert_eq!(captured, "hello world");
}

#[tokio::test]
async fn child_context_has_independent_output_buffer() {
    let (_result, captured) = run_async_builtin_with(crate::new_vm(), |ctx| async move {
        let child = ctx.child_ctx();
        child.forward_output("child");
        ctx.forward_output("parent");
    })
    .await;
    assert_eq!(captured, "parent");
}

#[test]
fn pending_builtin_restores_the_callers_pool_before_cancellation() {
    let outer = new_pool_registry();
    with_pool_registry_poll(&outer, || {
        let vm = crate::new_vm();
        let expected = Arc::clone(&vm.native().pool_registry);
        assert!(!Arc::ptr_eq(&expected, &outer));
        let polls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&polls);
        let mut future = Box::pin(run_async_builtin_with(vm, |ctx| {
            poll_fn(move |_| {
                assert!(Arc::ptr_eq(&current_pool_registry(), &expected));
                assert!(Arc::ptr_eq(&ctx.pool_registry(), &expected));
                observed.fetch_add(1, Ordering::SeqCst);
                Poll::<()>::Pending
            })
        }));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&current_pool_registry(), &outer));
        drop(future);
        assert!(Arc::ptr_eq(&current_pool_registry(), &outer));
    });
}
