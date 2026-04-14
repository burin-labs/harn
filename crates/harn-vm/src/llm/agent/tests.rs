// Direct-path imports: tests name symbols at the submodule that
// actually defines them. No re-export shim in `super::` (the
// `agent/mod.rs` orchestrator) is required.
use super::helpers::{
    action_turn_nudge, assistant_history_text, has_successful_tools,
    loop_state_requests_phase_change, prose_exceeds_budget, sentinel_without_action_nudge,
    should_stop_after_successful_tools, trim_prose_for_history,
};
use super::run_agent_loop_internal;
use crate::bridge::HostBridge;
use crate::llm::agent_config::{build_llm_call_result, AgentLoopConfig};
use crate::llm::agent_observe::{extract_retry_after_ms, observed_llm_call, LlmRetryConfig};
use crate::llm::agent_tools::{
    merge_agent_loop_policy, normalize_native_tools_for_format, normalize_tool_choice_for_format,
    normalize_tool_examples_for_format, required_tool_choice_for_provider,
};
use crate::llm::api::{LlmCallOptions, LlmResult};
use crate::llm::daemon::{persist_snapshot, DaemonLoopConfig, DaemonSnapshot};
use crate::llm::mock::{get_llm_mock_calls, reset_llm_mock_state};
use crate::orchestration::{pop_execution_policy, push_execution_policy, TurnPolicy};
use crate::tool_annotations::{ToolAnnotations, ToolKind};
use crate::value::{VmError, VmValue};
use serde_json::json;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

fn base_opts(messages: Vec<serde_json::Value>) -> LlmCallOptions {
    LlmCallOptions {
        provider: "mock".to_string(),
        model: "mock".to_string(),
        api_key: String::new(),
        messages,
        system: None,
        transcript_id: None,
        transcript_summary: None,
        transcript_metadata: None,
        max_tokens: 128,
        temperature: None,
        top_p: None,
        top_k: None,
        stop: None,
        seed: None,
        frequency_penalty: None,
        presence_penalty: None,
        response_format: None,
        json_schema: None,
        output_schema: None,
        output_validation: None,
        thinking: None,
        tools: None,
        native_tools: None,
        tool_choice: None,
        cache: false,
        timeout: None,
        idle_timeout: None,
        stream: true,
        provider_overrides: None,
    }
}

fn base_agent_config() -> AgentLoopConfig {
    AgentLoopConfig {
        persistent: false,
        max_iterations: 1,
        max_nudges: 1,
        nudge: None,
        done_sentinel: None,
        break_unless_phase: None,
        tool_retries: 0,
        tool_backoff_ms: 1,
        tool_format: "text".to_string(),
        auto_compact: None,
        policy: None,
        approval_policy: None,
        daemon: false,
        daemon_config: DaemonLoopConfig::default(),
        llm_retries: 0,
        llm_backoff_ms: 1,
        exit_when_verified: false,
        loop_detect_warn: 0,
        loop_detect_block: 0,
        loop_detect_skip: 0,
        tool_examples: None,
        turn_policy: None,
        stop_after_successful_tools: None,
        require_successful_tools: None,
        session_id: "test_session".to_string(),
        event_sink: None,
        task_ledger: Default::default(),
        post_turn_callback: None,
    }
}

#[test]
fn detects_phase_change_from_latest_loop_state_footer() {
    let text = "First\n\n## LOOP_STATE\nphase: assess\nnext_phase: ground\n## END_LOOP_STATE\n\nSecond\n\n## LOOP_STATE\nphase: ground\nnext_phase: execute\n## END_LOOP_STATE";
    assert!(loop_state_requests_phase_change(text, "ground"));
    assert!(!loop_state_requests_phase_change(text, "execute"));
}

#[test]
fn assistant_history_prefers_canonical_over_raw_text() {
    // Under the tagged response protocol the parser reconstructs a
    // canonical form of the turn. Replaying that form — not the raw
    // provider bytes — is what closes the self-poison loop where leading
    // raw code became "what the agent said" on the next turn.
    let raw_text = "def foo(): pass\n<tool_call>\nread({ path: \"src/lib.rs\" })\n</tool_call>";
    let canonical = "<tool_call>\nread({\n  \"path\": \"src/lib.rs\"\n})\n</tool_call>";
    let tool_calls = vec![json!({"name": "read", "arguments": {"path": "src/lib.rs"}})];

    let replayed = assistant_history_text(Some(canonical), raw_text, 0, &tool_calls);
    assert_eq!(replayed, canonical);
    assert!(
        !replayed.contains("def foo"),
        "raw leading code must NOT leak into replayed history: {replayed}",
    );
}

