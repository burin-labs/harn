//! Conservative monetary admission for one execution tree. Reservations are
//! separate from observed usage: missing usage never becomes a free attempt.
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use super::api::{LlmCallOptions, LlmRequestPayload, LlmResult};
use crate::value::{VmError, VmValue};

mod bound;
mod durable;
mod host;
use bound::AttemptBound;
use durable::DurableReservation;
pub use durable::{MachineSpendPolicy, MachineSpendQuota, MachineSpendReceipt};
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
    machine: Option<MachineSpendQuota>,
}

#[derive(Default)]
struct Ledger {
    ceiling: Option<Decimal>,
    prior_unreserved_attempt: bool,
    attempts_started: u64,
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
    money: MonetaryReservation,
    bound: AttemptBound,
}

/// A monetary hold shared by chat and native operations. Only this owner
/// moves money between in-flight, settled, and uncertain balances, and it
/// carries the durable machine-spend hold when a machine quota is active.
pub(crate) struct MonetaryReservation {
    scope: AdmissionScope,
    bound: Decimal,
    durable: Option<DurableReservation>,
    pending: bool,
}

impl Drop for MonetaryReservation {
    fn drop(&mut self) {
        if self.pending {
            let mut ledger = self.scope.ledger.lock().unwrap_or_else(|e| e.into_inner());
            ledger.in_flight -= self.bound;
            ledger.uncertain += self.bound;
        }
    }
}

impl AttemptReservation {
    pub(crate) fn settle(self, result: &LlmResult) -> Result<(), VmError> {
        if result.served_fast {
            self.money
                .scope
                .ledger
                .lock()
                .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?
                .contract_broken = true;
            if let Some(durable) = &self.money.durable {
                durable.invalidate()?;
            }
            return Err(error(
                DenialKind::ProviderContractViolation,
                "provider reported an unadmitted premium serving tier",
            ));
        }
        let Some((upper, token_limit_violated)) = self.bound.observed_upper(result) else {
            if self.bound.known_contract_violation(result) {
                self.money
                    .scope
                    .ledger
                    .lock()
                    .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?
                    .contract_broken = true;
                if let Some(durable) = &self.money.durable {
                    durable.invalidate()?;
                }
                return Err(error(
                    DenialKind::ProviderContractViolation,
                    "partial provider usage or route violated the admitted contract",
                ));
            }
            // Drop retains the reservation, including a successful response
            // without both usage counters. This is not reported as zero cost.
            return Ok(());
        };
        let actual = result.usage().cost_usd.map(money).transpose()?;
        self.money.settle_upper(upper, actual, token_limit_violated)
    }
}

impl MonetaryReservation {
    /// Fail closed for this execution and, when a machine quota is active,
    /// for every later process sharing it.
    pub(crate) fn retain_contract_violation(self) -> Result<(), VmError> {
        self.scope
            .ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contract_broken = true;
        // Drop keeps the reservation uncertain: downstream retries can cost
        // more than the final response's reported usage.
        match &self.durable {
            Some(durable) => durable.invalidate(),
            None => Ok(()),
        }
    }

    pub(crate) fn settle(self, observed_cost: Option<f64>) -> Result<(), VmError> {
        match observed_cost {
            Some(cost) => {
                let cost = money(cost)?;
                self.settle_upper(cost, Some(cost), false)
            }
            None => Ok(()), // Drop retains unknown usage as uncertain.
        }
    }

