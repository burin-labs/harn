//! Tool-channel history normalization tests, split from the host tests so the
//! production module keeps its exact source-length ratchet.

use crate::value::VmValue;
use serde_json::json;

use super::super::{
    assistant_message_from_llm_result, dict_get, list_items, record_tool_results_for_test,
    reset_agent_session_host_state, seed_host_session_provider_model, vm_to_json,
};

#[test]
fn text_only_route_repairs_stale_native_history_format() {
    let result = crate::schema::json_to_vm_value(&json!({
        "provider": "deepinfra",
        "model": "openai/gpt-oss-120b",
        "text": "",
        "_agent_tool_format": "native",
        "native_tool_calls": [{
            "id": "call_1",
            "name": "look",
            "arguments": {"file": "README.md"}
        }],
    }));

    let message = vm_to_json(&assistant_message_from_llm_result(&result));

    assert_eq!(message["role"], "assistant");
    let content = message["content"].as_str().expect("text history content");
    assert!(
        content.contains("<tool_call>") && content.contains("look({"),
        "the route's effective text channel must own history shape: {content}"
    );
    assert!(
        message.get("tool_calls").is_none(),
        "a stale requested format must not leak native calls onto a text-only route"
    );
    assert_eq!(message["_harn"]["kind"], "assistant");
    assert_eq!(message["_harn"]["tool_calls"][0]["name"], "look");
    assert_eq!(message["_harn"]["tool_calls"][0]["id"], "call_1");
}

#[test]
fn dispatch_receipt_overrides_stale_supported_text_grammar() {
    reset_agent_session_host_state();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(
        "fireworks-exact-text-history".to_string(),
    ));
    crate::agent_sessions::claim_tool_format(&session_id, "json")
        .expect("stale session preference claims");
    seed_host_session_provider_model(
        &session_id,
        "fireworks",
        "accounts/fireworks/models/gpt-oss-120b",
    );
    crate::agent_sessions::inject_message(
        &session_id,
        crate::schema::json_to_vm_value(&json!({"role": "user", "content": "inspect"})),
    )
    .expect("user turn injects");

    // This is the exact v9 split: the session/caller still says JSON, while
    // the provider transaction's resolved_dispatch receipt says text.
    let llm_result = crate::schema::json_to_vm_value(&json!({
        "provider": "fireworks",
        "model": "accounts/fireworks/models/gpt-oss-120b",
        "text": "",
        "_agent_tool_format": "json",
        "_effective_tool_format": "text",
        "native_tool_calls": [{
            "id": "call_1",
            "name": "look",
            "arguments": {"file": "README.md"}
        }],
        "tool_calls": [{
            "id": "call_1",
            "name": "look",
            "arguments": {"file": "README.md"}
        }],
    }));
    super::super::host_agent_session_record_assistant_builtin(
        &[VmValue::string(&session_id), llm_result],
        &mut String::new(),
    )
    .expect("assistant turn records");
    record_tool_results_for_test(
        &session_id,
        crate::schema::json_to_vm_value(&json!([{
            "tool_name": "look",
            "tool_call_id": "call_1",
            "observation": "[result of look]\nok\n[end of look result]\n",
            "ok": true
        }])),
    );

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let assistant = vm_to_json(&messages[1]);
    let result = vm_to_json(&messages[2]);
    let content = assistant["content"].as_str().expect("assistant content");
    assert!(
        content.contains("<tool_call>"),
        "exact text grammar: {content}"
    );
    assert!(
        !content.starts_with("```tool"),
        "stale JSON grammar leaked: {content}"
    );
    assert_eq!(result["role"], "user");
    assert!(result.get("tool_call_id").is_none());
    assert_eq!(assistant["_harn"]["tool_calls"][0]["name"], "look");
    assert_eq!(result["_harn"]["tool_call_id"], "call_1");
    assert_eq!(result["_harn"]["tool_name"], "look");
    assert_eq!(result["_harn"]["outcome"], "ok");
}

#[test]
fn json_text_route_reserializes_native_surprise_in_its_own_grammar() {
    let result = crate::schema::json_to_vm_value(&json!({
        "provider": "minimax",
        "model": "MiniMax-M2",
        "text": "",
        "_agent_tool_format": "json",
        "native_tool_calls": [{
            "id": "call_1",
            "name": "look",
            "arguments": {"file": "README.md"}
        }],
    }));

    let message = vm_to_json(&assistant_message_from_llm_result(&result));

    let content = message["content"]
        .as_str()
        .expect("JSON text history content");
    assert!(
        content.starts_with("```tool\n{"),
        "unexpected JSON history: {content}"
    );
    assert!(content.contains("\"name\": \"look\""));
    assert!(content.contains("\"args\""));
    assert!(message.get("tool_calls").is_none());
    assert_eq!(message["_harn"]["tool_calls"][0]["name"], "look");
    assert_eq!(message["_harn"]["tool_calls"][0]["id"], "call_1");
}

#[test]
fn text_only_stale_native_turn_records_text_channel_result_end_to_end() {
    reset_agent_session_host_state();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(
        "text-only-stale-native-history".to_string(),
    ));
    crate::agent_sessions::claim_tool_format(&session_id, "native")
        .expect("stale native session lock claims");
    seed_host_session_provider_model(&session_id, "deepinfra", "openai/gpt-oss-120b");
    crate::agent_sessions::inject_message(
        &session_id,
        crate::schema::json_to_vm_value(&json!({"role": "user", "content": "inspect"})),
    )
    .expect("user turn injects");

    let llm_result = crate::schema::json_to_vm_value(&json!({
        "provider": "deepinfra",
        "model": "openai/gpt-oss-120b",
        "text": "",
        "_agent_tool_format": "native",
        "native_tool_calls": [{
            "id": "call_1",
            "name": "look",
            "arguments": {"file": "README.md"}
        }],
    }));
    crate::agent_sessions::inject_message(
        &session_id,
        assistant_message_from_llm_result(&llm_result),
    )
    .expect("assistant turn injects");
    record_tool_results_for_test(
        &session_id,
        crate::schema::json_to_vm_value(&json!([{
            "tool_name": "look",
            "tool_call_id": "call_1",
            "observation": "[result of look]\nok\n[end of look result]\n",
            "ok": true
        }])),
    );

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let assistant = vm_to_json(&messages[1]);
    let result = vm_to_json(&messages[2]);
    assert!(assistant["content"]
        .as_str()
        .expect("assistant text history")
        .contains("<tool_call>"));
    assert!(assistant.get("tool_calls").is_none());
    assert_eq!(result["role"], "user");
    assert!(result.get("tool_call_id").is_none());
    assert_eq!(assistant["_harn"]["tool_calls"][0]["name"], "look");
    assert_eq!(result["_harn"]["tool_call_id"], "call_1");
    assert_eq!(result["_harn"]["outcome"], "ok");
}

#[test]
fn producer_histories_admit_for_openai_and_anthropic_and_keep_failure_egress() {
    for (provider, model) in [
        ("openai", "gpt-5.6-luna"),
        ("anthropic", "claude-sonnet-4-5"),
    ] {
        reset_agent_session_host_state();
        let session_id = crate::agent_sessions::open_or_create_for_test(Some(format!(
            "recorded-tool-error-admission-{provider}"
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
            assistant_message_from_llm_result(&llm_result),
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
                .unwrap_or(VmValue::Nil),
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