#[test]
fn assistant_history_falls_back_to_raw_when_no_canonical() {
    // Native tool-call mode and no-tools paths don't run the tagged
    // parser, so canonical is None. In that case we still need a
    // non-empty replay so the model remembers what it said.
    let raw_text = "Short native-mode response.";
    let replayed = assistant_history_text(None, raw_text, 0, &[]);
    assert_eq!(replayed, "Short native-mode response.");
}

#[test]
fn assistant_history_elides_malformed_turns() {
    // When parsing failed we still want a compact placeholder so the next
    // iteration doesn't see (and mutate) its own broken syntax. The
    // placeholder fires irrespective of whether canonical was captured.
    let raw_text = "<tool_call>\nread({ path: 'broken }\n</tool_call>";
    let tool_calls = vec![json!({"name": "read"})];
    let replayed = assistant_history_text(None, raw_text, 2, &tool_calls);
    assert!(replayed.contains("malformed tool call"));
    assert!(!replayed.contains("'broken"));
}

#[test]
fn retry_after_from_runtime_error() {
    let err = VmError::Runtime("rate limited, retry-after: 5".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(5000));
}

#[test]
fn retry_after_from_thrown_string() {
    let err = VmError::Thrown(VmValue::String(Rc::from(
        "HTTP 429 Retry-After: 2.5 seconds",
    )));
    assert_eq!(extract_retry_after_ms(&err), Some(2500));
}

#[test]
fn retry_after_case_insensitive() {
    let err = VmError::Runtime("RETRY-AFTER: 10".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(10000));
}

#[test]
fn merge_agent_loop_policy_narrows_to_ceiling() {
    push_execution_policy(crate::orchestration::CapabilityPolicy {
        side_effect_level: Some("workspace_write".to_string()),
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec!["write_text".to_string(), "read_text".to_string()],
        )]),
        ..Default::default()
    });
    // Request a higher side-effect level but only a subset of capabilities.
    let merged = merge_agent_loop_policy(Some(crate::orchestration::CapabilityPolicy {
        side_effect_level: Some("process_exec".to_string()),
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec!["write_text".to_string()],
        )]),
        ..Default::default()
    }))
    .expect("merged policy")
    .expect("policy present");
    pop_execution_policy();

    // Side-effect level narrowed to the ceiling's lower level.
    assert_eq!(merged.side_effect_level.as_deref(), Some("workspace_write"));
    // Capabilities narrowed to the requested subset within the ceiling.
    assert_eq!(
        merged.capabilities.get("workspace"),
        Some(&vec!["write_text".to_string()])
    );
}

#[test]
fn merge_agent_loop_policy_rejects_exceeding_capabilities() {
    push_execution_policy(crate::orchestration::CapabilityPolicy {
        side_effect_level: Some("workspace_write".to_string()),
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec!["write_text".to_string()],
        )]),
        ..Default::default()
    });
    let result = merge_agent_loop_policy(Some(crate::orchestration::CapabilityPolicy {
        side_effect_level: Some("process_exec".to_string()),
        capabilities: std::collections::BTreeMap::from([(
            "process".to_string(),
            vec!["exec".to_string()],
        )]),
        ..Default::default()
    }));
    pop_execution_policy();

    assert!(
        result.is_err(),
        "should reject capabilities outside ceiling"
    );
}

#[test]
fn merge_approval_policy_intersects_with_ambient_ceiling() {
    use crate::llm::agent_tools::merge_agent_loop_approval_policy;
    use crate::orchestration::{pop_approval_policy, push_approval_policy, ToolApprovalPolicy};

    push_approval_policy(ToolApprovalPolicy {
        auto_deny: vec!["shell*".to_string()],
        ..Default::default()
    });
    let merged = merge_agent_loop_approval_policy(Some(ToolApprovalPolicy {
        auto_deny: vec!["fs_delete".to_string()],
        ..Default::default()
    }))
    .expect("policy present");
    pop_approval_policy();

    // Union of deny lists (more restrictive).
    assert!(merged.auto_deny.iter().any(|p| p == "shell*"));
    assert!(merged.auto_deny.iter().any(|p| p == "fs_delete"));
}

#[test]
fn merge_approval_policy_defers_when_only_one_side_present() {
    use crate::llm::agent_tools::merge_agent_loop_approval_policy;
    use crate::orchestration::{current_approval_policy, ToolApprovalPolicy};

    assert!(current_approval_policy().is_none());
    let merged = merge_agent_loop_approval_policy(Some(ToolApprovalPolicy {
        auto_approve: vec!["read*".to_string()],
        ..Default::default()
    }))
    .expect("policy present");
    assert_eq!(merged.auto_approve, vec!["read*".to_string()]);
}