    fn settle_upper(
        mut self,
        upper: Decimal,
        actual: Option<Decimal>,
        token_limit_violated: bool,
    ) -> Result<(), VmError> {
        if token_limit_violated || upper > self.bound {
            // A provider/catalog contract violation cannot be undone; account
            // the evidence, then fail closed on this and all subsequent calls.
            let mut ledger =
                self.scope.ledger.lock().map_err(|_| {
                    error(DenialKind::ScopeUnavailable, "admission ledger poisoned")
                })?;
            ledger.in_flight -= self.bound;
            ledger.settled_upper += upper;
            ledger.contract_broken = true;
            self.pending = false;
            if let Some(durable) = &self.durable {
                durable.invalidate()?;
            }
            return Err(error(
                DenialKind::ProviderContractViolation,
                "provider usage exceeded an admitted token or cost bound",
            ));
        }
        // A failed durable settlement leaves both holds in place: Drop moves
        // the execution hold to uncertain and the machine hold stays reserved.
        if let Some(durable) = &self.durable {
            durable.settle(upper, actual)?;
        }
        let mut ledger = self
            .scope
            .ledger
            .lock()
            .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?;
        ledger.in_flight -= self.bound;
        ledger.settled_upper += upper;
        self.pending = false;
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
    if ledger.ceiling.is_some() || scope.machine.is_some() {
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
    // The provider registry marks a self-hosted runtime as a known-zero
    // billing route. It consumes neither the machine allowance nor an
    // uncertain reservation; an unknown paid route still fails closed below.
    if crate::llm_config::provider_is_self_hosted(&request.provider) {
        return Ok(None);
    }
    if ledger.ceiling.is_none() && scope.machine.is_none() {
        ledger.prior_unreserved_attempt = true;
        return Ok(None);
    }
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
    let hold = reserve_money(
        &scope,
        &mut ledger,
        bound.total(),
        opts.budget.as_ref().and_then(|b| b.max_cost_usd),
    )?;
    Ok(Some(AttemptReservation { money: hold, bound }))
}

fn reserve_money(
    scope: &AdmissionScope,
    ledger: &mut Ledger,
    bound: Decimal,
    per_call: Option<f64>,
) -> Result<MonetaryReservation, VmError> {
    if ledger.contract_broken {
        return Err(error(
            DenialKind::ProviderContractViolation,
            "a prior provider response violated the admitted bound",
        ));
    }
    if ledger.ceiling.is_none() && scope.machine.is_none() {
        return Err(error(
            DenialKind::ScopeUnavailable,
            "admission ceiling is missing",
        ));
    }
    if let Some(max) = per_call {
        if bound > money(max)? {
            ledger.denied += 1;
            return Err(error(
                DenialKind::InsufficientAllowance,
                "conservative attempt bound exceeds budget.max_cost_usd",
            ));
        }
    }
    if let Some(ceiling) = ledger.ceiling {
        if ledger.settled_upper + ledger.in_flight + ledger.uncertain + bound > ceiling {
            ledger.denied += 1;
            return Err(error(
                DenialKind::InsufficientAllowance,
                "conservative attempt bound exceeds the execution's remaining allowance",
            ));
        }
    }
    // Chat attempts and native operations draw on the same durable machine
    // allowance; an exhausted or broken quota refuses before transport.
    let durable = scope
        .machine
        .as_ref()
        .map(|machine| machine.reserve(bound))
        .transpose()?;
    ledger.in_flight += bound;
    ledger.attempts_started = ledger.attempts_started.saturating_add(1);
    Ok(MonetaryReservation {
        scope: scope.clone(),
        bound,
        durable,
        pending: true,
    })
}

/// Reserve a native operation's admitted monetary upper bound in the same
/// execution-tree ledger chat calls use. Activation and the hold are atomic.
pub(crate) fn reserve_decision(
    bound: f64,
    per_call: Option<f64>,
    total: Option<f64>,
) -> Result<MonetaryReservation, VmError> {
    let bound = money(bound)?;
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    let mut ledger = scope
        .ledger
        .lock()
        .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?;
    let budget = total.map(|total| super::cost::LlmBudgetEnvelope {
        admission: Some(AdmissionMode::Conservative),
        total_budget_usd: Some(total),
        ..Default::default()
    });
    activate(&mut ledger, budget.as_ref())?;
    reserve_money(&scope, &mut ledger, bound, per_call)
}

/// A measured allowance only exists after conservative authority was installed.
pub(crate) fn remaining_allowance() -> Option<f64> {
    SCOPE.with(|slot| {
        let scope = slot.borrow();
        let ledger = scope.ledger.lock().ok()?;
        let remaining =
            ledger.ceiling? - ledger.settled_upper - ledger.in_flight - ledger.uncertain;
        remaining.max(Decimal::ZERO).to_f64()
    })
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

pub(crate) fn machine_receipt() -> Result<Option<VmValue>, VmError> {
    let machine = SCOPE.with(|slot| slot.borrow().machine.clone());
    machine
        .map(|quota| {
            let receipt = quota.receipt()?;
            let value = serde_json::to_value(receipt).map_err(|_| {
                error(
                    DenialKind::ScopeUnavailable,
                    "machine spend receipt serialization failed",
                )
            })?;
            Ok(crate::schema::json_to_vm_value(&value))
        })
        .transpose()
}

pub(crate) fn machine_remaining_usd() -> Result<Option<f64>, VmError> {
    let machine = SCOPE.with(|slot| slot.borrow().machine.clone());
    machine
        .map(|quota| {
            let receipt = quota.receipt()?;
            if receipt.contract_broken {
                return Ok(0.0);
            }
            let remaining = [
                receipt.daily_remaining_microusd,
                receipt.monthly_remaining_microusd,
            ]
            .into_iter()
            .flatten()
            .min()
            .ok_or_else(|| error(DenialKind::InvalidBudget, "machine spend limit is absent"))?;
            Ok(remaining as f64 / 1_000_000.0)
        })
        .transpose()
}

pub(crate) fn execution_remaining_usd() -> Result<Option<f64>, VmError> {
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    let ledger = scope
        .ledger
        .lock()
        .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?;
    Ok(ledger.ceiling.map(|ceiling| {
        if ledger.contract_broken {
            0.0
        } else {
            (ceiling - ledger.settled_upper - ledger.in_flight - ledger.uncertain)
                .max(Decimal::ZERO)
                .to_f64()
                .unwrap_or(0.0)
        }
    }))
}

pub(crate) fn reset_unscoped_state() {
    SCOPE.with(|slot| {
        if !slot.borrow().host_owned && slot.borrow().machine.is_none() {
            slot.replace(AdmissionScope::default());
        }
    });
}

#[cfg(test)]
mod tests;
