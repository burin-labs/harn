use super::*;

/// A native escalated call must retain producer facts when its result is
/// recorded, even though the session's primary tool channel is text.
#[test]
fn dispatched_escalation_result_records_native_role_and_data_on_text_locked_session() {
    reset_agent_session_host_state();
    let session_id =
        crate::agent_sessions::open_or_create(Some("record-native-under-text-lock".to_string()));
    crate::agent_sessions::claim_tool_format(&session_id, "text").expect("text lock claims");
    seed_host_session_provider_model(&session_id, "anthropic", "claude-sonnet-4-5");

    crate::agent_sessions::inject_message(
        &session_id,
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "role": "user", "content": "read main"
        })),
    )
    .expect("user turn injects");
    let llm_result = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
        "text": "",
        "_agent_tool_format": "native",
        "native_tool_calls": [{"id": "tc_0", "name": "read", "arguments": {"path": "main.rs"}}],
    }));
    crate::agent_sessions::inject_message(
        &session_id,
        assistant_message_from_llm_result(&llm_result),
    )
    .expect("assistant turn injects");

    let dispatch = crate::stdlib::json_to_vm_value(&serde_json::json!([{
        "tool_name": "read",
        "tool_use_id": "tc_0",
        "ok": true,
        "observation": "file contents",
        "data": {
            "command_status": "succeeded",
            "run_outcome": {"exit_code": 0}
        },
    }]));
    crate::llm::agent_session_host::record_tool_results_for_test(&session_id, dispatch);

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let last = vm_to_json(messages.last().expect("a recorded result message"));
    assert_eq!(last["role"], "tool_result");
    assert_eq!(last["tool_use_id"], "tc_0");
    assert_eq!(last["data"]["command_status"], "succeeded");
    assert_eq!(last["data"]["run_outcome"]["exit_code"], 0);

    let messages_json: Vec<serde_json::Value> = messages.iter().map(vm_to_json).collect();
    assert!(
        orphaned_tool_use_ids(&messages_json).is_empty(),
        "the dispatched native tool_use must be paired"
    );
}

/// Ordinary text-channel results must not acquire a null producer-data field.
#[test]
fn dispatched_text_channel_result_omits_absent_data() {
    reset_agent_session_host_state();
    let session_id =
        crate::agent_sessions::open_or_create(Some("record-text-homogeneous".to_string()));
    crate::agent_sessions::claim_tool_format(&session_id, "text").expect("text lock claims");
    seed_host_session_provider_model(&session_id, "moonshot", "moonshot/kimi-k2.7-code-highspeed");

    crate::agent_sessions::inject_message(
        &session_id,
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "role": "user", "content": "read main"
        })),
    )
    .expect("user turn injects");
    let llm_result = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "provider": "moonshot",
        "model": "moonshot/kimi-k2.7-code-highspeed",
        "text": "read({ path: \"main.rs\" })",
        "_agent_tool_format": "text",
        "native_tool_calls": [],
        "tool_calls": [{"id": "tc_0", "name": "read", "arguments": {"path": "main.rs"}}],
    }));
    crate::agent_sessions::inject_message(
        &session_id,
        assistant_message_from_llm_result(&llm_result),
    )
    .expect("assistant turn injects");

    let dispatch = crate::stdlib::json_to_vm_value(&serde_json::json!([{
        "tool_name": "read",
        "tool_call_id": "tc_0",
        "ok": true,
        "observation": "file contents",
    }]));
    crate::llm::agent_session_host::record_tool_results_for_test(&session_id, dispatch);

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let last = vm_to_json(messages.last().expect("a recorded result message"));
    assert_eq!(last["role"], "user");
    assert!(last.get("data").is_none());
}

