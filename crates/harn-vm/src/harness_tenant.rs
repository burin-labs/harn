//! Ambient tenant scope threaded into `.harn` callees by hosts that
//! resolve a tenant before dispatch (today: `harn-serve` via
//! `AuthenticatedPrincipal::tenant_id`; future: in-process orchestrators
//! that already hold a `TenantId`).
//!
//! Exposed to scripts as the `harness.tenant` sub-handle:
//!
//! ```harn
//! let tenant_id = harness.tenant.id()
//! ```
//!
//! `id()` returns the active tenant id string or raises a typed
//! [`ErrorCategory::Auth`] runtime error when the host did not bind a
//! tenant. `try_id()` returns `nil` instead so callers can branch on
//! tenancy without try/catch.
//!
//! The scope is stack-shaped (push/pop via [`enter_tenant`]) so nested
//! dispatches (a callee that re-enters the dispatcher under a different
//! tenant) restore the outer tenant on return.

use std::cell::RefCell;

use crate::TenantId;

thread_local! {
    static ACTIVE_TENANT_STACK: RefCell<Vec<TenantId>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard returned by [`enter_tenant`]. Popping the stack on drop
/// keeps the ambient scope balanced even when the dispatched callable
/// panics or returns an error.
#[must_use = "dropping the guard immediately pops the tenant scope"]
pub struct TenantScopeGuard {
    _private: (),
}

impl Drop for TenantScopeGuard {
    fn drop(&mut self) {
        ACTIVE_TENANT_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Push `tenant` onto the ambient stack for the lifetime of the
/// returned guard. The innermost entry wins for [`current_tenant_id`].
pub fn enter_tenant(tenant: TenantId) -> TenantScopeGuard {
    ACTIVE_TENANT_STACK.with(|stack| stack.borrow_mut().push(tenant));
    TenantScopeGuard { _private: () }
}

/// Currently-active tenant id, or `None` when the host did not bind
/// one. The innermost [`enter_tenant`] scope wins.
pub fn current_tenant_id() -> Option<TenantId> {
    ACTIVE_TENANT_STACK.with(|stack| stack.borrow().last().cloned())
}

/// Standard message raised by `harness.tenant.id()` when no tenant is
/// bound. Lives here (and not inline at the call site) so adapters and
/// tests can assert against one canonical string.
pub const MISSING_TENANT_MESSAGE: &str =
    "harness.tenant.id(): no tenant bound to this dispatch — the host did not authenticate a tenant-scoped principal";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_returns_none_when_nothing_pushed() {
        assert_eq!(current_tenant_id(), None);
    }

    #[test]
    fn guard_pops_on_drop_and_inner_scope_shadows_outer() {
        let outer = enter_tenant(TenantId::new("outer"));
        assert_eq!(current_tenant_id(), Some(TenantId::new("outer")));
        {
            let _inner = enter_tenant(TenantId::new("inner"));
            assert_eq!(current_tenant_id(), Some(TenantId::new("inner")));
        }
        assert_eq!(current_tenant_id(), Some(TenantId::new("outer")));
        drop(outer);
        assert_eq!(current_tenant_id(), None);
    }
}
