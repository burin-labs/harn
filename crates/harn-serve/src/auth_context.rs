//! Ambient embedder auth context threaded into a dispatch by transports
//! whose [`crate::SiteAuth`] (or equivalent) hook resolved per-request
//! authentication state the host-call bridge needs to see.
//!
//! The value is opaque to harn-serve: a `serde_json::Value` the embedder
//! produced at admission (e.g. the API-key record, session claims, or a
//! worker-token descriptor). `DispatchCore` installs it for the duration
//! of the `.harn` invocation — on the same OS thread the VM (and
//! therefore any [`harn_vm::HostCallBridge`]) runs on — so a bridge
//! implementation can recover it with [`current_auth_context`] while
//! handling a `host_call` issued by the dispatched handler.
//!
//! The scope is stack-shaped, mirroring [`harn_vm::enter_tenant`] /
//! [`harn_vm::enter_request_id`], so nested dispatches restore the outer
//! context on return.

use std::cell::RefCell;
use std::sync::Arc;

thread_local! {
    static ACTIVE_AUTH_CONTEXT_STACK: RefCell<Vec<Arc<serde_json::Value>>> =
        const { RefCell::new(Vec::new()) };
}

/// RAII guard returned by [`enter_auth_context`]. Popping the stack on
/// drop keeps the ambient scope balanced even when the dispatched
/// callable panics or returns an error.
#[must_use = "dropping the guard immediately pops the auth-context scope"]
pub struct AuthContextScopeGuard {
    _private: (),
}

impl Drop for AuthContextScopeGuard {
    fn drop(&mut self) {
        ACTIVE_AUTH_CONTEXT_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Push `context` onto the ambient stack for the lifetime of the
/// returned guard. The innermost entry wins for [`current_auth_context`].
pub fn enter_auth_context(context: serde_json::Value) -> AuthContextScopeGuard {
    ACTIVE_AUTH_CONTEXT_STACK.with(|stack| stack.borrow_mut().push(Arc::new(context)));
    AuthContextScopeGuard { _private: () }
}

/// Currently-active embedder auth context, or `None` when the dispatch
/// was admitted without one. The innermost [`enter_auth_context`] scope
/// wins.
pub fn current_auth_context() -> Option<Arc<serde_json::Value>> {
    ACTIVE_AUTH_CONTEXT_STACK.with(|stack| stack.borrow().last().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_scopes_shadow_and_restore() {
        assert!(current_auth_context().is_none());
        let outer = enter_auth_context(serde_json::json!({"who": "outer"}));
        assert_eq!(
            current_auth_context().as_deref(),
            Some(&serde_json::json!({"who": "outer"}))
        );
        {
            let _inner = enter_auth_context(serde_json::json!({"who": "inner"}));
            assert_eq!(
                current_auth_context().as_deref(),
                Some(&serde_json::json!({"who": "inner"}))
            );
        }
        assert_eq!(
            current_auth_context().as_deref(),
            Some(&serde_json::json!({"who": "outer"}))
        );
        drop(outer);
        assert!(current_auth_context().is_none());
    }
}
