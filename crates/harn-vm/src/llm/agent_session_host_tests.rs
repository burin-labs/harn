use serde_json::json;

use super::{
    agent_turn_made_no_llm_call, assistant_message_from_llm_result, canonical_acp_stop_reason,
    canonical_provider_stop_reason, initial_user_content, is_length_truncation,
    last_assistant_text, text_has_tool_call_prefix, tool_result_message_for_provider,
    truncated_tool_call_should_continue, vm_to_json,
};

#[test]
fn model_less_turn_is_flagged_as_no_llm_call() {
    // Zero iterations + zero tokens + non-error status = silent
    // short-circuit. This is the model-less turn we must fail loud on.
    assert!(agent_turn_made_no_llm_call("", false, 0, 0, 0));
    assert!(agent_turn_made_no_llm_call("done", false, 0, 0, 0));
}

#[test]
fn real_turn_is_not_flagged_as_no_llm_call() {
    // Any real provider round-trip records iterations and/or tokens.
    assert!(!agent_turn_made_no_llm_call("done", false, 1, 0, 0));
    assert!(!agent_turn_made_no_llm_call("done", false, 0, 12, 0));
    assert!(!agent_turn_made_no_llm_call("done", false, 0, 0, 34));
    // Already-errored or terminal-error turns are left as-is.
    assert!(!agent_turn_made_no_llm_call("error", false, 0, 0, 0));
    assert!(!agent_turn_made_no_llm_call("failed", false, 0, 0, 0));
    assert!(!agent_turn_made_no_llm_call("", true, 0, 0, 0));
}

#[test]
fn native_tool_calls_replay_with_openai_wire_shape() {
    let result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "local",
        "text": "",
        "native_tool_calls": [{
            "id": "call_001",
            "name": "release_run",
            "arguments": {"command": "git status --short"}
        }],
    }));
    let message = vm_to_json(&assistant_message_from_llm_result(&result));

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["tool_calls"][0]["id"], "call_001");
    assert_eq!(message["tool_calls"][0]["type"], "function");
    assert_eq!(message["tool_calls"][0]["function"]["name"], "release_run");
    assert_eq!(
        message["tool_calls"][0]["function"]["arguments"],
        r#"{"command":"git status --short"}"#
    );
}

#[test]
fn initial_user_content_preserves_multimodal_blocks() {
    let mut opts = crate::value::DictMap::new();
    opts.insert(
        "initial_user_content".to_string(),
        crate::stdlib::json_to_vm_value(&json!([
            {"type": "text", "text": "Describe this image."},
            {
                "type": "image",
                "media_type": "image/png",
                "base64": "aGVsbG8="
            }
        ])),
    );

    let content = initial_user_content(&opts, "Describe this image.");

    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["base64"], "aGVsbG8=");
}

#[test]
fn initial_user_content_falls_back_to_text_message() {
    let opts = crate::value::DictMap::new();

    assert_eq!(
        initial_user_content(&opts, "hello"),
        serde_json::Value::String("hello".to_string())
    );
}

#[test]
fn tool_results_replay_with_provider_appropriate_ids() {
    let local = vm_to_json(&tool_result_message_for_provider(
        "local",
        "Qwen/Qwen3.6-35B-A3B",
        "native",
        "release_run",
        "call_001",
        "ok",
    ));
    assert_eq!(local["role"], "tool");
    assert_eq!(local["name"], "release_run");
    assert_eq!(local["tool_call_id"], "call_001");

    let anthropic = vm_to_json(&tool_result_message_for_provider(
        "anthropic",
        "claude-opus-4-7",
        "native",
        "release_run",
        "call_002",
        "ok",
    ));
    assert_eq!(anthropic["role"], "tool_result");
    assert_eq!(anthropic["tool_use_id"], "call_002");

    let bedrock_claude = vm_to_json(&tool_result_message_for_provider(
        "bedrock",
        "anthropic.claude-3-5-sonnet-20240620-v1:0",
        "native",
        "release_run",
        "call_003",
        "ok",
    ));
    assert_eq!(bedrock_claude["role"], "tool_result");
    assert_eq!(bedrock_claude["tool_use_id"], "call_003");

    let gemini = vm_to_json(&tool_result_message_for_provider(
        "gemini",
        "gemini-2.5-flash",
        "native",
        "release_run",
        "call_004",
        "ok",
    ));
    assert_eq!(gemini["role"], "tool");
    assert_eq!(gemini["name"], "release_run");
    assert_eq!(gemini["tool_call_id"], "call_004");

    let text_mode = vm_to_json(&tool_result_message_for_provider(
        "ollama",
        "devstral-small-2:24b",
        "text",
        "release_run",
        "call_005",
        "ok",
    ));
    assert_eq!(text_mode["role"], "user");
    assert!(text_mode.get("tool_call_id").is_none());
    assert!(text_mode.get("tool_use_id").is_none());
}

