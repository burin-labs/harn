use super::ObservedAttemptToken;
use crate::llm::{api::LlmResult, usage::LlmUsage};

pub(super) fn context(
    session_id: Option<&str>,
    opts: &crate::llm::api::LlmCallOptions,
    resident_tokens: i64,
) {
    let Some(session_id) = session_id else { return };
    let context_window = crate::llm_config::model_catalog_id_for_route(&opts.provider, &opts.model)
        .and_then(|id| crate::llm_config::model_catalog_entry(&id))
        .map(|model| model.runtime_context_window.unwrap_or(model.context_window))
        .filter(|tokens| *tokens > 0);
    crate::llm::emit_live_agent_event_sync(&crate::agent_events::AgentEvent::ContextUsage {
        session_id: session_id.to_string(),
        used_tokens: u64::try_from(resident_tokens).ok(),
        context_window,
        estimated: true,
    });
}

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
