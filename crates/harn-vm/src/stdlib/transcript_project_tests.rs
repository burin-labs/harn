use super::*;

fn json_dict_to_btree(value: &JsonValue) -> crate::value::DictMap {
    match json_to_vm_value(value) {
        VmValue::Dict(d) => (*d).clone(),
        _ => crate::value::DictMap::new(),
    }
}

fn transcript_with(messages: Vec<JsonValue>, summary: Option<&str>) -> crate::value::DictMap {
    let mut transcript = serde_json::json!({
        "_type": "transcript",
        "version": 2,
        "messages": messages,
        "events": [],
        "assets": [],
    });
    if let Some(text) = summary {
        transcript["summary"] = serde_json::Value::String(text.to_string());
    }
    json_dict_to_btree(&transcript)
}

#[tokio::test]
async fn raw_policy_is_identity_and_emits_hash() {
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({"role": "assistant", "content": "hey"}),
        ],
        None,
    );
    let policy = ProjectionPolicy::default_for(PolicyKind::Raw);
    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.dropped_indices.len(), 0);
    assert!(result.prefix_hash.starts_with("sha256:"));
}

#[tokio::test]
async fn clean_tool_repair_drops_failed_then_success_pair() {
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "run it"}),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "call_1", "name": "run", "arguments": {}}],
            }),
            serde_json::json!({
                "role": "tool_result",
                "tool_call_id": "call_1",
                "name": "run",
                "content": "Error: missing arg",
                "is_error": true,
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "call_2", "name": "run", "arguments": {"arg": "x"}}],
            }),
            serde_json::json!({
                "role": "tool_result",
                "tool_call_id": "call_2",
                "name": "run",
                "content": "ok",
            }),
        ],
        None,
    );
    let policy = ProjectionPolicy::default_for(PolicyKind::CleanToolRepair);
    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();
    // Drops the failed assistant (index 1) and its error tool result (index 2).
    assert_eq!(result.dropped_indices, vec![1, 2]);
    assert_eq!(result.kept_indices, vec![0, 3, 4]);
    assert_eq!(result.messages.len(), 3);
    assert!(result
        .messages
        .last()
        .and_then(|m| m.get("content"))
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .contains("ok"));
}

#[tokio::test]
async fn squash_failed_calls_drops_orphan_failures() {
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "go"}),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "a", "name": "lookup", "arguments": {}}],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "a",
                "name": "lookup",
                "content": "Error: not found",
                "is_error": true,
            }),
            serde_json::json!({"role": "assistant", "content": "Done after all."}),
        ],
        None,
    );
    let policy = ProjectionPolicy::default_for(PolicyKind::SquashFailedCalls);
    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();
    assert_eq!(result.dropped_indices, vec![1, 2]);
    assert_eq!(result.messages.len(), 2);
}

#[tokio::test]
async fn summary_prefix_replaces_old_history_with_synthetic_message() {
    let transcript = transcript_with(
        (0..6)
            .map(|i| {
                serde_json::json!({
                    "role": if i % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("msg{}", i),
                })
            })
            .collect(),
        Some("Earlier work boiled down."),
    );
    let mut policy = ProjectionPolicy::default_for(PolicyKind::SummaryPrefix);
    policy.summary_keep_last = 2;
    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();
    assert_eq!(result.messages.len(), 3); // 1 summary + 2 kept
    assert_eq!(result.dropped_indices.len(), 4);
    assert_eq!(result.kept_indices, vec![4, 5]);
    let summary_msg = &result.messages[0];
    assert_eq!(
        summary_msg.get("role").and_then(JsonValue::as_str),
        Some("system")
    );
    assert_eq!(
        summary_msg.get("content").and_then(JsonValue::as_str),
        Some("Earlier work boiled down.")
    );
    assert!(summary_msg
        .get("_harn_projection")
        .and_then(|p| p.get("synthetic"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false));
}

#[tokio::test]
async fn provider_safety_blocks_dropping_signed_reasoning() {
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "first"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "private chain", "signature": "abc123"},
                    {"type": "tool_use", "id": "call_x", "name": "run", "input": {}}
                ],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call_x",
                "name": "run",
                "content": "Error: boom",
                "is_error": true,
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "call_y", "name": "run", "arguments": {"v": 1}}],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call_y",
                "name": "run",
                "content": "ok",
            }),
        ],
        None,
    );
    let policy = ProjectionPolicy::default_for(PolicyKind::CleanToolRepair);
    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();
    assert!(result.provider_safety_blocked);
    // Falls back to identity: all messages survive.
    assert_eq!(result.kept_indices.len(), 5);
    assert_eq!(result.dropped_indices.len(), 0);
}

#[tokio::test]
async fn provider_safety_can_be_disabled_for_local_only_previews() {
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "first"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "private chain", "signature": "abc"},
                    {"type": "tool_use", "id": "c", "name": "run", "input": {}}
                ],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "c",
                "name": "run",
                "content": "Error",
                "is_error": true,
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "c2", "name": "run", "arguments": {}}],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "c2",
                "name": "run",
                "content": "ok",
            }),
        ],
        None,
    );
    let mut policy = ProjectionPolicy::default_for(PolicyKind::CleanToolRepair);
    policy.respect_provider_signatures = false;
    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();
    assert!(!result.provider_safety_blocked);
    assert!(!result.dropped_indices.is_empty());
}

