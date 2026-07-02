use serde_json::json;

use crate::agent_events::AgentEvent;

use super::{
    agent_turn_made_no_llm_call, assistant_message_from_llm_result, build_agent_event,
    canonical_acp_stop_reason, canonical_provider_stop_reason, dict_get, initial_user_content,
    is_length_truncation, last_assistant_text, list_items, pair_orphaned_tool_use,
    reset_agent_session_host_state, seed_host_session_provider_model,
    synthesize_orphan_tool_results, text_has_tool_call_prefix, tool_result_message_for_provider,
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
fn agent_emit_loop_stuck_preserves_pipeline_payload() {
    let payload = json!({
        "schema": "burin.stuck_handoff.v1",
        "action": "handoff",
        "terminal": true,
        "pattern": "no_progress_terminator",
        "message": "I am stuck after repeated verification failures.",
    });

    let event = build_agent_event("session-1", "loop_stuck", &payload).expect("loop_stuck event");

    match event {
        AgentEvent::LoopStuckSignal {
            session_id,
            payload: event_payload,
        } => {
            assert_eq!(session_id, "session-1");
            assert_eq!(event_payload, payload);
        }
        other => panic!("expected LoopStuckSignal, got {other:?}"),
    }
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
fn gpt_oss_harmony_leak_persists_clean_reasoning_and_tool_calls() {
    // Guard: the test model must resolve to a native-tools route, or the
    // backstop (which only fires for native-tools models) would no-op and the
    // assertion below would silently pass for the wrong reason.
    let caps = crate::llm::capabilities::lookup("fireworks", "gpt-oss-120b");
    assert!(
        caps.native_tools,
        "test precondition: gpt-oss must be a native-tools route"
    );

    // Leak-shaped llm_result: the provider failed to split harmony channels, so
    // the analysis reasoning AND the inline `tool`-key tool call collapsed into
    // `content` (`text`). The wire `reasoning` field was EMPTY (so `thinking` is
    // absent) and there were NO native tool calls. `vm_build_llm_result` then
    // recovered the call out of the dirty text into the merged `tool_calls`
    // (the `tool`-key dialect now parses). Persistence must rebuild the clean
    // shape rather than replaying the raw blob.
    let dirty = "We need to suppress warnings to make verification consider success. \
                 First inspect the model.\n\n\
                 {\"tool\":\"read\",\"arguments\":{\"path\":\"BatteryInfo.swift\"}}";
    let result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "fireworks",
        "model": "gpt-oss-120b",
        "text": dirty,
        "prose": dirty,
        "_agent_tool_format": "native",
        "native_tool_calls": [],
        "tool_calls": [{
            "id": "native_fallback",
            "name": "read",
            "arguments": {"path": "BatteryInfo.swift"}
        }],
    }));

    let message = vm_to_json(&assistant_message_from_llm_result(&result));

    assert_eq!(message["role"], "assistant");
    // Content must be EMPTY — the dirty blob must not be persisted verbatim.
    assert_eq!(
        message["content"], "",
        "leaked reasoning/JSON must not stay in content"
    );
    // The recovered call must be attached as a structured tool call.
    assert_eq!(message["tool_calls"][0]["function"]["name"], "read");
    // The leaked reasoning trace is preserved privately in `reasoning`, not in
    // `content`, so it is available for transcripts but stripped from the wire.
    assert_eq!(message["reasoning"], json!(dirty));
    // And the dirty blob (incl. the "game the verifier" plan) is gone from the
    // public content surface.
    assert!(
        !message["content"]
            .as_str()
            .unwrap_or_default()
            .contains("suppress warnings"),
        "verifier-gaming CoT leaked into persisted content"
    );
}

