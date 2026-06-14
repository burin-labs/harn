//! Ambient authenticated-principal scope threaded into `.harn` callees by
//! hosts that authenticate a request before dispatch (today: `harn-serve`,
//! which resolves an [`crate::auth`-style] principal — subject, scheme,
//! granted scopes, and an optional embedder-assigned `kind` — at admission).
//!
//! Exposed to scripts as the `harness.auth` sub-handle:
//!
//! ```harn
//! if !harness.auth.has_scope("admin:dlq:write") { return forbidden(req) }
//! let actor = harness.auth.subject()
//! ```
//!
//! The handle is **read-only identity** — it carries only the generic
//! principal facts harn-serve itself authenticated (subject, scheme,
//! granted scopes, an optional principal `kind`). It deliberately does NOT
//! expose the tenant (that stays the single-sourced [`harness.tenant`]
//! ambient, see [`crate::harness_tenant`]) and never carries credentials,
//! secrets, or product-specific authorization concepts: a `.harn` policy
//! helper composes these facts with `harness.tenant` and the route context
//! to decide admission, so the language core stays about principals and
//! scopes, not products.
//!
//! Method semantics:
//! - `is_authenticated()` — whether the host bound a principal at all.
//! - `subject()` / `scheme()` — the authenticated subject and auth scheme;
//!   raise a typed [`ErrorCategory::Auth`] error when no principal is bound
//!   (mirroring `harness.tenant.id()`). `try_subject()` / `try_scheme()`
//!   return `nil` instead so callers can branch without try/catch.
//! - `kind()` — the optional principal classification the host assigned
//!   (e.g. `"operator"`, `"tenant"`, `"worker"`); `nil` when unset, even for
//!   an authenticated principal, so it is inherently a `try`-shaped getter.
//! - `scopes()` — the granted scope set as a sorted list (empty when no
//!   principal is bound — an unauthenticated caller has granted nothing).
//! - `has_scope(scope)` — membership test against `scopes()`; `false` when
//!   no principal is bound.
//!
//! The scope is stack-shaped (push/pop via [`enter_auth_principal`]) so
//! nested dispatches (a callee that re-enters the dispatcher under a
//! different principal) restore the outer principal on return, exactly like
//! [`crate::harness_tenant`] and [`crate::observability::request_id`].

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The authenticated principal a host bound for the duration of a
/// dispatch. Carries only generic identity facts harn-serve authenticated;
/// never secrets, tenant (see [`crate::harness_tenant`]), or product
/// authorization concepts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthPrincipal {
    /// Stable identifier for the authenticated subject (e.g. an API-key id,
    /// OAuth `sub`, or worker token id). Empty string is treated as "no
    /// subject" by the getters but a bound principal should always set it.
    pub subject: String,
    /// Auth scheme that admitted the request (e.g. `"apikey"`, `"oauth"`,
    /// `"hmac"`). Lets a policy gate on credential class.
    pub scheme: String,
    /// Scopes the credential carries — the same set harn-serve checked the
    /// route's `@scopes` against. Sorted/deduped via `BTreeSet`.
    pub scopes: BTreeSet<String>,
    /// Optional principal classification the host assigned (e.g.
    /// `"operator"` vs `"tenant"` vs `"worker"`). Generic — harn-serve does
    /// not interpret it; policies match against it for "allowed principal
    /// kinds". `None` when the host did not classify the principal.
    pub kind: Option<String>,
}

thread_local! {
    static ACTIVE_PRINCIPAL_STACK: RefCell<Vec<Arc<AuthPrincipal>>> =
        const { RefCell::new(Vec::new()) };
}

/// RAII guard returned by [`enter_auth_principal`]. Popping the stack on
/// drop keeps the ambient scope balanced even when the dispatched callable
/// panics or returns an error.
#[must_use = "dropping the guard immediately pops the auth-principal scope"]
pub struct AuthPrincipalScopeGuard {
    _private: (),
}

impl Drop for AuthPrincipalScopeGuard {
    fn drop(&mut self) {
        ACTIVE_PRINCIPAL_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Push `principal` onto the ambient stack for the lifetime of the
/// returned guard. The innermost entry wins for [`current_auth_principal`].
pub fn enter_auth_principal(principal: AuthPrincipal) -> AuthPrincipalScopeGuard {
    ACTIVE_PRINCIPAL_STACK.with(|stack| stack.borrow_mut().push(Arc::new(principal)));
    AuthPrincipalScopeGuard { _private: () }
}

/// Currently-active authenticated principal, or `None` when the host
/// dispatched without authenticating one. The innermost
/// [`enter_auth_principal`] scope wins.
pub fn current_auth_principal() -> Option<Arc<AuthPrincipal>> {
    ACTIVE_PRINCIPAL_STACK.with(|stack| stack.borrow().last().cloned())
}

/// Standard message raised by `harness.auth.subject()` /
/// `harness.auth.scheme()` when no principal is bound. Lives here (and not
/// inline at the call site) so adapters and tests can assert against one
/// canonical string.
pub const MISSING_PRINCIPAL_MESSAGE: &str =
    "harness.auth: no principal bound to this dispatch — the host did not authenticate the request";

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(subject: &str, scopes: &[&str]) -> AuthPrincipal {
        AuthPrincipal {
            subject: subject.to_string(),
            scheme: "apikey".to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            kind: Some("operator".to_string()),
        }
    }

    #[test]
    fn current_returns_none_when_nothing_pushed() {
        assert!(current_auth_principal().is_none());
    }

    #[test]
    fn guard_pops_on_drop_and_inner_scope_shadows_outer() {
        let outer = enter_auth_principal(principal("outer", &["read:a"]));
        assert_eq!(
            current_auth_principal().map(|p| p.subject.clone()),
            Some("outer".to_string())
        );
        {
            let _inner = enter_auth_principal(principal("inner", &["read:b"]));
            assert_eq!(
                current_auth_principal().map(|p| p.subject.clone()),
                Some("inner".to_string())
            );
        }
        assert_eq!(
            current_auth_principal().map(|p| p.subject.clone()),
            Some("outer".to_string())
        );
        drop(outer);
        assert!(current_auth_principal().is_none());
    }
}
