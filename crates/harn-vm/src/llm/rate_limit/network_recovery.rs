//! Deadline-bounded admission through the existing route breaker registry.

use super::*;
use crate::llm::api::LlmCallOptions;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::PoisonError;

/// Preserve direct fail-fast checks of the breaker state machine.
#[cfg(test)]
pub(crate) fn check_network_breaker_for_llm_call(
    opts: &LlmCallOptions,
) -> Result<(), crate::value::VmError> {
    match network_breaker_admission(opts) {
        Some((remaining, reason)) => Err(breaker_open_error(
            &opts.provider,
            &opts.model,
            remaining,
            reason,
        )),
        None => Ok(()),
    }
}

pub(super) fn network_breaker_admission(
    opts: &LlmCallOptions,
) -> Option<(Duration, BreakerOpenReason)> {
    ensure_initialized_from_config();
    let keys = limiter_keys(&opts.provider, &opts.model);
    let now_ms = crate::clock_mock::instant_now().as_millis();
    let owner = PROMPT_PROBE_OWNER.with(|slot| slot.borrow().clone());
    let owner = owner.as_deref();
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    // Inspect every key before reserving any half-open probe. Otherwise a
    // blocked sibling key strands an earlier reservation with no provider call
    // to close it. An unproductive-completion open wins the reason tie-break so
    // its `circuit_open` category (loop degrades onto primary) is not masked by
    // a coincidental network open on a sibling key.
    let mut blocked: Option<(Duration, BreakerOpenReason)> = None;
    for key in &keys {
        let limiter = limiter_for_key(&mut registry.limiters, key);
        limiter.breaker.take_interactive_probe(now_ms, owner);
        if let Some((remaining, reason)) = limiter.breaker.blocked(now_ms) {
            let reason = limiter.breaker.reason_for_caller(reason, owner);
            blocked = Some(match blocked {
                Some((prev, prev_reason)) => (prev.max(remaining), prev_reason.max(reason)),
                None => (remaining, reason),
            });
        }
    }
    if blocked.is_none() {
        for key in keys {
            limiter_for_key(&mut registry.limiters, &key).breaker_block(now_ms);
        }
    }
    blocked
}

thread_local! {
    /// The session whose prompt the current poll belongs to. Prompts interleave
    /// on one thread, so the ambient scope swaps this around every poll and
    /// hands a copy to the workers a turn starts.
    static PROMPT_PROBE_OWNER: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn swap_prompt_probe_owner(replacement: Option<String>) -> Option<String> {
    PROMPT_PROBE_OWNER.with(|slot| slot.replace(replacement))
}

/// One person's prompt, from sending it until its turn ends.
#[must_use = "dropping the guard ends the prompt's claim on its probe"]
pub struct InteractivePromptGuard {
    owner: String,
}

/// Offer this prompt's next request on each failing route the chance to probe
/// it immediately. Other callers keep waiting. If the probe fails the route
/// reopens, and calls in this prompt's turn fail fast instead of waiting.
///
/// Call this inside the turn's ambient scope and hold the guard for the turn.
pub fn begin_interactive_prompt(session_id: &str) -> InteractivePromptGuard {
    static PROMPT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    ensure_initialized_from_config();
    // Unique per prompt, so a worker outliving its turn cannot act for the
    // session's next prompt.
    let owner = format!(
        "{session_id}#{}",
        PROMPT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    swap_prompt_probe_owner(Some(owner.clone()));
    let mut registry = registry().lock().expect("rate limiter mutex poisoned");
    for limiter in registry.limiters.values_mut() {
        limiter.breaker.offer_interactive_probe(&owner);
    }
    InteractivePromptGuard { owner }
}

impl Drop for InteractivePromptGuard {
    fn drop(&mut self) {
        // A cancelled turn drops this outside its poll, when the slot belongs
        // to whoever is polling now.
        PROMPT_PROBE_OWNER.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_deref() == Some(self.owner.as_str()) {
                *slot = None;
            }
        });
        let now_ms = crate::clock_mock::instant_now().as_millis();
        let mut registry = registry().lock().unwrap_or_else(PoisonError::into_inner);
        for limiter in registry.limiters.values_mut() {
            limiter.breaker.withdraw_prompt(&self.owner, now_ms);
        }
    }
}

/// Wait for network recovery without consuming a provider request. The VM's
/// execution/scope deadline and cancellation still interrupt this future.
/// Return the transport timeout left after waiting, in its existing seconds
/// contract; rounding down keeps admission from extending the call's budget.
pub(crate) async fn await_network_breaker_for_llm_call(
    opts: &LlmCallOptions,
) -> Result<Option<u64>, crate::value::VmError> {
    let started = crate::clock_mock::instant_now();
    let budget = Duration::from_secs(opts.resolve_timeout());
    let mut waited = false;
    loop {
        let elapsed = crate::clock_mock::instant_now().duration_since(started);
        let remaining_budget = budget.saturating_sub(elapsed);
        if waited && remaining_budget.as_secs() == 0 {
            // Nothing was sent, so this is an unreachable route rather than a
            // provider that took too long to answer.
            return Err(crate::value::VmError::CategorizedError {
                message: format!(
                    "no request was sent: '{}' was still recovering from network failures after \
                     {}s",
                    opts.provider,
                    budget.as_secs()
                ),
                category: crate::value::ErrorCategory::TransientNetwork,
            });
        }
        match network_breaker_admission(opts) {
            None => return Ok(waited.then_some(remaining_budget.as_secs())),
            Some((
                remaining,
                reason @ (BreakerOpenReason::UnproductiveCompletion
                | BreakerOpenReason::InteractiveProbeFailed),
            )) => {
                return Err(breaker_open_error(
                    &opts.provider,
                    &opts.model,
                    remaining,
                    reason,
                ));
            }
            Some((remaining, BreakerOpenReason::Network)) => {
                // Zero means another caller owns the half-open probe. Poll at
                // a bounded interval rather than spin or admit a second probe.
                let wait = remaining
                    .max(Duration::from_millis(50))
                    .min(remaining_budget);
                crate::events::log_info(
                    "llm.network_recovery",
                    &format!(
                        "Waiting {}ms for network recovery before provider dispatch",
                        wait.as_millis()
                    ),
                );
                crate::clock_mock::sleep(wait).await;
                waited = true;
            }
        }
    }
}
