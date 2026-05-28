//! Ambient request_id scope threaded into `.harn` callees by hosts that
//! mint a request id at ingress (today: `harn-serve`'s HTTP/ACP/MCP/A2A
//! adapters via `http_codec::fresh_request_id`; future: in-process
//! callers that already track one).
//!
//! Exposed to scripts as the `harness.obs.request_id()` method:
//!
//! ```harn
//! let id = harness.obs.request_id()
//! ```
//!
//! Like [`crate::harness_tenant`], the scope is a stack so nested
//! dispatches restore the outer id on return. The host pushes once per
//! incoming request; the .harn callee sees a stable id for the entire
//! span tree of its dispatch.
//!
//! When no host has pushed a request id, `request_id()` returns `nil` —
//! callers in tests / standalone `harn run` contexts shouldn't have to
//! mint one to call obs methods.

use std::cell::RefCell;

thread_local! {
    static ACTIVE_REQUEST_ID_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard returned by [`enter_request_id`]. Popping the stack on
/// drop keeps the ambient balanced even when the dispatched callable
/// panics or returns an error.
#[must_use = "dropping the guard immediately pops the request_id scope"]
pub struct RequestIdScopeGuard {
    _private: (),
}

impl Drop for RequestIdScopeGuard {
    fn drop(&mut self) {
        ACTIVE_REQUEST_ID_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Push `request_id` onto the ambient stack for the lifetime of the
/// returned guard. The innermost entry wins for [`current_request_id`].
pub fn enter_request_id(request_id: impl Into<String>) -> RequestIdScopeGuard {
    ACTIVE_REQUEST_ID_STACK.with(|stack| stack.borrow_mut().push(request_id.into()));
    RequestIdScopeGuard { _private: () }
}

/// Currently-active request id, or `None` when the host did not bind
/// one (e.g. `harn run` against a local script with no ingress).
pub fn current_request_id() -> Option<String> {
    ACTIVE_REQUEST_ID_STACK.with(|stack| stack.borrow().last().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_returns_none_when_nothing_pushed() {
        assert_eq!(current_request_id(), None);
    }

    #[test]
    fn guard_pops_on_drop_and_inner_scope_shadows_outer() {
        let outer = enter_request_id("req_outer");
        assert_eq!(current_request_id().as_deref(), Some("req_outer"));
        {
            let _inner = enter_request_id("req_inner");
            assert_eq!(current_request_id().as_deref(), Some("req_inner"));
        }
        assert_eq!(current_request_id().as_deref(), Some("req_outer"));
        drop(outer);
        assert_eq!(current_request_id(), None);
    }
}