#[test]
fn retry_after_missing() {
    let err = VmError::Runtime("rate limited".to_string());
    assert_eq!(extract_retry_after_ms(&err), None);
}

#[test]
fn retry_after_non_numeric() {
    let err = VmError::Runtime("retry-after: tomorrow".to_string());
    assert_eq!(extract_retry_after_ms(&err), None);
}

#[test]
fn retry_after_at_end_of_message() {
    let err = VmError::Runtime("retry-after: 3".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(3000));
}

#[test]
fn retry_after_fractional_seconds() {
    let err = VmError::Runtime("retry-after: 0.5".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(500));
}

#[test]
fn retry_after_non_string_error() {
    let err = VmError::Thrown(VmValue::Int(42));
    assert_eq!(extract_retry_after_ms(&err), None);
}

#[test]
fn retry_after_with_extra_whitespace() {
    let err = VmError::Runtime("retry-after:   7  ".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(7000));
}

#[test]
fn read_only_classification_follows_tool_kind_annotations() {
    // Tools declared with ACP Read|Search|Think|Fetch kinds are read-only;
    // Edit|Delete|Move|Execute|Other are not. The VM reads from the
    // active policy's tool_annotations registry — no hardcoded names.
    let mut registry = std::collections::BTreeMap::new();
    for (name, kind) in [
        ("read", ToolKind::Read),
        ("lookup", ToolKind::Read),
        ("search", ToolKind::Search),
        ("outline", ToolKind::Search),
        ("web_search", ToolKind::Search),
        ("web_fetch", ToolKind::Fetch),
        ("think", ToolKind::Think),
        ("write", ToolKind::Edit),
        ("edit", ToolKind::Edit),
        ("delete", ToolKind::Delete),
        ("exec", ToolKind::Execute),
        ("other", ToolKind::Other),
    ] {
        registry.insert(
            name.to_string(),
            ToolAnnotations {
                kind,
                ..Default::default()
            },
        );
    }
    let policy = crate::orchestration::CapabilityPolicy {
        tool_annotations: registry,
        ..Default::default()
    };
    push_execution_policy(policy);

    let is_ro = |name: &str| {
        crate::orchestration::current_tool_annotations(name)
            .map(|a| a.kind.is_read_only())
            .unwrap_or(false)
    };
    assert!(is_ro("read"));
    assert!(is_ro("lookup"));
    assert!(is_ro("search"));
    assert!(is_ro("outline"));
    assert!(is_ro("web_search"));
    assert!(is_ro("web_fetch"));
    assert!(is_ro("think"));
    assert!(!is_ro("write"));
    assert!(!is_ro("edit"));
    assert!(!is_ro("delete"));
    assert!(!is_ro("exec"));
    // Other is NOT read-only (fail-safe).
    assert!(!is_ro("other"));
    // Unannotated tools are NOT read-only.
    assert!(!is_ro("unknown_tool"));
    assert!(!is_ro(""));

    pop_execution_policy();
}

#[test]
fn stop_after_successful_tools_matches_successful_turn() {
    let stop_tools = vec!["edit".to_string(), "scaffold".to_string()];
    let tool_results = vec![
        json!({"tool_name": "read", "status": "ok"}),
        json!({"tool_name": "edit", "status": "ok"}),
    ];
    assert!(should_stop_after_successful_tools(
        &tool_results,
        &stop_tools
    ));
}

#[test]
fn stop_after_successful_tools_ignores_failed_or_unlisted_tools() {
    let stop_tools = vec!["edit".to_string()];
    let failed_results = vec![json!({"tool_name": "edit", "status": "error"})];
    assert!(!should_stop_after_successful_tools(
        &failed_results,
        &stop_tools
    ));

    let unrelated_results = vec![json!({"tool_name": "read", "status": "ok"})];
    assert!(!should_stop_after_successful_tools(
        &unrelated_results,
        &stop_tools
    ));
}

#[test]
fn has_successful_tools_matches_any_required_tool() {
    let required_tools = vec!["edit".to_string(), "create".to_string()];
    let tool_results = vec![
        json!({"tool_name": "lookup", "status": "ok"}),
        json!({"tool_name": "edit", "status": "ok"}),
    ];
    assert!(has_successful_tools(&tool_results, &required_tools));
}

#[test]
fn has_successful_tools_ignores_failed_turns() {
    let required_tools = vec!["edit".to_string()];
    let tool_results = vec![json!({"tool_name": "edit", "status": "error"})];
    assert!(!has_successful_tools(&tool_results, &required_tools));
}