#[test]
fn text_tool_calls_replay_as_text_history_even_on_native_capable_routes() {
    let text_call = "<tool_call>\nlookup_ping({ query: \"catalog-refresh\" })\n</tool_call>";
    let result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "moonshot",
        "model": "moonshot/kimi-k2.7-code-highspeed",
        "text": text_call,
        "_agent_tool_format": "text",
        "native_tool_calls": [],
        "tool_calls": [{
            "id": "tc_0",
            "name": "lookup_ping",
            "arguments": {"query": "catalog-refresh"}
        }],
    }));

    let message = vm_to_json(&assistant_message_from_llm_result(&result));

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], text_call);
    assert!(
        message.get("tool_calls").is_none(),
        "text-mode parsed calls must not poison provider-native history"
    );
    assert!(message.get("reasoning").is_none());
}

#[test]
fn initial_user_content_preserves_multimodal_blocks() {
    let mut opts = crate::value::DictMap::new();
    opts.insert(
        crate::value::intern_key("initial_user_content"),
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

/// Anthropic's Messages API rejects (non-retryable HTTP 400) any request in
/// which an assistant `tool_use` block is not immediately followed by a
/// `tool_result` carrying the same id. This mirrors that wire check over the
/// persisted transcript so the repro tests assert the exact failure the run hit.
/// Returns the ids of orphaned `tool_use` blocks (empty = provider-valid).
fn orphaned_tool_use_ids(messages: &[serde_json::Value]) -> Vec<String> {
    let mut orphans = Vec::new();
    for (idx, message) in messages.iter().enumerate() {
        if message.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        // Collect this assistant turn's native tool-call ids (Anthropic content
        // blocks + OpenAI top-level tool_calls).
        let mut ids: Vec<String> = Vec::new();
        if let Some(blocks) = message.get("content").and_then(|v| v.as_array()) {
            for block in blocks {
                if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                    if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
        if let Some(calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
            for call in calls {
                if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                    ids.push(id.to_string());
                }
            }
        }
        if ids.is_empty() {
            continue;
        }
        // The paired result must be the IMMEDIATELY following message(s).
        let next = messages.get(idx + 1);
        let paired_id = next.and_then(|m| {
            let role = m.get("role").and_then(|v| v.as_str());
            if role == Some("tool_result") || role == Some("tool") {
                m.get("tool_use_id")
                    .or_else(|| m.get("tool_call_id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        });
        for id in ids {
            if paired_id.as_deref() != Some(id.as_str()) {
                orphans.push(id);
            }
        }
    }
    orphans
}

/// REPRO of the escalation-orphan HTTP 400, driven through the REAL production
/// entrypoint (`pair_orphaned_tool_use`), not the hardcoded-native helper.
///
/// The bug: `pair_orphaned_tool_use` sourced its synthesis format from the
/// SESSION-locked `tool_format`. On a text-primary run that lock is pinned to
/// `"text"` at session init (`claim_tool_format`) and is never re-claimed on
/// escalation. So when the escalated Anthropic model emits a real native
/// `tool_use` block and the loop declines to dispatch it,
/// `tool_result_message_for_provider` took its text-channel branch and emitted a
/// bare `role:"user"` message — leaving the native `tool_use` block orphaned and
/// re-triggering the exact Anthropic 400 the #3833 repair was supposed to
/// prevent. The masking test proved only that the synthesizer *can* pair when
/// handed `"native"`; it never exercised the session-locked production path.
///
/// This test locks the session to `text`, records the escalated Anthropic turn
/// through the real record path, then calls `pair_orphaned_tool_use`. It MUST
/// fail on pre-fix main (synthesized `role:"user"`, still orphaned) and pass
/// after the fix (native `tool_result` + `tool_use_id`).
#[test]
fn escalation_orphaned_tool_use_repaired_via_production_path_on_text_locked_session() {
    reset_agent_session_host_state();
    let session_id = crate::agent_sessions::open_or_create(Some(
        "orphan-repair-text-lock-anthropic".to_string(),
    ));
    // PRIMARY model was text-format: the session lock is pinned to `text` and is
    // never re-claimed when the run escalates to a native model.
    crate::agent_sessions::claim_tool_format(&session_id, "text")
        .expect("primary text lock claims");

    // The escalated turn ran on anthropic/sonnet — `pair_orphaned_tool_use`
    // reads provider/model from the host session store.
    seed_host_session_provider_model(&session_id, "anthropic", "claude-sonnet-4-5");

    // Seed the transcript: user task, then the escalated Anthropic assistant turn
    // carrying a real native `tool_use` block, recorded exactly as the loop does.
    crate::agent_sessions::inject_message(
        &session_id,
        crate::stdlib::json_to_vm_value(&json!({"role": "user", "content": "fix auth"})),
    )
    .expect("user turn injects");
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
        "text": "I'll apply the fix.",
        "_agent_tool_format": "native",
        "native_tool_calls": [{
            "id": "tc_0",
            "name": "edit",
            "arguments": {"path": "auth.go", "body": "package auth"}
        }],
    }));
    let assistant = assistant_message_from_llm_result(&llm_result);
    // Sanity: this really persisted as an Anthropic tool_use block.
    let assistant_json = vm_to_json(&assistant);
    assert_eq!(assistant_json["content"][1]["type"], "tool_use");
    assert_eq!(assistant_json["content"][1]["id"], "tc_0");
    crate::agent_sessions::inject_message(&session_id, assistant).expect("assistant turn injects");

    // The loop declines to dispatch and is about to inject bare user feedback.
    // Repair first, through the REAL entrypoint (session-locked to `text`).
    let feedback = "Emit your tool call as a native tool_use block, not text.";
    let repaired = pair_orphaned_tool_use(&session_id, feedback);
    assert_eq!(repaired, 1, "exactly one orphan must be repaired");

    // The synthesized message MUST be a native Anthropic tool_result, NOT the
    // text-channel `role:"user"` echo — otherwise the native tool_use stays
    // orphaned and the provider 400 fires anyway.
    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let last = vm_to_json(messages.last().expect("a synthesized trailing message"));
    assert_eq!(
        last["role"], "tool_result",
        "orphan repair must ride the native tool_result role, not role:\"user\" \
         (the session text-lock must NOT leak into orphan synthesis)"
    );
    assert_eq!(last["tool_use_id"], "tc_0");
    assert_eq!(last["content"], feedback);

    // And the transcript now has no orphaned tool_use ids -> provider-valid.
    let messages_json: Vec<serde_json::Value> = messages.iter().map(vm_to_json).collect();
    assert!(
        orphaned_tool_use_ids(&messages_json).is_empty(),
        "after repair the tool_use must be paired -> provider-valid"
    );
}

/// The openai-compat escalation shape (top-level `tool_calls`,
/// `tool`/`tool_call_id` result role) must also repair through the production
/// path when the session is text-locked.
#[test]
fn escalation_orphan_repaired_via_production_path_openai_shape() {
    reset_agent_session_host_state();
    let session_id =
        crate::agent_sessions::open_or_create(Some("orphan-repair-text-lock-openai".to_string()));
    crate::agent_sessions::claim_tool_format(&session_id, "text").expect("text lock claims");
    seed_host_session_provider_model(&session_id, "local", "Qwen/Qwen3.6-35B-A3B");

    crate::agent_sessions::inject_message(
        &session_id,
        crate::stdlib::json_to_vm_value(&json!({"role": "user", "content": "read main.rs"})),
    )
    .expect("user turn injects");
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "local",
        "model": "Qwen/Qwen3.6-35B-A3B",
        "text": "",
        "_agent_tool_format": "native",
        "native_tool_calls": [{
            "id": "call_9",
            "name": "read",
            "arguments": {"path": "main.rs"}
        }],
    }));
    crate::agent_sessions::inject_message(
        &session_id,
        assistant_message_from_llm_result(&llm_result),
    )
    .expect("assistant turn injects");

    let repaired = pair_orphaned_tool_use(&session_id, "nudge");
    assert_eq!(repaired, 1);

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let last = vm_to_json(messages.last().expect("a synthesized trailing message"));
    assert_eq!(
        last["role"], "tool",
        "openai-shape orphan repair must ride the native `tool` role"
    );
    assert_eq!(last["name"], "read");
    assert_eq!(last["tool_call_id"], "call_9");
    assert_eq!(last["content"], "nudge");
}

/// The DISPATCHED escalation path (`record_tool_results`) had the SAME latent
/// text-lock bug as the orphan-repair path: it recorded a dispatched result
/// using the session-locked `tool_format`, which stays `"text"` on a
/// text-primary run even after escalating to a native model. So a native
/// escalated tool call that WAS dispatched got its result recorded as a bare
/// `role:"user"` message — leaving the assistant's native `tool_use` block
/// orphaned and re-triggering the Anthropic 400 on the SUCCESSFUL-dispatch path.
///
/// This exercises the real `record_tool_results` builtin against a text-locked
/// session whose trailing assistant turn carries a native anthropic `tool_use`
/// block, and asserts the recorded result rides the native `tool_result` role.
#[test]
fn dispatched_escalation_result_records_native_role_on_text_locked_session() {
    reset_agent_session_host_state();
    let session_id =
        crate::agent_sessions::open_or_create(Some("record-native-under-text-lock".to_string()));
    crate::agent_sessions::claim_tool_format(&session_id, "text").expect("text lock claims");
    seed_host_session_provider_model(&session_id, "anthropic", "claude-sonnet-4-5");

    crate::agent_sessions::inject_message(
        &session_id,
        crate::stdlib::json_to_vm_value(&json!({"role": "user", "content": "read main"})),
    )
    .expect("user turn injects");
    // Escalated native assistant turn carrying a real anthropic tool_use block.
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
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

    // Dispatch result for the native call, shaped as agent_dispatch_tool_batch
    // returns it (a flat list with tool_use_id).
    let dispatch = crate::stdlib::json_to_vm_value(&json!([{
        "tool_name": "read",
        "tool_use_id": "tc_0",
        "ok": true,
        "observation": "file contents",
    }]));
    super::record_tool_results_for_test(&session_id, dispatch);

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let last = vm_to_json(messages.last().expect("a recorded result message"));
    assert_eq!(
        last["role"], "tool_result",
        "a dispatched native escalation result must ride the native tool_result role, \
         not role:\"user\" (the session text-lock must NOT leak into the record path)"
    );
    assert_eq!(last["tool_use_id"], "tc_0");

    let messages_json: Vec<serde_json::Value> = messages.iter().map(vm_to_json).collect();
    assert!(
        orphaned_tool_use_ids(&messages_json).is_empty(),
        "the dispatched native tool_use must be paired -> provider-valid"
    );
}

/// REGRESSION GUARD for the record path: a homogeneous text-channel run keeps
/// its calls inline in `content`, so the trailing assistant turn carries NO
/// structured block. `record_tool_results` must keep recording results on the
/// text-channel `role:"user"` echo — the native-format override must NOT fire.
#[test]
fn dispatched_text_channel_result_stays_user_echo() {
    reset_agent_session_host_state();
    let session_id =
        crate::agent_sessions::open_or_create(Some("record-text-homogeneous".to_string()));
    crate::agent_sessions::claim_tool_format(&session_id, "text").expect("text lock claims");
    seed_host_session_provider_model(&session_id, "moonshot", "moonshot/kimi-k2.7-code-highspeed");

    crate::agent_sessions::inject_message(
        &session_id,
        crate::stdlib::json_to_vm_value(&json!({"role": "user", "content": "read main"})),
    )
    .expect("user turn injects");
    // Text-channel assistant turn: the call is inline in `content`, no structured
    // block persists.
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
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

    let dispatch = crate::stdlib::json_to_vm_value(&json!([{
        "tool_name": "read",
        "tool_call_id": "tc_0",
        "ok": true,
        "observation": "file contents",
    }]));
    super::record_tool_results_for_test(&session_id, dispatch);

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(crate::value::VmValue::Nil),
    );
    let last = vm_to_json(messages.last().expect("a recorded result message"));
    assert_eq!(
        last["role"], "user",
        "homogeneous text-channel results must stay on the user echo (no native override)"
    );
}

/// The repair covers the OpenAI-compatible wire shape too (top-level
/// `tool_calls`, `tool`/`tool_call_id` result role) — escalation targets aren't
/// only Anthropic.
#[test]
fn orphan_repair_covers_openai_wire_shape() {
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "local",
        "model": "Qwen/Qwen3.6-35B-A3B",
        "text": "",
        "_agent_tool_format": "native",
        "native_tool_calls": [{
            "id": "call_9",
            "name": "read",
            "arguments": {"path": "main.rs"}
        }],
    }));
    let assistant = assistant_message_from_llm_result(&llm_result);
    let synthetic = synthesize_orphan_tool_results(
        &assistant,
        "local",
        "Qwen/Qwen3.6-35B-A3B",
        "nudge",
        &std::collections::BTreeSet::new(),
    );
    assert_eq!(synthetic.len(), 1);
    let msg = vm_to_json(&synthetic[0]);
    assert_eq!(msg["role"], "tool");
    assert_eq!(msg["name"], "read");
    assert_eq!(msg["tool_call_id"], "call_9");
    assert_eq!(msg["content"], "nudge");
}

