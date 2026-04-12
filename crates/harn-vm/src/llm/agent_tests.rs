use super::{
    action_turn_nudge, compact_malformed_assistant_turn, extract_retry_after_ms,
    has_successful_tools, is_read_only_tool, loop_state_requests_phase_change,
    normalize_native_tools_for_format, normalize_tool_choice_for_format,
    normalize_tool_examples_for_format, observed_llm_call, prose_exceeds_budget,
    required_tool_choice_for_provider, run_agent_loop_internal, sentinel_without_action_nudge,
    should_stop_after_successful_tools, trim_prose_for_history, AgentLoopConfig, LlmRetryConfig,
    build_llm_call_result,
};
use crate::llm::api::{LlmCallOptions, LlmResult};
use crate::llm::daemon::{persist_snapshot, DaemonLoopConfig, DaemonSnapshot};
use crate::llm::mock::{get_llm_mock_calls, reset_llm_mock_state};
use crate::orchestration::TurnPolicy;
use crate::value::{VmError, VmValue};
use serde_json::json;
use std::rc::Rc;

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
        context_callback: None,
        policy: None,
        daemon: false,
        daemon_config: DaemonLoopConfig::default(),
        llm_retries: 0,
        llm_backoff_ms: 1,
        exit_when_verified: false,
        loop_detect_warn: 0,
        loop_detect_block: 0,
        loop_detect_skip: 0,
        tool_examples: None,
        post_turn_callback: None,
        turn_policy: None,
        stop_after_successful_tools: None,
        require_successful_tools: None,
        on_tool_call: None,
        on_tool_result: None,
    }
}

#[test]
fn detects_phase_change_from_latest_loop_state_footer() {
    let text = "First\n\n## LOOP_STATE\nphase: assess\nnext_phase: ground\n## END_LOOP_STATE\n\nSecond\n\n## LOOP_STATE\nphase: ground\nnext_phase: execute\n## END_LOOP_STATE";
    assert!(loop_state_requests_phase_change(text, "ground"));
    assert!(!loop_state_requests_phase_change(text, "execute"));
}

#[test]
fn compact_malformed_assistant_turn_elides_raw_text() {
    let msg = compact_malformed_assistant_turn(1);
    assert!(msg.contains("1 malformed tool call"));
    assert!(msg.contains("elided"));
    assert!(!msg.contains("```call"));
    assert!(!msg.contains("<<'EOF'"));

    let msg_plural = compact_malformed_assistant_turn(3);
    assert!(msg_plural.contains("3 malformed tool calls"));
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
fn read_only_tools_recognized() {
    assert!(is_read_only_tool("read"));
    assert!(is_read_only_tool("read_file"));
    assert!(is_read_only_tool("lookup"));
    assert!(is_read_only_tool("search"));
    assert!(is_read_only_tool("outline"));
    assert!(is_read_only_tool("list_directory"));
    assert!(is_read_only_tool("web_search"));
    assert!(is_read_only_tool("web_fetch"));
}

#[test]
fn write_tools_not_read_only() {
    assert!(!is_read_only_tool("write"));
    assert!(!is_read_only_tool("edit"));
    assert!(!is_read_only_tool("delete"));
    assert!(!is_read_only_tool("exec"));
    assert!(!is_read_only_tool(""));
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
    assert!(dict.get("data").is_some(), "structured output should populate data");
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
fn native_format_drops_text_tool_examples() {
    assert_eq!(
        normalize_tool_examples_for_format("native", Some("edit({ path: \"a\" })".to_string())),
        None
    );
    assert_eq!(
        normalize_tool_examples_for_format("text", Some(" edit({ path: \"a\" }) ".to_string())),
        Some("edit({ path: \"a\" })".to_string())
    );
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
        msg.contains("either call at least one tool, switch phase, or output the done sentinel")
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
    assert!(msg.contains("Use an available tool now, or switch phase"));
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
    assert!(!msg.contains("done sentinel"));
    assert!(msg.contains("either call at least one tool or switch phase"));
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
    assert!(msg.contains("Do not output a done sentinel in this stage"));
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
    assert!(system.contains("take action with tools"));
    assert!(!system.contains("##DONE##"));
    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn observed_llm_call_transcript_uses_explicit_tool_format() {
    reset_llm_mock_state();
    let dir = std::env::temp_dir().join(format!("harn-llm-transcript-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let old_dir = std::env::var("HARN_LLM_TRANSCRIPT_DIR").ok();
    std::env::set_var("HARN_LLM_TRANSCRIPT_DIR", dir.to_string_lossy().to_string());

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
        persist_path: Some(snapshot_path.to_string_lossy().to_string()),
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
    assert_eq!(result["status"], "idle");
    assert_eq!(result["daemon_state"], "idle");
    assert_eq!(result["iterations"].as_u64(), Some(2));
    assert_eq!(
        result["daemon_snapshot_path"].as_str(),
        Some(snapshot_path.to_str().unwrap())
    );

    let snapshot = super::super::daemon::load_snapshot(snapshot_path.to_str().unwrap()).unwrap();
    assert_eq!(snapshot.daemon_state, "idle");
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
        resume_path: Some(snapshot_path.to_string_lossy().to_string()),
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