#[test]
fn text_tool_format_drops_native_tool_channel() {
    let native_tools = vec![json!({
        "type": "function",
        "function": {
            "name": "edit",
            "description": "Edit a file",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }
    })];
    assert!(normalize_native_tools_for_format("text", Some(native_tools.clone())).is_none());
    assert!(normalize_native_tools_for_format("json", Some(native_tools)).is_none());
}

#[test]
fn build_llm_call_result_extracts_balanced_json_payloads() {
    let mut opts = base_opts(vec![json!({"role": "user", "content": "Summarize"})]);
    opts.response_format = Some("json".to_string());
    opts.output_schema = Some(json!({
        "type": "object",
        "properties": {
            "purpose": {"type": "string"}
        }
    }));

    let result = LlmResult {
        text: "Here is the result:\n{\"purpose\":\"cli\"}\nThanks.".to_string(),
        tool_calls: Vec::new(),
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        stop_reason: None,
        blocks: Vec::new(),
    };

    let vm_result = build_llm_call_result(&result, &opts);
    let dict = vm_result.as_dict().expect("dict");
    let data = dict.get("data").expect("parsed data");
    let data_dict = data.as_dict().expect("object data");
    assert_eq!(
        data_dict.get("purpose").map(VmValue::display).as_deref(),
        Some("cli")
    );
}

#[test]
fn build_llm_call_result_uses_output_schema_without_response_format_flag() {
    let mut opts = base_opts(vec![json!({"role": "user", "content": "Summarize"})]);
    opts.output_schema = Some(json!({
        "type": "object",
        "properties": {
            "frameworks": {
                "type": "array",
                "items": {"type": "string"}
            }
        }
    }));

    let result = LlmResult {
        text: "{\"frameworks\":[\"go test\"]}".to_string(),
        tool_calls: Vec::new(),
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        stop_reason: None,
        blocks: Vec::new(),
    };

    let vm_result = build_llm_call_result(&result, &opts);
    let dict = vm_result.as_dict().expect("dict");
    assert!(
        dict.get("data").is_some(),
        "structured output should populate data"
    );
}

#[test]
fn native_tool_format_preserves_native_tool_channel() {
    let native_tools = vec![json!({
        "type": "function",
        "function": {
            "name": "edit",
            "description": "Edit a file",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }
    })];
    let preserved = normalize_native_tools_for_format("native", Some(native_tools.clone()));
    assert_eq!(preserved, Some(native_tools));
}

#[test]
fn tool_examples_render_in_both_formats() {
    // Pre-v0.5.82 native mode dropped the text-mode examples to avoid two
    // protocols competing in the prompt. That breaks for hosts that strip
    // the native `tools` parameter (e.g. Ollama models with bare
    // `{{ .Prompt }}` chat templates) — the model ends up with no tool
    // guidance at all. Examples now flow through in both modes; the parser
    // accepts either channel.
    assert_eq!(
        normalize_tool_examples_for_format("native", Some(" edit({ path: \"a\" }) ".to_string())),
        Some("edit({ path: \"a\" })".to_string())
    );
    assert_eq!(
        normalize_tool_examples_for_format("text", Some(" edit({ path: \"a\" }) ".to_string())),
        Some("edit({ path: \"a\" })".to_string())
    );
    assert_eq!(
        normalize_tool_examples_for_format("native", Some("   ".to_string())),
        None
    );
    assert_eq!(normalize_tool_examples_for_format("native", None), None);
}

#[test]
fn native_action_stage_requires_tool_choice_when_missing() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: false,
        max_prose_chars: Some(120),
    };
    let native_tools = vec![json!({
        "type": "function",
        "function": {
            "name": "edit",
            "parameters": {"type": "object"}
        }
    })];
    let choice = normalize_tool_choice_for_format(
        "openrouter",
        "native",
        Some(&native_tools),
        None,
        Some(&policy),
    );
    assert_eq!(choice, Some(serde_json::json!("required")));
}

#[test]
fn native_action_stage_uses_provider_specific_tool_choice() {
    assert_eq!(
        required_tool_choice_for_provider("anthropic"),
        serde_json::json!({"type": "any"})
    );
}

#[test]
fn prose_budget_detection_and_trimming_work() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: true,
        max_prose_chars: Some(12),
    };
    assert!(prose_exceeds_budget(
        "This prose is definitely too long.",
        Some(&policy)
    ));
    let trimmed = trim_prose_for_history("This prose is definitely too long.", Some(&policy));
    assert!(trimmed.contains("assistant prose truncated by turn policy"));
}

