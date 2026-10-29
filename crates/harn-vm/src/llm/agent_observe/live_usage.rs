use super::ObservedAttemptToken;
use crate::llm::{api::LlmResult, usage::LlmUsage};

#[inline(never)]
pub(super) fn settled(observed: &ObservedAttemptToken, call_id: &str, result: &LlmResult) -> bool {
    let Some(session_id) = observed.session_id.as_deref() else {
        return false;
    };
    let dispatched = observed.dispatches.observed();
    if dispatched == 0 {
        return false;
    }
    let usage = LlmUsage::aggregate_attempt_ledger(
        &[result.usage()],
        usize::try_from(dispatched).unwrap_or(usize::MAX),
    );
    crate::llm::agent_session_host::record_provider_attempt_usage(
        session_id,
        call_id,
        &result.provider,
        &result.model,
        &usage,
    );
    true
}

#[inline(never)]
pub(super) fn failed(
    observed: &ObservedAttemptToken,
    call_id: &str,
    provider: &str,
    model: &str,
    reported: Option<&LlmUsage>,
) {
    let Some(session_id) = observed.session_id.as_deref() else {
        return;
    };
    let dispatched = observed.dispatches.observed();
    if dispatched == 0 {
        return;
    }
    let usage = reported.cloned().unwrap_or_else(LlmUsage::unknown_attempt);
    let usage = LlmUsage::aggregate_attempt_ledger(
        &[usage],
        usize::try_from(dispatched).unwrap_or(usize::MAX),
    );
    crate::llm::agent_session_host::record_provider_attempt_usage(
        session_id, call_id, provider, model, &usage,
    );
}