#[tokio::test]
async fn reachability_gc_redacts_only_unrooted_stale_tool_results() {
    let stale_body = format!("src/old.rs\n{}", "old line\n".repeat(180));
    let rooted_body = format!(
        "src/live.rs\nIMPORTANT_LIVE_VALUE\n{}",
        "live line\n".repeat(180)
    );
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "inspect old and live files"}),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "old_call", "name": "read", "arguments": {"path": "src/old.rs"}}],
            }),
            serde_json::json!({
                "role": "tool_result",
                "tool_call_id": "old_call",
                "name": "read",
                "content": stale_body,
                "_harn": {
                    "kind": "tool_result",
                    "outcome": "ok",
                    "schema": "agent.tool_result.v1",
                },
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "live_call", "name": "read", "arguments": {"path": "src/live.rs"}}],
            }),
            serde_json::json!({
                "role": "tool_result",
                "tool_call_id": "live_call",
                "name": "read",
                "content": rooted_body,
                "_harn": {
                    "kind": "tool_result",
                    "outcome": "ok",
                    "schema": "agent.tool_result.v1",
                },
            }),
            serde_json::json!({"role": "assistant", "content": "The active change is in src/live.rs."}),
            serde_json::json!({"role": "user", "content": "Continue from the live file finding."}),
        ],
        None,
    );
    let mut policy = ProjectionPolicy::default_for(PolicyKind::ReachabilityGc);
    policy.gc_root_window = 2;
    policy.gc_min_chars = 100;

    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();

    assert_eq!(result.dropped_indices.len(), 0);
    assert_eq!(result.redacted_indices, vec![2]);
    assert!(result.reclaimed_tokens > 0);
    assert!(result
        .roots_consulted
        .contains(&"last_2_messages".to_string()));
    assert_eq!(
        result.redaction_pointers[0]["source"],
        "transcript.messages[2].content"
    );
    assert!(result.messages[2]["content"]
        .as_str()
        .unwrap_or_default()
        .contains("reclaimed by reachability_gc"));
    assert!(result.messages[4]["content"]
        .as_str()
        .unwrap_or_default()
        .contains("IMPORTANT_LIVE_VALUE"));
    assert!(result.messages[2]["_harn_projection"]["redacted"]
        .as_bool()
        .unwrap_or(false));
}

#[tokio::test]
async fn reachability_gc_keeps_domain_kind_identifiers_outside_harn_metadata() {
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "Inspect the stored record."}),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "record_call", "name": "lookup", "arguments": {}}],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "record_call",
                "name": "lookup",
                "content": "record payload\n".repeat(180),
                "data": {"kind": "CustomerRecord"},
                "_harn": {
                    "kind": "tool_result",
                    "outcome": "ok",
                    "schema": "agent.tool_result.v1",
                },
            }),
            serde_json::json!({"role": "assistant", "content": "CustomerRecord is the active domain object."}),
            serde_json::json!({"role": "user", "content": "Continue with CustomerRecord."}),
        ],
        None,
    );
    let mut policy = ProjectionPolicy::default_for(PolicyKind::ReachabilityGc);
    policy.gc_root_window = 2;
    policy.gc_min_chars = 100;

    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();

    assert!(result.redacted_indices.is_empty());
    assert!(result.messages[2]["content"]
        .as_str()
        .unwrap_or_default()
        .contains("record payload"));
}

#[tokio::test]
async fn reachability_gc_preserves_structured_ids_without_spelling_heuristics() {
    let uuid = "8f14e45f-ea5e-4dab-8c71-6f589afb8e17";
    let sha = "deadbeef1234567890abcdef1234567890abcdef";
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "Inspect the selected objects."}),
            serde_json::json!({
                "role": "tool",
                "name": "lookup",
                "content": "lowercase object payload\n".to_string() + &"value\n".repeat(180),
                "data": {"object_id": "widget"},
            }),
            serde_json::json!({
                "role": "tool",
                "name": "lookup",
                "content": "UUID payload\n".to_string() + &"value\n".repeat(180),
                "data": {"record_id": uuid},
            }),
            serde_json::json!({
                "role": "tool",
                "name": "lookup",
                "content": "commit payload\n".to_string() + &"value\n".repeat(180),
                "data": {"commit_sha": sha},
            }),
            serde_json::json!({
                "role": "tool",
                "name": "lookup",
                "content": "issue payload\n".to_string() + &"value\n".repeat(180),
                "data": {"issue_number": 12},
            }),
            serde_json::json!({
                "role": "user",
                "content": format!("Continue with widget, {uuid}, {sha}, and issue 12."),
            }),
        ],
        None,
    );
    let mut policy = ProjectionPolicy::default_for(PolicyKind::ReachabilityGc);
    policy.gc_root_window = 1;
    policy.gc_min_chars = 100;

    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();

    assert!(result.redacted_indices.is_empty());
}