#[test]
fn final_visible_text_skips_control_only_assistant_turns() {
    let snapshot = crate::stdlib::json_to_vm_value(&json!({
        "messages": [
            {"role": "assistant", "content": "Final answer before sentinel."},
            {"role": "assistant", "content": "\n\n##DONE##"}
        ]
    }));

    assert_eq!(
        last_assistant_text(&snapshot).as_deref(),
        Some("Final answer before sentinel.")
    );
}

#[test]
fn iteration_cap_maps_to_max_turn_requests() {
    assert_eq!(
        canonical_acp_stop_reason("budget_exhausted", 5, 5, None),
        "max_turn_requests"
    );
    assert_eq!(
        canonical_acp_stop_reason("budget_exhausted", 6, 5, Some("end_turn")),
        "max_turn_requests"
    );
}

#[test]
fn other_budget_paths_also_map_to_max_turn_requests() {
    // Token / cost / autonomy budgets all stop the loop short, so
    // they share the canonical ACP reason even when iterations are
    // below the cap.
    assert_eq!(
        canonical_acp_stop_reason("budget_exhausted", 2, 50, Some("end_turn")),
        "max_turn_requests"
    );
}

#[test]
fn provider_max_tokens_promoted_when_loop_clean() {
    assert_eq!(
        canonical_acp_stop_reason("done", 1, 50, Some("max_tokens")),
        "max_tokens"
    );
    // OpenAI flavor.
    assert_eq!(
        canonical_acp_stop_reason("done", 1, 50, Some("length")),
        "max_tokens"
    );
    // Case-insensitive on the provider value.
    assert_eq!(
        canonical_acp_stop_reason("done", 1, 50, Some("MAX_TOKENS")),
        "max_tokens"
    );
}

#[test]
fn provider_stop_reason_normalization_is_shared_with_transcripts() {
    assert_eq!(canonical_provider_stop_reason(Some("length")), "max_tokens");
    assert_eq!(canonical_provider_stop_reason(Some("refusal")), "refusal");
    assert_eq!(canonical_provider_stop_reason(Some("tool_use")), "end_turn");
    assert_eq!(canonical_provider_stop_reason(None), "end_turn");
}

#[test]
fn anthropic_refusal_stop_reason_maps_to_refusal() {
    assert_eq!(
        canonical_acp_stop_reason("done", 1, 50, Some("refusal")),
        "refusal"
    );
}

#[test]
fn natural_completion_maps_to_end_turn() {
    assert_eq!(
        canonical_acp_stop_reason("done", 1, 50, Some("end_turn")),
        "end_turn"
    );
    assert_eq!(canonical_acp_stop_reason("", 1, 50, None), "end_turn");
    // Anthropic `tool_use` is normal mid-turn behavior; if it
    // somehow surfaced as the last call's stop_reason (loop ended
    // before the next turn ran), it still represents a clean stop.
    assert_eq!(
        canonical_acp_stop_reason("done", 1, 50, Some("tool_use")),
        "end_turn"
    );
}