/// REGRESSION GUARD: a homogeneous text-format run keeps its tool calls inline
/// in `content` (a plain string), so the assistant message carries NO structured
/// tool_use block. The repair must synthesize nothing — proving passing runs are
/// untouched.
#[test]
fn orphan_repair_is_noop_for_text_format_runs() {
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "moonshot",
        "model": "moonshot/kimi-k2.7-code-highspeed",
        "text": "read({ path: \"main.rs\" })",
        "_agent_tool_format": "text",
        "native_tool_calls": [],
        "tool_calls": [{"id": "tc_0", "name": "read", "arguments": {"path": "main.rs"}}],
    }));
    let assistant = assistant_message_from_llm_result(&llm_result);
    // Precondition: text-format history keeps the call inline, no structured block.
    let assistant_json = vm_to_json(&assistant);
    assert!(assistant_json.get("tool_calls").is_none());
    assert!(assistant_json["content"].is_string());

    let synthetic = synthesize_orphan_tool_results(
        &assistant,
        "moonshot",
        "moonshot/kimi-k2.7-code-highspeed",
        "nudge",
        &std::collections::BTreeSet::new(),
    );
    assert!(
        synthetic.is_empty(),
        "text-format runs carry no structured tool_use; nothing to repair"
    );
}

/// REGRESSION GUARD: a block whose id ALREADY has a paired tool_result (the loop
/// dispatched it normally) must not get a second, synthetic result.
#[test]
fn orphan_repair_skips_already_paired_blocks() {
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "anthropic",
        "model": "claude-opus-4-8",
        "text": "",
        "_agent_tool_format": "native",
        "native_tool_calls": [{"id": "tc_0", "name": "read", "arguments": {"path": "a"}}],
    }));
    let assistant = assistant_message_from_llm_result(&llm_result);
    let mut paired = std::collections::BTreeSet::new();
    paired.insert("tc_0".to_string());
    let synthetic = synthesize_orphan_tool_results(
        &assistant,
        "anthropic",
        "claude-opus-4-8",
        "nudge",
        &paired,
    );
    assert!(
        synthetic.is_empty(),
        "an already-dispatched block must not be double-paired"
    );
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
            crate::value::intern_key("policy"),
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