/// Structured mutation receipts from dynamic/MCP tools must survive dispatch
/// into the durable transcript so completion policy need not infer writes from
/// human-readable output.
#[test]
fn dispatched_result_projects_mutation_facts_into_transcript_data() {
    reset_agent_session_host_state();
    let session_id =
        crate::agent_sessions::open_or_create(Some("record-structured-mutation-facts".to_string()));
    crate::agent_sessions::claim_tool_format(&session_id, "text").expect("text lock claims");
    seed_host_session_provider_model(&session_id, "moonshot", "moonshot/kimi-k2.7-code-highspeed");

    let dispatch = crate::stdlib::json_to_vm_value(&serde_json::json!([{
        "tool_name": "repo-workflows__add_internal_api_endpoint",
        "tool_call_id": "tc_0",
        "ok": true,
        "observation": "created endpoint",
        "data": {"endpoint": "GET /health"},
        "mutation_status": "applied",
        "changed_paths": [
            "src/internal-api/handlers/health.ts",
            "src/internal-api/routes.ts"
        ],
    }]));
    crate::llm::agent_session_host::record_tool_results_for_test(&session_id, dispatch);

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let last = vm_to_json(messages.last().expect("a recorded result message"));
    assert_eq!(last["data"]["endpoint"], "GET /health");
    assert_eq!(last["data"]["mutation_status"], "applied");
    assert_eq!(
        last["data"]["changed_paths"],
        serde_json::json!([
            "src/internal-api/handlers/health.ts",
            "src/internal-api/routes.ts"
        ])
    );
}

/// MCP and generated Harn tools return the handler value under the canonical
/// dispatch envelope's `result` key. Those receipts are just as authoritative
/// as receipts emitted directly by a first-party host tool.
#[test]
fn dispatched_dynamic_result_projects_nested_mutation_facts_into_transcript_data() {
    reset_agent_session_host_state();
    let session_id = crate::agent_sessions::open_or_create(Some(
        "record-nested-structured-mutation-facts".to_string(),
    ));
    crate::agent_sessions::claim_tool_format(&session_id, "text").expect("text lock claims");
    seed_host_session_provider_model(&session_id, "mock", "fixture-fast");

    let dispatch = crate::stdlib::json_to_vm_value(&serde_json::json!([{
        "tool_name": "crystallized-workers__add_worker",
        "tool_call_id": "tc_nested",
        "ok": true,
        "observation": "created worker",
        "result": {
            "schema": "harn.agent_tool_handler_result.v1",
            "text": "created worker",
            "data": {
                "worker": "Payment",
                "mutation_status": "applied",
                "changed_paths": ["workers/payment.rivet", "src/payment.rv"],
                "verification": {
                    "schema": "harn.agent_tool_postcondition.v1",
                    "status": "passed",
                    "verified_paths": ["workers/payment.rivet", "src/payment.rv"]
                }
            }
        }
    }]));
    crate::llm::agent_session_host::record_tool_results_for_test(&session_id, dispatch);

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let last = vm_to_json(messages.last().expect("a recorded result message"));
    assert_eq!(last["data"]["mutation_status"], "applied");
    assert_eq!(last["data"]["worker"], "Payment");
    assert_eq!(
        last["data"]["changed_paths"],
        serde_json::json!(["workers/payment.rivet", "src/payment.rv"])
    );
    assert_eq!(
        last["data"]["verification"],
        serde_json::json!({
            "schema": "harn.agent_tool_postcondition.v1",
            "status": "passed",
            "verified_paths": ["workers/payment.rivet", "src/payment.rv"]
        })
    );
}

#[test]
fn dispatched_idempotent_result_preserves_satisfied_unchanged_outcome() {
    reset_agent_session_host_state();
    let session_id =
        crate::agent_sessions::open_or_create(Some("record-idempotent-mutation-facts".to_string()));
    crate::agent_sessions::claim_tool_format(&session_id, "text").expect("text lock claims");
    seed_host_session_provider_model(&session_id, "mock", "fixture-fast");

    let dispatch = crate::stdlib::json_to_vm_value(&serde_json::json!([{
        "tool_name": "repo-workflows__add_internal_api_endpoint",
        "tool_call_id": "tc_0",
        "ok": true,
        "observation": "already current",
        "mutation_status": "unchanged",
        "changed_paths": [],
    }]));
    crate::llm::agent_session_host::record_tool_results_for_test(&session_id, dispatch);

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let last = vm_to_json(messages.last().expect("a recorded result message"));
    assert_eq!(last["data"]["mutation_status"], "unchanged");
}
