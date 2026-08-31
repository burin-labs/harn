//! Ambient EXECUTION SCOPE — an immutable owner token minted once per Harn
//! program run and readable, like [`crate::observability::request_id`], from
//! hostlib builtins on the synchronous dispatch stack.
//!
//! Unlike `request_id` (which a *host* pushes only at served ingress and which
//! is `None` for standalone `harn run`/`harn test`), the execution scope is
//! established by the VM itself at the top-level program boundary
//! (`Vm::execute_scoped`), so it is ALWAYS present during real execution and is
//! never a shared sentinel. That is exactly what lets the verdict issuance
//! authority bind a proof-of-execution receipt to the specific run that
//! PRODUCED the evidence: `run_test` captures the active scope when it records a
//! real execution, and `harness.verdict.issue` mints a positive verdict only
//! when the current active scope EQUALS that captured owner — so an old green
//! handle cannot bless a later, different run (the cross-run replay class).
//!
//! The scope is a stack so nested program executions restore the outer owner on
//! return; the innermost entry wins for [`current_execution_scope`]. When no
//! scope is active (there is no owning execution), issuance FAILS CLOSED — it
//! never falls back to a default owner.

use std::cell::RefCell;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

/// Stable prefix for VM-owned execution identities.
pub const EXECUTION_ID_PREFIX: &str = "hxe-";

/// Harn-owned identity for one top-level execution tree.
///
/// Construction is deliberately closed: values are either minted here or
/// parsed through the canonical UUIDv7 validator. Wire formats may carry a
/// string, but runtime authority never carries an unchecked one.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionId(Arc<str>);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid Harn execution identity")]
pub struct InvalidExecutionId;

impl ExecutionId {
    /// Mint a fresh UUIDv7 identity. UUIDv7 preserves useful creation-time
    /// ordering while remaining unique across processes and hosts.
    pub fn mint() -> Self {
        Self(Arc::from(format!(
            "{EXECUTION_ID_PREFIX}{}",
            uuid::Uuid::now_v7()
        )))
    }

    /// Parse an identity at a trust boundary, accepting only the canonical
    /// lowercase, hyphenated UUIDv7 representation owned by Harn.
    pub fn parse(candidate: &str) -> Result<Self, InvalidExecutionId> {
        let raw = candidate
            .strip_prefix(EXECUTION_ID_PREFIX)
            .ok_or(InvalidExecutionId)?;
        let value = uuid::Uuid::parse_str(raw).map_err(|_| InvalidExecutionId)?;
        if raw != value.hyphenated().to_string()
            || value.get_version_num() != 7
            || value.get_variant() != uuid::Variant::RFC4122
        {
            return Err(InvalidExecutionId);
        }
        Ok(Self(Arc::from(candidate)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ExecutionId")
            .field(&self.as_str())
            .finish()
    }
}

impl fmt::Display for ExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for ExecutionId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::ops::Deref for ExecutionId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl FromStr for ExecutionId {
    type Err = InvalidExecutionId;

    fn from_str(candidate: &str) -> Result<Self, Self::Err> {
        Self::parse(candidate)
    }
}

impl serde::Serialize for ExecutionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ExecutionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let candidate = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::parse(&candidate).map_err(serde::de::Error::custom)
    }
}

thread_local! {
    static ACTIVE_EXECUTION_SCOPE_STACK: RefCell<Vec<ExecutionId>> = const { RefCell::new(Vec::new()) };
}

/// Mint a fresh, durable execution id. UUIDv7 keeps identifiers unique across
/// processes and hosts while preserving useful creation-time ordering.
pub fn mint_execution_scope() -> ExecutionId {
    ExecutionId::mint()
}

/// RAII guard returned by [`enter_execution_scope`]. Popping the stack on drop
/// keeps the ambient balanced even when the enclosed program run panics or
/// returns an error.
#[must_use = "dropping the guard immediately pops the execution scope"]
pub struct ExecutionScopeGuard {
    _private: (),
}

impl Drop for ExecutionScopeGuard {
    fn drop(&mut self) {
        ACTIVE_EXECUTION_SCOPE_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Push `scope` onto the ambient stack for the lifetime of the returned guard.
/// The innermost entry wins for [`current_execution_scope`].
pub fn enter_execution_scope(scope: ExecutionId) -> ExecutionScopeGuard {
    ACTIVE_EXECUTION_SCOPE_STACK.with(|stack| stack.borrow_mut().push(scope));
    ExecutionScopeGuard { _private: () }
}

/// Currently-active execution scope, or `None` when no owning program run is
/// active on this task. Verdict issuance treats `None` as fail-closed.
pub fn current_execution_scope() -> Option<ExecutionId> {
    ACTIVE_EXECUTION_SCOPE_STACK.with(|stack| stack.borrow().last().cloned())
}

/// Replace the whole ambient stack, returning the previous one. Used by the
/// orchestration ambient-scope machinery to carry the owner across
/// `spawn_local` fan-out boundaries (which plain thread-locals do not cross),
/// mirroring how the current-session stack is propagated.
pub(crate) fn swap_execution_scope_stack(replacement: Vec<ExecutionId>) -> Vec<ExecutionId> {
    ACTIVE_EXECUTION_SCOPE_STACK
        .with(|stack| std::mem::replace(&mut *stack.borrow_mut(), replacement))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_returns_none_when_nothing_pushed() {
        // A fresh thread has no owning execution.
        std::thread::spawn(|| {
            assert_eq!(current_execution_scope(), None);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn guard_pops_on_drop_and_inner_shadows_outer() {
        std::thread::spawn(|| {
            let outer = mint_execution_scope();
            let inner = mint_execution_scope();
            assert_ne!(outer, inner);
            let _o = enter_execution_scope(outer.clone());
            assert_eq!(current_execution_scope().as_deref(), Some(&*outer));
            {
                let _i = enter_execution_scope(inner.clone());
                assert_eq!(current_execution_scope().as_deref(), Some(&*inner));
            }
            assert_eq!(current_execution_scope().as_deref(), Some(&*outer));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn parse_and_serde_reject_noncanonical_or_non_v7_ids() {
        let minted = ExecutionId::mint();
        assert_eq!(ExecutionId::parse(minted.as_str()), Ok(minted));

        let valid = "hxe-019c13e0-8080-7000-8000-000000000001";
        assert_eq!(ExecutionId::parse(valid).unwrap().as_str(), valid);
        assert_eq!(
            serde_json::to_string(&ExecutionId::parse(valid).unwrap()).unwrap(),
            format!("\"{valid}\"")
        );

        for invalid in [
            "cloud-run-id",
            "hxe-019C13E0-8080-7000-8000-000000000001",
            "hxe-019c13e0-8080-4000-8000-000000000001",
        ] {
            assert_eq!(ExecutionId::parse(invalid), Err(InvalidExecutionId));
            assert!(serde_json::from_str::<ExecutionId>(&format!("\"{invalid}\"")).is_err());
        }
    }
}