#[test]
fn action_turn_nudge_mentions_action_or_yield() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: true,
        max_prose_chars: Some(120),
    };
    let msg = action_turn_nudge("text", Some(&policy), true).expect("nudge");
    assert!(
        msg.contains("either make concrete progress with a well-formed <tool_call> block, switch phase, or emit a <done> block")
    );
    assert!(msg.contains("120"));
    assert!(msg.contains("too much budget on prose"));
}

#[test]
fn sentinel_without_action_nudge_stays_stage_agnostic() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: true,
        max_prose_chars: Some(180),
    };
    let msg = sentinel_without_action_nudge("text", Some(&policy));
    assert!(msg.contains("without taking any tool action"));
    assert!(msg.contains("Make concrete progress with an available tool now, or switch phase"));
    assert!(!msg.contains("lookup() or read()"));
    assert!(msg.contains("Keep prose to at most 180 visible characters"));
}

#[test]
fn action_turn_nudge_omits_done_sentinel_when_stage_disallows_it() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: false,
        max_prose_chars: Some(90),
    };
    let msg = action_turn_nudge("text", Some(&policy), false).expect("nudge");
    assert!(!msg.contains("<done>"));
    assert!(msg.contains(
        "either make concrete progress with a well-formed <tool_call> block or switch phase"
    ));
}

#[test]
fn sentinel_without_action_nudge_explains_workflow_owned_stage_rule() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: false,
        max_prose_chars: Some(90),
    };
    let msg = sentinel_without_action_nudge("text", Some(&policy));
    assert!(msg.contains("workflow-owned action stage"));
    assert!(msg.contains("Do not output a <done> block in this stage"));
}

#[test]
fn native_action_turn_nudge_mentions_native_channel() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: false,
        max_prose_chars: Some(90),
    };
    let msg = action_turn_nudge("native", Some(&policy), false).expect("nudge");
    assert!(msg.contains("provider tool channel only"));
    assert!(msg.contains("handwritten tool-call text is invalid"));
}

#[tokio::test(flavor = "current_thread")]
async fn persistent_prompt_omits_done_sentinel_when_stage_disallows_it() {
    reset_llm_mock_state();
    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "repair the attached file",
    })]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.turn_policy = Some(TurnPolicy {
        require_action_or_yield: false,
        allow_done_sentinel: false,
        max_prose_chars: None,
    });

    let _ = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    let system = calls
        .last()
        .and_then(|call| call.system.as_ref())
        .expect("mock call system prompt");
    assert!(system.contains("take action with tool calls"));
    assert!(!system.contains("##DONE##"));
    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::await_holding_lock)]
