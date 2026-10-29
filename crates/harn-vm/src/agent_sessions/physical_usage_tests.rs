use crate::llm::agent_session_host as host;
use host::{reset_agent_session_host_state, seed_host_session_provider_model, vm_to_json};

struct ResetTranscriptBudgetPolicy;

impl Drop for ResetTranscriptBudgetPolicy {
    fn drop(&mut self) {
        crate::agent_sessions::reset_default_transcript_budget_policy();
        crate::agent_sessions::reset_session_store();
        reset_agent_session_host_state();
    }
}

fn physical_usage_result() -> crate::llm::api::LlmResult {
    crate::llm::api::LlmResult {
        text: "measured response".to_string(),
        input_tokens: 11,
        output_tokens: 3,
        stop_reason: Some("stop".to_string()),
        ..crate::llm::api::LlmResult::new("openai", "gpt-5.6-luna")
    }
}

fn transcript_has_llm_call(session_id: &str) -> bool {
    crate::agent_sessions::transcript(session_id)
        .as_ref()
        .map(vm_to_json)
        .and_then(|transcript| transcript.get("events").cloned())
        .and_then(|events| events.as_array().cloned())
        .is_some_and(|events| {
            events.iter().any(|event| {
                event.get("kind").and_then(serde_json::Value::as_str) == Some("llm_call")
            })
        })
}

#[test]
fn physical_usage_survives_a_rejected_transcript_projection() {
    let _reset_policy = ResetTranscriptBudgetPolicy;
    crate::agent_sessions::reset_session_store();
    reset_agent_session_host_state();
    crate::agent_sessions::set_default_transcript_budget_policy(
        crate::agent_sessions::SessionTranscriptBudgetPolicy::reject(16, 16)
            .with_max_approx_bytes(Some(1)),
    );
    let bounded_session = crate::agent_sessions::open_or_create_for_test(Some(format!(
        "physical-usage-bounded-{}",
        uuid::Uuid::now_v7()
    )));
    seed_host_session_provider_model(&bounded_session, "openai", "gpt-5.6-luna");
    let options = crate::llm::api::LlmCallOptions {
        session_id: Some(bounded_session.clone()),
        ..Default::default()
    };

    host::record_provider_dispatch(&bounded_session);
    host::record_provider_attempt_usage(
        &bounded_session,
        "bounded-call",
        "openai",
        "gpt-5.6-luna",
        &physical_usage_result().usage(),
    );
    host::record_completed_call_usage(&options, &physical_usage_result(), "bounded-call");

    assert!(
        !transcript_has_llm_call(&bounded_session),
        "the bounded transcript must refuse the physical llm_call event"
    );
    let usage =
        host::physical_usage_for_session(&bounded_session).expect("bounded session physical usage");
    assert_eq!((usage.input_tokens, usage.output_tokens), (11, 3));
    assert_eq!(usage.provider_call_count, Some(1));
    assert_eq!(usage.usage_unknown_calls, 0);

    crate::agent_sessions::reset_default_transcript_budget_policy();
    let retained_session = crate::agent_sessions::open_or_create_for_test(Some(format!(
        "physical-usage-retained-{}",
        uuid::Uuid::now_v7()
    )));
    seed_host_session_provider_model(&retained_session, "openai", "gpt-5.6-luna");
    let options = crate::llm::api::LlmCallOptions {
        session_id: Some(retained_session.clone()),
        ..Default::default()
    };

    host::record_provider_dispatch(&retained_session);
    host::record_provider_attempt_usage(
        &retained_session,
        "retained-call",
        "openai",
        "gpt-5.6-luna",
        &physical_usage_result().usage(),
    );
    host::record_completed_call_usage(&options, &physical_usage_result(), "retained-call");

    assert!(
        transcript_has_llm_call(&retained_session),
        "the default policy contrast retains the physical llm_call event"
    );
    let usage = host::physical_usage_for_session(&retained_session)
        .expect("retained session physical usage");
    assert_eq!((usage.input_tokens, usage.output_tokens), (11, 3));
    assert_eq!(usage.provider_call_count, Some(1));
    assert_eq!(usage.usage_unknown_calls, 0);
}

#[test]
fn physical_usage_distinguishes_no_dispatch_from_dispatched_unknown_usage() {
    let not_run_session = format!("physical-usage-not-run-{}", uuid::Uuid::now_v7());
    seed_host_session_provider_model(&not_run_session, "openai", "gpt-5.6-luna");
    let not_run = host::physical_usage_value_for_session(&not_run_session)
        .expect("initialized session has a physical usage census");
    let not_run = vm_to_json(&not_run);
    assert_eq!(not_run["status"], "not_run");
    assert_eq!(not_run["provider_call_count"], 0);
    assert_eq!(not_run["usage_unknown_calls"], 0);
    assert_eq!(not_run["cost_usd"], 0.0);

    let unknown_session = format!("physical-usage-unknown-{}", uuid::Uuid::now_v7());
    seed_host_session_provider_model(&unknown_session, "openai", "gpt-5.6-luna");
    host::record_provider_dispatch(&unknown_session);
    let unknown = host::physical_usage_value_for_session(&unknown_session)
        .expect("dispatched session has a physical usage census");
    let unknown = vm_to_json(&unknown);
    assert_eq!(unknown["status"], "partial");
    assert_eq!(unknown["provider_call_count"], 1);
    assert_eq!(unknown["usage_unknown_calls"], 1);
    assert_eq!(unknown["unpriced_calls"], 1);
    assert!(unknown["cost_usd"].is_null());
}
