//! One host-owned allowance, retained across module initialization, calls and
//! asynchronous suspension. Hosts keep the handle for the intended lifetime;
//! cloning it shares the allowance rather than granting another one.
use std::future::{poll_fn, Future};
use std::task::Poll;

use super::*;

#[derive(Clone)]
pub struct ConservativeLlmBudget {
    scope: AdmissionScope,
}

impl ConservativeLlmBudget {
    /// Start an independent host allowance, or tighten the active parent's
    /// allowance when called from an existing execution.
    pub fn new(ceiling_usd: f64) -> Result<Self, VmError> {
        let active = SCOPE.with(|slot| slot.borrow().clone());
        let mut scope = if active.host_owned
            || active.machine.is_some()
            || crate::current_execution_scope().is_some()
        {
            active
        } else {
            AdmissionScope::default()
        };
        scope.host_owned = true;
        let budget = Self { scope };
        budget.tighten(ceiling_usd)?;
        Ok(budget)
    }

    /// Read conservative accounting without confusing it with actual usage.
    pub fn receipt(&self) -> Result<AdmissionReceipt, VmError> {
        self.scope.receipt().ok_or_else(|| {
            error(
                DenialKind::ScopeUnavailable,
                "host admission receipt unavailable",
            )
        })
    }

    /// A later request may reduce this allowance, but cannot reopen consumed
    /// allowance or raise a previously installed ceiling.
    pub fn tighten(&self, ceiling_usd: f64) -> Result<(), VmError> {
        let ceiling = money(ceiling_usd)?;
        let mut ledger = self
            .scope
            .ledger
            .lock()
            .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?;
        if ledger.prior_unreserved_attempt {
            return Err(error(
                DenialKind::LateActivation,
                "conservative admission must start before the first provider attempt",
            ));
        }
        ledger.ceiling = Some(ledger.ceiling.map_or(ceiling, |old| old.min(ceiling)));
        Ok(())
    }

    /// Install this allowance only while polling `inner`. Independent host
    /// tasks cannot observe each other's ledger, and every VM entry in this
    /// span inherits the same allowance. An unrelated handle cannot replace
    /// an active parent budget.
    pub async fn scope<F: Future>(&self, inner: F) -> Result<F::Output, VmError> {
        self.validate_parent()?;
        let mut ambient = crate::orchestration::AmbientExecutionScope::capture_for_inline_subtask();
        ambient.set_llm_admission(self.scope.clone());
        let mut inner = std::pin::pin!(crate::orchestration::scope_ambient(ambient, inner));
        poll_fn(|context| {
            // Futures can be constructed before entering a parent, or moved
            // under a different parent after suspension. Validate before every
            // ambient swap, not only when the wrapper is constructed.
            if let Err(error) = self.validate_parent() {
                return Poll::Ready(Err(error));
            }
            inner.as_mut().poll(context).map(Ok)
        })
        .await
    }

    fn validate_parent(&self) -> Result<(), VmError> {
        let active = SCOPE.with(|slot| slot.borrow().clone());
        if active.machine.as_ref().is_some_and(|machine| {
            self.scope
                .machine
                .as_ref()
                .is_none_or(|owned| !owned.same_scope(machine))
        }) {
            return Err(error(
                DenialKind::ScopeUnavailable,
                "a host allowance cannot discard the active machine spend budget",
            ));
        }
        if (active.host_owned || crate::current_execution_scope().is_some())
            && !Arc::ptr_eq(&active.ledger, &self.scope.ledger)
        {
            return Err(error(
                DenialKind::ScopeUnavailable,
                "an unrelated host allowance cannot replace the active execution budget",
            ));
        }
        Ok(())
    }
}