async fn observed_llm_call_transcript_deduplicates_system_and_tool_schemas() {
    // Two back-to-back calls with identical system prompt and tool schema
    // list should emit `system_prompt` and `tool_schemas` events exactly
    // once each, while `provider_call_request` fires on every call. The
    // dedup state is per-agent-loop; for standalone `observed_llm_call`
    // tests we rely on the thread-local seeded in the first dump.
    //
    // Guard against parallel tests racing on the shared HARN_LLM_TRANSCRIPT_DIR
    // env var — other tests in this module set/unset the same variable.
    let _guard = transcript_env_lock();
    reset_llm_mock_state();
    let dir = std::env::temp_dir().join(format!(
        "harn-llm-transcript-dedup-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let old_dir = std::env::var("HARN_LLM_TRANSCRIPT_DIR").ok();
    std::env::set_var(
        "HARN_LLM_TRANSCRIPT_DIR",
        dir.to_string_lossy().into_owned(),
    );
    super::super::agent_observe::reset_transcript_dedup();

    let mut opts = base_opts(vec![serde_json::json!({"role": "user", "content": "ping"})]);
    opts.system = Some("static system prompt".to_string());

    for iteration in 0..3 {
        let _ = observed_llm_call(
            &opts,
            Some("text"),
            None,
            &LlmRetryConfig::default(),
            Some(iteration),
            false,
            false,
        )
        .await
        .unwrap();
    }

    // Other parallel tests in this binary may briefly point
    // HARN_LLM_TRANSCRIPT_DIR at the same temp dir via the shared env var,
    // so our file can pick up stray events. Filter on our marker system
    // prompt before asserting counts.
    let transcript =
        std::fs::read_to_string(dir.join("llm_transcript.jsonl")).expect("transcript file");
    let system_events_for_us = transcript
        .lines()
        .filter(|l| {
            l.contains("\"type\":\"system_prompt\"") && l.contains("\"static system prompt\"")
        })
        .count();
    let schema_events_for_us = transcript
        .lines()
        .filter(|l| l.contains("\"type\":\"tool_schemas\"") && l.contains("\"schemas\":[]"))
        .count();
    let request_events = transcript
        .lines()
        .filter(|l| l.contains("\"type\":\"provider_call_request\""))
        .count();
    assert_eq!(
        system_events_for_us, 1,
        "our system prompt should be emitted once; transcript:\n{transcript}"
    );
    assert!(
        schema_events_for_us >= 1,
        "empty tool schemas should be emitted at least once; transcript:\n{transcript}"
    );
    assert!(
        request_events >= 3,
        "provider_call_request fires per call; transcript:\n{transcript}"
    );
    assert!(
        !transcript.contains("\"messages\":[{"),
        "provider_call_request must not embed the message list; messages are emitted as their own events: {transcript}",
    );

    if let Some(previous) = old_dir {
        std::env::set_var("HARN_LLM_TRANSCRIPT_DIR", previous);
    } else {
        std::env::remove_var("HARN_LLM_TRANSCRIPT_DIR");
    }
    let _ = std::fs::remove_dir_all(dir);
    reset_llm_mock_state();
}

/// Mutex protecting the HARN_LLM_TRANSCRIPT_DIR env var so transcript
/// tests in this module don't race each other and end up writing to a
/// neighbour's temp dir.
fn transcript_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::await_holding_lock)]
async fn observed_llm_call_transcript_uses_explicit_tool_format() {
    let _guard = transcript_env_lock();
    reset_llm_mock_state();
    let dir = std::env::temp_dir().join(format!("harn-llm-transcript-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let old_dir = std::env::var("HARN_LLM_TRANSCRIPT_DIR").ok();
    std::env::set_var(
        "HARN_LLM_TRANSCRIPT_DIR",
        dir.to_string_lossy().into_owned(),
    );

    let opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "perform one grounded edit",
    })]);
    let _ = observed_llm_call(
        &opts,
        Some("native"),
        None,
        &LlmRetryConfig::default(),
        Some(0),
        false,
        false,
    )
    .await
    .unwrap();

    let transcript =
        std::fs::read_to_string(dir.join("llm_transcript.jsonl")).expect("transcript file");
    assert!(transcript.contains("\"tool_format\":\"native\""));

    if let Some(previous) = old_dir {
        std::env::set_var("HARN_LLM_TRANSCRIPT_DIR", previous);
    } else {
        std::env::remove_var("HARN_LLM_TRANSCRIPT_DIR");
    }
    let _ = std::fs::remove_dir_all(dir);
    reset_llm_mock_state();
}

