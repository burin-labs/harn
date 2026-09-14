//! Conservative monetary admission for one execution tree. Reservations are
//! separate from observed usage: missing usage never becomes a free attempt.
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use rust_decimal::Decimal;

use super::api::{LlmCallOptions, LlmRequestPayload, LlmResult};
use crate::value::{VmError, VmValue};

mod bound;
mod host;
use bound::AttemptBound;
pub use host::ConservativeLlmBudget;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionMode {
    Conservative,
}

#[derive(Clone, Default)]
pub(crate) struct AdmissionScope {
    ledger: Arc<Mutex<Ledger>>,
    pub(crate) host_owned: bool,
}

#[derive(Default)]
struct Ledger {
    ceiling: Option<Decimal>,
    prior_unreserved_attempt: bool,
    settled_upper: Decimal,
    in_flight: Decimal,
    uncertain: Decimal,
    denied: u64,
    contract_broken: bool,
}

thread_local! {
    static SCOPE: RefCell<AdmissionScope> = RefCell::new(AdmissionScope::default());
}

pub(crate) fn swap_scope(scope: AdmissionScope) -> AdmissionScope {
    SCOPE.with(|slot| slot.replace(scope))
}

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DenialKind {
    InvalidBudget,
    UnsupportedBillingShape,
    UnknownPricing,
    ScopeUnavailable,
    LateActivation,
    InsufficientAllowance,
    ProviderContractViolation,
}

#[derive(serde::Serialize)]
struct AdmissionDenial<'a> {
    category: &'static str,
    kind: &'static str,
    reason: &'static str,
    admission_reason: DenialKind,
    message: &'a str,
}

fn error(admission_reason: DenialKind, message: &str) -> VmError {
    let denial = AdmissionDenial {
        category: "budget_exceeded",
        kind: "terminal",
        reason: "budget_exceeded",
        admission_reason,
        message,
    };
    VmError::Thrown(crate::schema::json_to_vm_value(
        &serde_json::to_value(denial).expect("admission denial contains only strings"),
    ))
}

fn money(value: f64) -> Result<Decimal, VmError> {
    if !value.is_finite() || value < 0.0 {
        return Err(error(
            DenialKind::InvalidBudget,
            "conservative admission requires a finite non-negative ceiling",
        ));
    }
    value
        .to_string()
        .parse()
        .map_err(|_| error(DenialKind::InvalidBudget, "invalid monetary ceiling"))
}

/// Captured by the future before transport. Cancellation and errors leave the
/// full bound uncertain; only a complete supported usage response can release it.
pub(crate) struct AttemptReservation {
    scope: AdmissionScope,
    bound: AttemptBound,
    pending: bool,
}

impl Drop for AttemptReservation {
    fn drop(&mut self) {
        if self.pending {
            let mut ledger = self.scope.ledger.lock().unwrap_or_else(|e| e.into_inner());
            ledger.in_flight -= self.bound.total();
            ledger.uncertain += self.bound.total();
        }
    }
}

impl AttemptReservation {
    pub(crate) fn settle(mut self, result: &LlmResult) -> Result<(), VmError> {
        if result.served_fast {
            self.scope
                .ledger
                .lock()
                .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?
                .contract_broken = true;
            return Err(error(
                DenialKind::ProviderContractViolation,
                "provider reported an unadmitted premium serving tier",
            ));
        }
        let Some((upper, token_limit_violated)) = self.bound.observed_upper(result) else {
            if self.bound.known_contract_violation(result) {
                self.scope
                    .ledger
                    .lock()
                    .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?
                    .contract_broken = true;
                return Err(error(
                    DenialKind::ProviderContractViolation,
                    "partial provider usage or route violated the admitted contract",
                ));
            }
            // Drop retains the reservation, including a successful response
            // without both usage counters. This is not reported as zero cost.
            return Ok(());
        };
        let mut ledger = self
            .scope
            .ledger
            .lock()
            .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?;
        ledger.in_flight -= self.bound.total();
        ledger.settled_upper += upper;
        self.pending = false;
        if token_limit_violated || upper > self.bound.total() {
            // A provider/catalog contract violation cannot be undone; account
            // the evidence, then fail closed on this and all subsequent calls.
            ledger.contract_broken = true;
            return Err(error(
                DenialKind::ProviderContractViolation,
                "provider usage exceeded an admitted token or cost bound",
            ));
        }
        Ok(())
    }
}

