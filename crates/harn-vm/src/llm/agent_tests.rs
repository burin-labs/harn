use super::{
    compact_malformed_assistant_turn, extract_retry_after_ms, is_read_only_tool,
    loop_state_requests_phase_change, run_agent_loop_internal, AgentLoopConfig,
};
use crate::llm::api::LlmCallOptions;
use crate::llm::daemon::{persist_snapshot, DaemonLoopConfig, DaemonSnapshot};
use crate::value::{VmError, VmValue};
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