#[test]
fn budget_exhausted_overrides_provider_signal() {
    // The loop ran out of budget before the model could refuse or
    // truncate again, so loop-level cap wins.
    assert_eq!(
        canonical_acp_stop_reason("budget_exhausted", 50, 50, Some("max_tokens")),
        "max_turn_requests"
    );
    assert_eq!(
        canonical_acp_stop_reason("budget_exhausted", 50, 50, Some("refusal")),
        "max_turn_requests"
    );
}

#[test]
fn length_truncation_recognized_across_provider_spellings() {
    // Keyed on the normalized condition, not one wire format.
    assert!(is_length_truncation(Some("length"))); // OpenAI/OpenRouter/Ollama
    assert!(is_length_truncation(Some("max_tokens"))); // Anthropic
    assert!(is_length_truncation(Some("LENGTH"))); // case-insensitive
    assert!(!is_length_truncation(Some("stop")));
    assert!(!is_length_truncation(Some("end_turn")));
    assert!(!is_length_truncation(Some("tool_use")));
    assert!(!is_length_truncation(Some("refusal")));
    assert!(!is_length_truncation(None));
}

#[test]
fn truncated_tool_call_prefix_detection_covers_both_wire_shapes() {
    // Tagged opener.
    assert!(text_has_tool_call_prefix(
        "let me edit\n<tool_call>\nedit({ path: \"a.rs\", body: <<EOF\nfn"
    ));
    // Bare `name(` at line start.
    assert!(text_has_tool_call_prefix(
        "I'll write the file.\nwrite_file({ path: \"a.rs\", contents: <<EOF\nfn main"
    ));
    // Pure prose with no call shape — not a truncated call.
    assert!(!text_has_tool_call_prefix(
        "Here is a long explanation of the algorithm that just kept going"
    ));
    // A bare ident with no opening paren is not a call prefix.
    assert!(!text_has_tool_call_prefix(
        "write_file is the tool you want"
    ));
}

#[test]
fn auto_continue_fires_on_length_truncation_with_partial_call() {
    // (a) finish_reason == length + truncated tool-call prefix with zero
    // resolved calls -> auto-continue.
    let truncated_body = "edit({ path: \"a.rs\", body: <<EOF\nfn main() {";
    // Via a parser diagnostic (unterminated heredoc).
    assert!(truncated_tool_call_should_continue(
        Some("length"),
        truncated_body,
        0,
        true,
    ));
    // Via the text prefix alone, even with no parser diagnostic surfaced.
    assert!(truncated_tool_call_should_continue(
        Some("max_tokens"),
        truncated_body,
        0,
        false,
    ));
}

#[test]
fn auto_continue_does_not_fire_when_calls_resolved() {
    // A length truncation that still landed a usable tool call made real
    // progress; do not re-issue.
    assert!(!truncated_tool_call_should_continue(
        Some("length"),
        "edit({ path: \"a.rs\", body: <<EOF\nfn main() {}\nEOF })",
        1,
        false,
    ));
}

#[test]
fn auto_continue_does_not_fire_on_clean_stop_with_malformed_call() {
    // (c) Clean stop + malformed call -> NOT auto-continue. This is the
    // #3137/#3142 domain (parse-tolerance / reasoning-leak); the
    // length-truncation gate is what keeps the two from colliding.
    let malformed = "edit({ path: \"a.rs\" body \"oops\" })";
    assert!(!truncated_tool_call_should_continue(
        Some("stop"),
        malformed,
        0,
        true,
    ));
    assert!(!truncated_tool_call_should_continue(
        Some("end_turn"),
        malformed,
        0,
        true,
    ));
    assert!(!truncated_tool_call_should_continue(
        None, malformed, 0, true
    ));
}

#[test]
fn auto_continue_does_not_fire_on_length_truncated_prose() {
    // A model that simply ran long on prose with no tool intent should not
    // trigger a continuation: there is no partial-call signal.
    assert!(!truncated_tool_call_should_continue(
        Some("length"),
        "Here is a very long explanation that ran past the token cap",
        0,
        false,
    ));
}

