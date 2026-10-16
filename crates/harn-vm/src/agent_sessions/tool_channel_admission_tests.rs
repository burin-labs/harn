use crate::llm::agent_session_host::{
    assistant_message_from_llm_result_for_test, dict_get, list_items, record_tool_results_for_test,
    reset_agent_session_host_state, seed_host_session_provider_model, vm_to_json,
};
use serde_json::json;

#[test]
fn producer_histories_admit_for_openai_and_anthropic_and_keep_failure_egress() {
    for (provider, model) in [
        ("openai", "gpt-5.6-luna"),
        ("anthropic", "claude-sonnet-4-5"),
    ] {
        reset_agent_session_host_state();
        let session_id = crate::agent_sessions::open_or_create_for_test(Some(format!(
            "recorded-tool-error-admission-{provider}-{}",
            uuid::Uuid::now_v7()
        )));
        seed_host_session_provider_model(&session_id, provider, model);
        crate::agent_sessions::claim_tool_format(&session_id, "native")
            .expect("native tool format claims");
        crate::agent_sessions::inject_message(
            &session_id,
            crate::schema::json_to_vm_value(&json!({"role": "user", "content": "inspect"})),
        )
        .expect("user turn injects");
        let llm_result_json = if provider == "anthropic" {
            json!({
                "provider": provider,
                "model": model,
                "text": "Checking the requested file.",
                "thinking": "Private continuation stays in transcript.",
                "blocks": [{
                    "type": "thinking",
                    "thinking": "Signed continuation.",
                    "signature": "signed-thinking",
                }],
                "_agent_tool_format": "native",
                "native_tool_calls": [{
                    "id": "call_failed",
                    "name": "inspect",
                    "arguments": {"path": "README.md"},
                }],
            })
        } else {
            json!({
                "provider": provider,
                "model": model,
                "text": "Checking the requested file.",
                "_agent_tool_format": "native",
                "native_tool_calls": [{
                    "id": "call_failed",
                    "name": "inspect",
                    "arguments": {"path": "README.md"},
                }],
            })
        };
        let llm_result = crate::schema::json_to_vm_value(&llm_result_json);
        crate::agent_sessions::inject_message(
            &session_id,
            assistant_message_from_llm_result_for_test(&llm_result),
        )
        .expect("assistant tool call injects");

        // This is the real host producer used after a tool dispatch. Its
        // failure flag and transcript provenance must survive admission.
        record_tool_results_for_test(
            &session_id,
            crate::schema::json_to_vm_value(&json!([{
                "tool_name": "inspect",
                "tool_call_id": "call_failed",
                "observation": "inspection failed",
                "ok": false,
            }])),
        );
        let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
        let messages = list_items(
            &dict_get(&transcript, "messages")
                .cloned()
                .unwrap_or(crate::value::VmValue::Nil),
        );
        let assistant = vm_to_json(&messages[1]);
        let recorded_result = vm_to_json(&messages[2]);
        assert_eq!(recorded_result["role"], "tool_result");
        assert_eq!(recorded_result["is_error"], true);
        assert_eq!(recorded_result["_harn"]["tool_call_id"], "call_failed");
        assert_eq!(recorded_result["_harn"]["outcome"], "error");

        let mut options = crate::llm::api::options::base_opts(provider);
        options.model = model.into();
        options.provider_overrides = None;
        if provider == "anthropic" {
            options.native_tools = Some(vec![json!({
                "name": "inspect",
                "input_schema": {"type": "object"},
            })]);
        }
        options.budget = Some(crate::llm::cost::LlmBudgetEnvelope {
            admission: Some(crate::llm::admission::AdmissionMode::Conservative),
            total_budget_usd: Some(1.0),
            ..Default::default()
        });
        options.messages = vec![assistant, recorded_result];
        let request = crate::llm::api::LlmRequestPayload::from(&options);
        assert!(request
            .messages
            .iter()
            .all(|message| message.get("_harn").is_none()));
        let previous =
            crate::llm::admission::swap_scope(crate::llm::admission::AdmissionScope::default());
        let reservation = crate::llm::admission::reserve(
            &options,
            &request,
            crate::llm::cost_context::LlmCallTokenProjection::for_options(&options),
        )
        .unwrap_or_else(|error| panic!("recorded {provider} tool history refused: {error:?}"));
        assert!(
            reservation.is_some(),
            "the paid {provider} route reserves its bound"
        );
        drop(reservation);
        crate::llm::admission::swap_scope(previous);

        if provider == "anthropic" {
            assert!(request.messages[0].get("provider_continuation").is_some());
            assert_eq!(
                request.messages[0]["reasoning"],
                "Private continuation stays in transcript."
            );
            let body = crate::llm::providers::AnthropicProvider::build_request_body(&request);
            let assistant_blocks = body["messages"][0]["content"]
                .as_array()
                .expect("Anthropic assistant blocks");
            assert!(assistant_blocks.iter().any(|block| block["type"] == "text"));
            assert!(assistant_blocks
                .iter()
                .any(|block| block["type"] == "thinking"));
            assert!(assistant_blocks
                .iter()
                .any(|block| block["type"] == "tool_use"));
            let wire_result = &body["messages"][1]["content"][0];
            assert_eq!(wire_result["type"], "tool_result");
            assert_eq!(wire_result["tool_use_id"], "call_failed");
            assert_eq!(wire_result["is_error"], true);
        }
    }
}