async fn assert_observed_llm_call_bridge_user_visible(user_visible: bool) {
    reset_llm_mock_state();

    let bridge = Rc::new(HostBridge::from_parts(
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(())),
        1,
    ));
    let opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "stream a visible reply",
    })]);

    let expected_text = tokio::task::LocalSet::new()
        .run_until(async {
            let result = observed_llm_call(
                &opts,
                None,
                Some(&bridge),
                &LlmRetryConfig::default(),
                None,
                user_visible,
                false,
            )
            .await
            .unwrap();
            for _ in 0..10 {
                if bridge.recorded_notifications().len() >= 3 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            result.text
        })
        .await;

    let notifications = bridge.recorded_notifications();
    let session_updates: Vec<_> = notifications
        .iter()
        .filter(|notification| notification["method"] == "session/update")
        .collect();
    assert_eq!(session_updates.len(), 3);

    let call_start = session_updates
        .iter()
        .find(|notification| notification["params"]["update"]["sessionUpdate"] == "call_start")
        .expect("call_start notification")["params"]["update"]["content"]
        .clone();
    let call_id = call_start["toolCallId"]
        .as_str()
        .expect("call_start toolCallId");
    assert_eq!(call_start["metadata"]["user_visible"], json!(user_visible));

    let call_progress = session_updates
        .iter()
        .find(|notification| notification["params"]["update"]["sessionUpdate"] == "call_progress")
        .expect("call_progress notification")["params"]["update"]["content"]
        .clone();
    assert_eq!(call_progress["toolCallId"].as_str(), Some(call_id));
    // `delta` carries the raw provider bytes for observability, including
    // any tagged-protocol wrappers. `visible_text` goes through the
    // user-facing sanitizer that unwraps <assistant_prose> and hides
    // <tool_call> / <done> blocks. Check each against its own contract.
    assert_eq!(
        call_progress["delta"].as_str(),
        Some(expected_text.as_str())
    );
    let visible_expected =
        crate::visible_text::sanitize_visible_assistant_text(&expected_text, false);
    assert_eq!(
        call_progress["visible_text"].as_str(),
        Some(visible_expected.as_str())
    );
    assert_eq!(call_progress["user_visible"], json!(user_visible));

    let call_end = session_updates
        .iter()
        .find(|notification| notification["params"]["update"]["sessionUpdate"] == "call_end")
        .expect("call_end notification")["params"]["update"]["content"]
        .clone();
    assert_eq!(call_end["toolCallId"].as_str(), Some(call_id));
    assert_eq!(call_end["metadata"]["user_visible"], json!(user_visible));

    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn observed_llm_call_bridge_events_include_user_visible() {
    assert_observed_llm_call_bridge_user_visible(true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn observed_llm_call_bridge_events_include_non_user_visible() {
    assert_observed_llm_call_bridge_user_visible(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn daemon_timer_wake_persists_snapshot_and_compacts_on_idle() {
    let dir = std::env::temp_dir().join(format!("harn-agent-daemon-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let snapshot_path = dir.join("daemon.json");
    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "poll background state",
    })]);
    let mut config = base_agent_config();
    config.max_iterations = 2;
    config.daemon = true;
    config.daemon_config = DaemonLoopConfig {
        persist_path: Some(snapshot_path.to_string_lossy().into_owned()),
        wake_interval_ms: Some(1),
        consolidate_on_idle: true,
        ..Default::default()
    };
    config.auto_compact = Some(crate::orchestration::AutoCompactConfig {
        token_threshold: 1,
        keep_last: 1,
        ..Default::default()
    });

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    // The daemon exhausted max_iterations (2) without any natural
    // terminal condition firing, so the loop now reports
    // budget_exhausted rather than the ambiguous "idle" it used to.
    assert_eq!(result["status"], "budget_exhausted");
    assert_eq!(result["daemon_state"], "budget_exhausted");
    assert_eq!(result["iterations"].as_u64(), Some(2));
    assert_eq!(
        result["daemon_snapshot_path"].as_str(),
        Some(snapshot_path.to_str().unwrap())
    );

    let snapshot = super::super::daemon::load_snapshot(snapshot_path.to_str().unwrap()).unwrap();
    assert_eq!(snapshot.daemon_state, "budget_exhausted");
    assert_eq!(snapshot.total_iterations, 2);
    assert!(snapshot.transcript_summary.is_some());

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "current_thread")]
async fn require_successful_tools_marks_loop_failed_when_no_write_succeeds() {
    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "make a deterministic write",
    })]);
    let mut config = base_agent_config();
    config.require_successful_tools = Some(vec!["edit".to_string()]);

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(result["successful_tools"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn daemon_resume_path_restores_prior_session_state() {
    let dir = std::env::temp_dir().join(format!("harn-agent-resume-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let snapshot_path = dir.join("daemon.json");
    let seeded_snapshot = DaemonSnapshot {
        daemon_state: "idle".to_string(),
        visible_messages: vec![serde_json::json!({
            "role": "user",
            "content": "resume the daemon",
        })],
        recorded_messages: vec![serde_json::json!({
            "role": "user",
            "content": "resume the daemon",
        })],
        total_text: "prior transcript\n".to_string(),
        total_iterations: 5,
        idle_backoff_ms: 250,
        ..Default::default()
    };
    persist_snapshot(snapshot_path.to_str().unwrap(), &seeded_snapshot).unwrap();

    let mut opts = base_opts(Vec::new());
    let mut config = base_agent_config();
    config.daemon = true;
    config.daemon_config = DaemonLoopConfig {
        resume_path: Some(snapshot_path.to_string_lossy().into_owned()),
        ..Default::default()
    };

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_eq!(result["status"], "idle");
    assert_eq!(
        result["daemon_snapshot_path"].as_str(),
        Some(snapshot_path.to_str().unwrap())
    );
    assert_eq!(result["iterations"].as_u64(), Some(6));
    assert!(result["text"]
        .as_str()
        .unwrap_or("")
        .starts_with("prior transcript"));

    let _ = std::fs::remove_dir_all(dir);
}

// ── Event substrate tests ──────────────────────────────────────────
// These cover the WS-1 contract (ToolAnnotations + AgentEvent +
// agent_subscribe + agent_inject_feedback + clear_session_sinks)
// with minimal harness — no LLM call, no tokio runtime dance — just
// exercise the thread-local registries and drain/cleanup helpers.

#[test]
fn pending_feedback_drain_filters_by_session_and_preserves_order() {
    use crate::llm::agent::{drain_pending_feedback, push_pending_feedback};

    // Push items into two sessions interleaved. Drain one; the other
    // must survive untouched with its original ordering.
    push_pending_feedback("sess_a", "post_turn", "a-first");
    push_pending_feedback("sess_b", "post_turn", "b-only");
    push_pending_feedback("sess_a", "grounding_violation", "a-second");

    let drained_a = drain_pending_feedback("sess_a");
    assert_eq!(
        drained_a,
        vec![
            ("post_turn".to_string(), "a-first".to_string()),
            ("grounding_violation".to_string(), "a-second".to_string()),
        ],
        "drain must return session-matched entries in push order"
    );

    let remaining_b = drain_pending_feedback("sess_b");
    assert_eq!(
        remaining_b,
        vec![("post_turn".to_string(), "b-only".to_string())],
        "unrelated session's queue must survive the first drain"
    );

    // Draining again yields nothing — queue is empty now.
    assert!(drain_pending_feedback("sess_a").is_empty());
    assert!(drain_pending_feedback("sess_b").is_empty());
}

#[test]
fn session_sink_registry_round_trip_and_cleanup() {
    use crate::agent_events::{
        clear_session_sinks, emit_event, register_sink, reset_all_sinks,
        session_external_sink_count, AgentEvent, AgentEventSink,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counter(Arc<AtomicUsize>);
    impl AgentEventSink for Counter {
        fn handle_event(&self, _event: &AgentEvent) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    reset_all_sinks();
    let hits = Arc::new(AtomicUsize::new(0));
    register_sink("sink-lifecycle", Arc::new(Counter(hits.clone())));
    assert_eq!(session_external_sink_count("sink-lifecycle"), 1);

    emit_event(&AgentEvent::TurnStart {
        session_id: "sink-lifecycle".into(),
        iteration: 0,
    });
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    clear_session_sinks("sink-lifecycle");
    assert_eq!(session_external_sink_count("sink-lifecycle"), 0);

    // A post-clear emit must NOT re-invoke the stale counter.
    emit_event(&AgentEvent::TurnEnd {
        session_id: "sink-lifecycle".into(),
        iteration: 0,
        turn_info: json!({}),
    });
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    reset_all_sinks();
}

#[test]
fn tool_kind_is_read_only_excludes_other() {
    // Regression for invariant #5 of the ACP refactor: ToolKind::Other
    // must NOT auto-classify as read-only. Unannotated tools stay out
    // of the concurrent-dispatch fast path by design.
    let annotations = ToolAnnotations {
        kind: ToolKind::Other,
        ..Default::default()
    };
    assert!(!annotations.kind.is_read_only());
    for kind in [
        ToolKind::Read,
        ToolKind::Search,
        ToolKind::Think,
        ToolKind::Fetch,
    ] {
        assert!(kind.is_read_only(), "{:?} must be read-only", kind);
    }
    for kind in [
        ToolKind::Edit,
        ToolKind::Delete,
        ToolKind::Move,
        ToolKind::Execute,
    ] {
        assert!(
            !kind.is_read_only(),
            "{:?} must NOT be read-only (has side effect)",
            kind
        );
    }
}

#[test]
fn workflow_stage_extracts_session_id_from_raw_model_policy() {
    // orchestration/workflow.rs reads session_id from the caller's
    // model_policy dict. A workflow-stage builder minting its own id
    // (via `burin_new_session_id`) must thread through unchanged —
    // otherwise the pipeline-side agent_subscribe handlers attach to
    // an id the VM never uses.
    let mut dict: std::collections::BTreeMap<String, VmValue> = Default::default();
    dict.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from("implement_abc123")),
    );
    let raw_model_policy = VmValue::Dict(Rc::new(dict));

    let extracted = raw_model_policy
        .as_dict()
        .and_then(|d| d.get("session_id"))
        .and_then(|v| match v {
            VmValue::String(s) if !s.trim().is_empty() => Some(s.to_string()),
            _ => None,
        });
    assert_eq!(extracted.as_deref(), Some("implement_abc123"));

    // Nil / blank / wrong-type values must fall through to None so
    // the workflow executor mints a fresh id.
    for bad in [
        VmValue::Nil,
        VmValue::String(Rc::from("")),
        VmValue::String(Rc::from("   ")),
        VmValue::Int(42),
    ] {
        let mut d: std::collections::BTreeMap<String, VmValue> = Default::default();
        d.insert("session_id".to_string(), bad.clone());
        let probe = VmValue::Dict(Rc::new(d));
        let got = probe
            .as_dict()
            .and_then(|dd| dd.get("session_id"))
            .and_then(|v| match v {
                VmValue::String(s) if !s.trim().is_empty() => Some(s.to_string()),
                _ => None,
            });
        assert_eq!(got, None, "value {:?} must not extract as session_id", bad);
    }
}