mod nested_budget_tests {
    use crate::orchestration::{
        clear_execution_policy_stacks, current_execution_policy, pop_execution_policy,
        push_execution_policy, CapabilityPolicy,
    };
    use crate::value::{VmDictExt, VmError, VmValue};

    use super::super::{build_nested_budget_denial, install_session_nested_budget};
    use super::vm_to_json;

    fn policy_value(policy: &CapabilityPolicy) -> VmValue {
        crate::stdlib::json_to_vm_value(&serde_json::to_value(policy).unwrap())
    }

    fn empty_session_id() -> String {
        format!("test_session_{}", uuid::Uuid::now_v7())
    }

    #[test]
    fn install_session_nested_budget_rejects_when_parent_is_zero() {
        clear_execution_policy_stacks();
        let parent = CapabilityPolicy {
            recursion_limit: Some(0),
            ..Default::default()
        };
        push_execution_policy(parent);

        let opts_map = crate::value::DictMap::new();
        let session_id = empty_session_id();
        let error = install_session_nested_budget(&opts_map, &session_id).unwrap_err();
        match error {
            VmError::CategorizedError { message, category } => {
                assert_eq!(category.as_str(), "budget_exceeded");
                assert!(message.contains("agent_loop"), "missing kind: {message}");
                assert!(message.contains(&session_id), "missing label: {message}");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
        pop_execution_policy();
    }

    #[test]
    fn install_session_nested_budget_decrements_when_parent_has_room() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(3),
            ..Default::default()
        });

        let opts_map = crate::value::DictMap::new();
        let guard = install_session_nested_budget(&opts_map, "child").unwrap();
        assert_eq!(guard.parent_limit, Some(3));
        assert_eq!(guard.child_limit, Some(2));
        assert_eq!(current_execution_policy().unwrap().recursion_limit, Some(2));
        drop(guard);
        pop_execution_policy();
    }

    #[test]
    fn install_session_nested_budget_reads_kind_and_label_from_options() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(0),
            ..Default::default()
        });

        let mut opts_map = crate::value::DictMap::new();
        opts_map.put_str("_nested_kind", "sub_agent_run");
        opts_map.put_str("_nested_label", "research-worker");
        let error = install_session_nested_budget(&opts_map, "ignored").unwrap_err();
        match error {
            VmError::CategorizedError { message, .. } => {
                assert!(
                    message.contains("sub_agent_run"),
                    "kind not surfaced: {message}"
                );
                assert!(
                    message.contains("research-worker"),
                    "label not surfaced: {message}"
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        pop_execution_policy();
    }

    #[test]
    fn install_session_nested_budget_intersects_requested_policy() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(10),
            ..Default::default()
        });

        let mut opts_map = crate::value::DictMap::new();
        opts_map.insert(
            "policy".to_string(),
            policy_value(&CapabilityPolicy {
                recursion_limit: Some(1),
                ..Default::default()
            }),
        );
        let guard = install_session_nested_budget(&opts_map, "child").unwrap();
        // Parent had Some(10); decremented to Some(9). Intersected with
        // the requested ceiling Some(1) yields the tighter Some(1).
        assert_eq!(guard.child_limit, Some(1));
        drop(guard);
        pop_execution_policy();
    }

    #[test]
    fn build_nested_budget_denial_carries_budget_exceeded_category() {
        let error = VmError::CategorizedError {
            message: "nested execution budget exhausted before sub_agent_run: research-worker"
                .to_string(),
            category: crate::value::ErrorCategory::BudgetExceeded,
        };
        let result = build_nested_budget_denial("session-x", "go", &error);
        let json = vm_to_json(&result);
        assert_eq!(json["final_status"], "blocked");
        assert_eq!(json["stop_reason"], "nested_execution_budget_exhausted");
        assert_eq!(json["error"]["category"], "budget_exceeded");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("research-worker"));
        assert_eq!(json["session_id"], "session-x");
        assert_eq!(json["task"], "go");
    }
}