#[tokio::test]
async fn reachability_gc_matches_references_as_tokens_not_substrings() {
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "Inspect two old objects."}),
            serde_json::json!({
                "role": "tool",
                "name": "lookup",
                "content": "old issue payload\n".to_string() + &"value\n".repeat(180),
                "data": {"issue_number": 2},
            }),
            serde_json::json!({
                "role": "tool",
                "name": "lookup",
                "content": "old commit payload\n".to_string() + &"value\n".repeat(180),
                "data": {"commit_sha": "abc1234"},
            }),
            serde_json::json!({
                "role": "user",
                "content": "Continue with issue 12 and xabc1234y.",
            }),
        ],
        None,
    );
    let mut policy = ProjectionPolicy::default_for(PolicyKind::ReachabilityGc);
    policy.gc_root_window = 1;
    policy.gc_min_chars = 100;

    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();

    assert_eq!(result.redacted_indices, vec![1, 2]);
}

#[tokio::test]
async fn reachability_gc_redacts_only_selected_tool_result_blocks() {
    let stale_body = format!("src/old.rs\n{}", "old line\n".repeat(180));
    let rooted_body = format!(
        "src/live.rs\nIMPORTANT_LIVE_VALUE\n{}",
        "live line\n".repeat(180)
    );
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "inspect old and live files"}),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "old_call", "name": "read", "input": {"path": "src/old.rs"}},
                    {"type": "tool_use", "id": "live_call", "name": "read", "input": {"path": "src/live.rs"}}
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "old_call", "content": stale_body},
                    {"type": "tool_result", "tool_use_id": "live_call", "content": rooted_body}
                ],
            }),
            serde_json::json!({"role": "assistant", "content": "The active change is in src/live.rs."}),
            serde_json::json!({"role": "user", "content": "Continue from the live file finding."}),
        ],
        None,
    );
    let mut policy = ProjectionPolicy::default_for(PolicyKind::ReachabilityGc);
    policy.gc_root_window = 2;
    policy.gc_min_chars = 100;

    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();

    assert_eq!(result.redacted_indices, vec![2]);
    assert_eq!(result.redaction_pointers.len(), 1);
    assert_eq!(
        result.redaction_pointers[0]["source"],
        "transcript.messages[2].content[0].content"
    );
    let blocks = result.messages[2]["content"].as_array().unwrap();
    assert!(blocks[0]["content"]
        .as_str()
        .unwrap_or_default()
        .contains("reclaimed by reachability_gc"));
    assert!(blocks[1]["content"]
        .as_str()
        .unwrap_or_default()
        .contains("IMPORTANT_LIVE_VALUE"));
    assert_eq!(
        result.messages[2]["_harn_projection"]["redaction_pointers"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
}

#[tokio::test]
async fn reachability_gc_honors_required_write_barrier() {
    let transcript = transcript_with(
        vec![
            serde_json::json!({"role": "user", "content": "read stale"}),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "call", "name": "read", "arguments": {"path": "src/stale.rs"}}],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call",
                "name": "read",
                "content": "src/stale.rs\n".to_string() + &"stale line\n".repeat(180),
            }),
            serde_json::json!({"role": "user", "content": "new task"}),
        ],
        None,
    );
    let mut policy = ProjectionPolicy::default_for(PolicyKind::ReachabilityGc);
    policy.gc_root_window = 1;
    policy.gc_min_chars = 100;
    policy.gc_require_write_barrier = true;

    let result = project_transcript(None, &transcript, &policy)
        .await
        .unwrap();

    assert!(result.redacted_indices.is_empty());
    assert_eq!(result.reason, "reachability_gc_write_barrier_missing");
    assert!(result
        .roots_consulted
        .contains(&"write_barrier_required".to_string()));
}

#[tokio::test]
async fn hash_changes_when_messages_change() {
    let raw_msgs = vec![
        serde_json::json!({"role": "user", "content": "hi"}),
        serde_json::json!({"role": "assistant", "content": "hey"}),
    ];
    let h1 = hash_messages(&raw_msgs);
    let h2 = hash_messages(&[
        serde_json::json!({"role": "user", "content": "hi"}),
        serde_json::json!({"role": "assistant", "content": "different"}),
    ]);
    assert_ne!(h1, h2);
    let h3 = hash_messages(&raw_msgs);
    assert_eq!(h1, h3);
}

#[test]
fn parse_policy_accepts_string_shorthand() {
    let policy =
        parse_projection_options(&VmValue::String(arcstr::ArcStr::from("clean_tool_repair")))
            .unwrap();
    assert_eq!(policy.kind, PolicyKind::CleanToolRepair);
}

#[test]
fn parse_policy_rejects_unknown_kind() {
    let err =
        parse_projection_options(&VmValue::String(arcstr::ArcStr::from("bogus"))).unwrap_err();
    match err {
        VmError::Runtime(msg) => assert!(msg.contains("bogus")),
        _ => panic!("expected runtime error"),
    }
}
