use super::*;

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
    crate::llm::agent_observe::reset_transcript_dedup();

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

    let snapshot = crate::llm::daemon::load_snapshot(snapshot_path.to_str().unwrap()).unwrap();
    assert_eq!(snapshot.daemon_state, "budget_exhausted");
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

#[tokio::test(flavor = "current_thread")]
async fn persisted_session_keeps_compacted_prompt_surface_on_resume() {
    reset_llm_mock_state();
    crate::agent_sessions::reset_session_store();
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: "first answer".to_string(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    });

    let session_id = "persisted_compaction_resume".to_string();
    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "very old task context",
    })]);
    let mut config = base_agent_config();
    config.session_id = session_id.clone();
    config.daemon = true;
    config.daemon_config = DaemonLoopConfig {
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

    let prompt_state = crate::agent_sessions::prompt_state_json(&session_id);
    assert_eq!(prompt_state.messages[0]["role"].as_str(), Some("user"));
    assert!(
        prompt_state.messages[0]["content"]
            .as_str()
            .unwrap_or("")
            .contains("[auto-compacted"),
        "compacted prompt surface should be persisted for resume: {:?}",
        prompt_state.messages
    );
    assert!(
        !prompt_state.messages.iter().any(|message| {
            message.get("role").and_then(|value| value.as_str()) == Some("user")
                && message.get("content").and_then(|value| value.as_str())
                    == Some("very old task context")
        }),
        "stale pre-compaction user turn should not survive in the resume surface: {:?}",
        prompt_state.messages
    );

    reset_llm_mock_state();
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: "second answer".to_string(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    });

    let mut resume_opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "follow-up task",
    })]);
    let mut resume_config = base_agent_config();
    resume_config.session_id = session_id.clone();
    let _ = run_agent_loop_internal(&mut resume_opts, resume_config)
        .await
        .unwrap();

    let calls = get_llm_mock_calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].messages[0]["content"]
            .as_str()
            .unwrap_or("")
            .contains("[auto-compacted"),
        "resume call should start from compacted summary, not the stale pre-compaction transcript: {:?}",
        calls[0].messages
    );

    crate::agent_sessions::reset_session_store();
    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn user_response_block_can_complete_persistent_loop_without_done_sentinel() {
    reset_llm_mock_state();
    let response_text = "<tool_call>\nledger({ action: \"note\", text: \"verified completion\" })\n</tool_call>\n<user_response>Completed cleanly.</user_response>";
    let parsed = crate::llm::tools::parse_text_tool_calls_with_tools(response_text, None);
    assert_eq!(parsed.calls.len(), 1, "parsed: {:?}", parsed.errors);
    assert_eq!(parsed.user_response.as_deref(), Some("Completed cleanly."));
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: response_text.to_string(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    });

    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "finish the task",
    })]);
    let mut tool_params = std::collections::BTreeMap::new();
    tool_params.insert(
        "path".to_string(),
        VmValue::Dict(Rc::new(std::collections::BTreeMap::from([(
            "type".to_string(),
            VmValue::String(Rc::from("string")),
        )]))),
    );
    let dummy_tool = VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
        ("name".to_string(), VmValue::String(Rc::from("read"))),
        (
            "description".to_string(),
            VmValue::String(Rc::from("Read a file.")),
        ),
        (
            "parameters".to_string(),
            VmValue::Dict(Rc::new(tool_params)),
        ),
    ])));
    opts.tools = Some(VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
        (
            "tools".to_string(),
            VmValue::List(Rc::new(vec![dummy_tool])),
        ),
    ]))));
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 2;

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_eq!(result["status"], "done");
    assert_eq!(result["visible_text"].as_str(), Some("Completed cleanly."));
    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn ledger_tool_is_rejected_when_no_task_ledger_is_active() {
    reset_llm_mock_state();
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: "<tool_call>\nledger({ action: \"note\", text: \"hidden state\" })\n</tool_call>"
            .to_string(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    });

    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "do one thing",
    })]);
    let mut tool_params = std::collections::BTreeMap::new();
    tool_params.insert(
        "path".to_string(),
        VmValue::Dict(Rc::new(std::collections::BTreeMap::from([(
            "type".to_string(),
            VmValue::String(Rc::from("string")),
        )]))),
    );
    let dummy_tool = VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
        ("name".to_string(), VmValue::String(Rc::from("read"))),
        (
            "description".to_string(),
            VmValue::String(Rc::from("Read a file.")),
        ),
        (
            "parameters".to_string(),
            VmValue::Dict(Rc::new(tool_params)),
        ),
    ])));
    opts.tools = Some(VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
        (
            "tools".to_string(),
            VmValue::List(Rc::new(vec![dummy_tool])),
        ),
    ]))));
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 1;

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_eq!(result["successful_tools"], json!([]));
    assert_eq!(result["rejected_tools"], json!(["ledger"]));
    let transcript = serde_json::to_string(&result["transcript"]).expect("transcript json");
    assert!(transcript.contains("ledger unavailable"));
    reset_llm_mock_state();
}

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