fn activate(
    ledger: &mut Ledger,
    budget: Option<&super::cost::LlmBudgetEnvelope>,
) -> Result<(), VmError> {
    let explicit = budget.filter(|b| b.admission.is_some());
    if let Some(budget) = explicit {
        let ceiling = money(budget.total_budget_usd.ok_or_else(|| {
            error(
                DenialKind::InvalidBudget,
                "conservative admission requires budget.total_budget_usd",
            )
        })?)?;
        if ledger.prior_unreserved_attempt {
            return Err(error(
                DenialKind::LateActivation,
                "conservative admission must start before the first provider attempt",
            ));
        }
        ledger.ceiling = Some(ledger.ceiling.map_or(ceiling, |old| old.min(ceiling)));
    }
    Ok(())
}

/// Auxiliary model operations have no supported monetary bound yet. Refuse
/// them in a conservative scope, and remember earlier unreserved operations
/// so a later activation cannot silently exclude their possible charges.
pub(crate) fn check_auxiliary(
    budget: Option<&super::cost::LlmBudgetEnvelope>,
    operation: &str,
) -> Result<(), VmError> {
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    let mut ledger = scope
        .ledger
        .lock()
        .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?;
    activate(&mut ledger, budget)?;
    if ledger.ceiling.is_some() {
        ledger.denied += 1;
        return Err(error(
            DenialKind::UnsupportedBillingShape,
            &format!("conservative admission does not support {operation}"),
        ));
    }
    ledger.prior_unreserved_attempt = true;
    Ok(())
}

pub(crate) fn reserve(
    opts: &LlmCallOptions,
    request: &LlmRequestPayload,
) -> Result<Option<AttemptReservation>, VmError> {
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    let mut ledger = scope
        .ledger
        .lock()
        .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?;
    activate(&mut ledger, opts.budget.as_ref())?;
    // Latch the execution ceiling even when the adaptive preflight refuses.
    // Such a refusal precedes transport, so it consumes no reservation.
    super::cost::check_llm_preflight_budget(opts)?;
    let Some(ceiling) = ledger.ceiling else {
        ledger.prior_unreserved_attempt = true;
        return Ok(None);
    };
    if ledger.contract_broken {
        return Err(error(
            DenialKind::ProviderContractViolation,
            "a prior provider response violated the admitted bound",
        ));
    }
    let bound = match AttemptBound::for_request(request) {
        Ok(bound) => bound,
        Err(err) => {
            ledger.denied += 1;
            return Err(err);
        }
    };
    if let Some(max) = opts.budget.as_ref().and_then(|b| b.max_cost_usd) {
        if bound.total() > money(max)? {
            ledger.denied += 1;
            return Err(error(
                DenialKind::InsufficientAllowance,
                "conservative attempt bound exceeds budget.max_cost_usd",
            ));
        }
    }
    if ledger.settled_upper + ledger.in_flight + ledger.uncertain + bound.total() > ceiling {
        ledger.denied += 1;
        return Err(error(
            DenialKind::InsufficientAllowance,
            "conservative attempt bound exceeds the execution's remaining allowance",
        ));
    }
    ledger.in_flight += bound.total();
    drop(ledger);
    Ok(Some(AttemptReservation {
        scope,
        bound,
        pending: true,
    }))
}

/// Upper accounting is deliberately named apart from the actual-usage ledger.
#[derive(Clone, Debug, serde::Serialize)]
pub struct AdmissionReceipt {
    pub mode: AdmissionMode,
    pub ceiling_usd: Decimal,
    pub settled_upper_usd: Decimal,
    pub in_flight_usd: Decimal,
    pub uncertain_usd: Decimal,
    pub denied_attempts: u64,
    pub contract_broken: bool,
}

impl AdmissionScope {
    fn receipt(&self) -> Option<AdmissionReceipt> {
        let ledger = self.ledger.lock().ok()?;
        Some(AdmissionReceipt {
            mode: AdmissionMode::Conservative,
            ceiling_usd: ledger.ceiling?,
            settled_upper_usd: ledger.settled_upper,
            in_flight_usd: ledger.in_flight,
            uncertain_usd: ledger.uncertain,
            denied_attempts: ledger.denied,
            contract_broken: ledger.contract_broken,
        })
    }
}

pub(crate) fn receipt() -> Option<VmValue> {
    SCOPE.with(|slot| {
        let receipt = slot.borrow().receipt()?;
        Some(crate::schema::json_to_vm_value(
            &serde_json::to_value(receipt).ok()?,
        ))
    })
}

pub(crate) fn reset_unscoped_state() {
    SCOPE.with(|slot| {
        if !slot.borrow().host_owned {
            slot.replace(AdmissionScope::default());
        }
    });
}

#[cfg(test)]
mod tests;