/// Two sequential `agent_loop` calls with the same `session_id`
/// produce a coherent conversation: the second call sees the first
/// call's assistant reply as prior history. This is the core
/// persistence invariant for first-class sessions.
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_session_id_persists_across_calls() {
    crate::reset_thread_local_state();
    let session_id = format!("session-persistence-{}", uuid::Uuid::now_v7());

    // First call: single user message.
    let mut opts_a = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "first-turn-prompt",
    })]);
    let mut config_a = base_agent_config();
    config_a.session_id = session_id.clone();
    let result_a = run_agent_loop_internal(&mut opts_a, config_a)
        .await
        .expect("first call");
    let messages_a = result_a["transcript"]["messages"]
        .as_array()
        .expect("transcript.messages is a list");
    assert!(
        messages_a.len() >= 2,
        "first call should have at least user+assistant, got {}",
        messages_a.len()
    );

    // Snapshot the mock call count before the second call.
    let calls_before = get_llm_mock_calls().len();

    // Second call with the SAME session_id: only one new user message.
    let mut opts_b = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "second-turn-prompt",
    })]);
    let mut config_b = base_agent_config();
    config_b.session_id = session_id.clone();
    let result_b = run_agent_loop_internal(&mut opts_b, config_b)
        .await
        .expect("second call");

    // The second call's transcript must include the first call's
    // prior messages plus the new turn — so it is strictly longer.
    let messages_b = result_b["transcript"]["messages"]
        .as_array()
        .expect("transcript.messages is a list");
    assert!(
        messages_b.len() > messages_a.len(),
        "second call transcript must extend first ({}→{})",
        messages_a.len(),
        messages_b.len()
    );

    // Verify the mock actually SAW the prefix on its first call of the
    // second loop — not just that the transcript was assembled in the
    // finalize step. This is the real test of "session prefix load".
    let calls = get_llm_mock_calls();
    let second_loop_first_call = calls
        .get(calls_before)
        .expect("second loop issued at least one call");
    let sent = &second_loop_first_call.messages;
    assert!(
        sent.iter()
            .any(|m| m.get("content").and_then(|c| c.as_str()) == Some("first-turn-prompt")),
        "prefix from session store missing; sent messages were {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|m| m.get("content").and_then(|c| c.as_str()) == Some("second-turn-prompt")),
        "caller's new message missing; sent messages were {sent:?}"
    );

    // Cleanup: close the session so later tests on the same thread
    // don't inherit state.
    crate::agent_sessions::close(&session_id);
}

/// `agent_session_reset` on a session clears the prior prefix before
/// the next `agent_loop` sees it.
#[tokio::test(flavor = "current_thread")]
async fn agent_session_reset_drops_prefix_for_next_loop() {
    crate::reset_thread_local_state();
    let session_id = format!("session-reset-{}", uuid::Uuid::now_v7());

    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "seed-prompt",
    })]);
    let mut config = base_agent_config();
    config.session_id = session_id.clone();
    let _ = run_agent_loop_internal(&mut opts, config)
        .await
        .expect("seed call");

    // Reset, then run again. The mock should see ONLY the new prompt.
    assert!(crate::agent_sessions::reset_transcript(&session_id));

    let calls_before = get_llm_mock_calls().len();
    let mut opts_after = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "post-reset-prompt",
    })]);
    let mut config_after = base_agent_config();
    config_after.session_id = session_id.clone();
    let _ = run_agent_loop_internal(&mut opts_after, config_after)
        .await
        .expect("post-reset call");

    let calls = get_llm_mock_calls();
    let first_post_reset = calls
        .get(calls_before)
        .expect("post-reset loop issued a call");
    assert!(
        !first_post_reset
            .messages
            .iter()
            .any(|m| m.get("content").and_then(|c| c.as_str()) == Some("seed-prompt")),
        "reset should drop the prior prefix, got {:?}",
        first_post_reset.messages
    );

    crate::agent_sessions::close(&session_id);
}

/// An `agent_loop` call without a `session_id` does NOT persist any
/// transcript — subsequent calls with the same (anonymous) minted id
/// can't see it because each call mints its own fresh id.
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_without_session_id_does_not_persist() {
    crate::reset_thread_local_state();

    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "anonymous-prompt",
    })]);
    let mut config = base_agent_config();
    config.session_id = String::new(); // anonymous
    let result = run_agent_loop_internal(&mut opts, config)
        .await
        .expect("anonymous call");
    let minted_id = result["transcript"]["id"]
        .as_str()
        .expect("transcript.id")
        .to_string();
    assert!(
        !crate::agent_sessions::exists(&minted_id),
        "anonymous call must not leave its minted id in the session store"
    );
}
