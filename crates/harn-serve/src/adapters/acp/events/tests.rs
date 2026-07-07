use harn_vm::agent_events::{
    AgentEvent, AgentEventSink, FsWatchEvent, ToolCallErrorCategory, ToolCallStatus, ToolExecutor,
};
use harn_vm::composition::{
    composition_snippet_hash, CompositionChildCall, CompositionChildResult,
    CompositionFailureCategory, CompositionRunEnvelope,
};
use harn_vm::llm::receipts::ToolCallReceipt;
use harn_vm::orchestration::{
    HandoffArtifact, HandoffTargetRecord, MutationSessionRecord, ToolApprovalPolicy,
};
use harn_vm::tool_annotations::{SideEffectLevel, ToolAnnotations, ToolKind};
use tokio::sync::mpsc;

use super::super::schema::{
    ACP_SESSION_UPDATE_VARIANTS, HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD,
    HARN_SESSION_UPDATE_EXTENSIONS,
};
use super::{AcpAgentEventSink, AcpOutput};

async fn collect_notifications(events: Vec<AgentEvent>) -> Vec<serde_json::Value> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    let expected_len = events.len();
    for event in events {
        sink.handle_event(&event);
    }

    let mut notifications = Vec::with_capacity(expected_len);
    for _ in 0..expected_len {
        let line = rx.recv().await.expect("ACP event notification");
        notifications.push(serde_json::from_str(&line).expect("json"));
    }
    notifications
}

fn update_harn_meta(payload: &serde_json::Value) -> &serde_json::Value {
    &payload["params"]["update"]["_meta"]["harn"]
}

fn fixture_handoff() -> HandoffArtifact {
    HandoffArtifact {
        type_name: "handoff_artifact".to_string(),
        id: "handoff-1".to_string(),
        parent_run_id: None,
        source_persona: "merge_captain".to_string(),
        target_persona_or_human: HandoffTargetRecord {
            kind: "persona".to_string(),
            id: Some("review_captain".to_string()),
            label: Some("review_captain".to_string()),
            uri: None,
        },
        task: "Review the patch".to_string(),
        reason: "Merge queue requires review".to_string(),
        created_at: "2026-04-28T00:00:00Z".to_string(),
        ..Default::default()
    }
    .normalize()
}

fn fixture_tool_call_receipt() -> ToolCallReceipt {
    ToolCallReceipt {
        schema_version: 1,
        session_id: "session-1".to_string(),
        run_id: Some("run-1".to_string()),
        tool_call_id: "tool-1".to_string(),
        tool_name: "read_file".to_string(),
        iteration: 6,
        turn_index: Some(5),
        emit_order: 0,
        reason: Some("Read project context".to_string()),
        kind: Some("read".to_string()),
        executor: Some("harn".to_string()),
        status: "ok".to_string(),
        error_category: None,
        duration_ms: 7,
        args_hash: "0".repeat(64),
        result_hash: Some("1".repeat(64)),
        audit: serde_json::json!({
            "summary": "Read project context",
            "consent": "not_required"
        }),
        emitted_at: "2026-05-16T00:00:00Z".to_string(),
        model: Some("mock".to_string()),
        provider: Some("mock".to_string()),
    }
}

fn standard_fixture_events() -> Vec<AgentEvent> {
    vec![
        AgentEvent::AgentMessageChunk {
            session_id: "session-1".to_string(),
            content: "hello".to_string(),
        },
        AgentEvent::AgentThoughtChunk {
            session_id: "session-1".to_string(),
            content: "thinking".to_string(),
        },
        AgentEvent::ToolCall {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "read".to_string(),
            kind: Some(ToolKind::Read),
            status: ToolCallStatus::Pending,
            raw_input: serde_json::json!({"path": "README.md"}),
            parsing: None,
            audit: None,
        },
        AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "read".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: Some(serde_json::json!({"ok": true})),
            error: None,
            duration_ms: Some(7),
            execution_duration_ms: Some(5),
            error_category: None,
            executor: Some(ToolExecutor::HarnBuiltin),
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        },
        AgentEvent::Plan {
            session_id: "session-1".to_string(),
            plan: serde_json::json!([
                {"content": "edit", "status": "pending"}
            ]),
        },
    ]
}

fn extension_fixture_events() -> Vec<AgentEvent> {
    vec![
        AgentEvent::Artifact {
            session_id: "session-1".to_string(),
            artifact_id: "artifact-chart-1".to_string(),
            kind: "vega-lite".to_string(),
            title: Some("Build throughput".to_string()),
            mime_type: "application/vnd.vegalite.v5+json".to_string(),
            spec: serde_json::json!({
                "mark": "bar",
                "data": {"values": [{"name": "a", "count": 2}]},
                "encoding": {
                    "x": {"field": "name", "type": "nominal"},
                    "y": {"field": "count", "type": "quantitative"}
                }
            }),
            fallback: "Build throughput (bar chart)".to_string(),
            size_bytes: 153,
            provenance: serde_json::json!({"source": "agent"}),
            metadata: serde_json::json!({"unit": "builds"}),
        },
        AgentEvent::SkillActivated {
            session_id: "session-1".to_string(),
            skill_name: "rust".to_string(),
            iteration: 1,
            reason: "matched".to_string(),
        },
        AgentEvent::SkillDeactivated {
            session_id: "session-1".to_string(),
            skill_name: "rust".to_string(),
            iteration: 2,
        },
        AgentEvent::SkillScopeTools {
            session_id: "session-1".to_string(),
            skill_name: "rust".to_string(),
            allowed_tools: vec!["read".to_string()],
        },
        AgentEvent::SkillNarrow {
            session_id: "session-1".to_string(),
            reason: "unused across 5 turns".to_string(),
            removed_tools: vec!["write".to_string()],
            remaining_tools: vec!["read".to_string()],
            policy: serde_json::Value::Null,
            removed_tool_details: serde_json::Value::Null,
            kept_tool_details: serde_json::Value::Null,
        },
        AgentEvent::StanceTransition {
            session_id: "session-1".to_string(),
            phase: "write_access_granted".to_string(),
            escape_tool: "request_write_access".to_string(),
            allowed_tools: vec![
                "look".to_string(),
                "search".to_string(),
                "request_write_access".to_string(),
            ],
            justification: "User asked me to make the change.".to_string(),
            consent: "express".to_string(),
            reason: "The user explicitly asked for the edit.".to_string(),
        },
        AgentEvent::ToolSearchQuery {
            session_id: "session-1".to_string(),
            tool_use_id: "search-1".to_string(),
            name: "tool_search".to_string(),
            query: serde_json::json!({"q": "read"}),
            strategy: "semantic".to_string(),
            mode: "client".to_string(),
        },
        AgentEvent::ToolSearchResult {
            session_id: "session-1".to_string(),
            tool_use_id: "search-1".to_string(),
            promoted: vec!["read".to_string()],
            strategy: "semantic".to_string(),
            mode: "client".to_string(),
        },
        AgentEvent::TranscriptCompacted {
            session_id: "session-1".to_string(),
            mode: "auto".to_string(),
            reason: "threshold".to_string(),
            strategy: "summary".to_string(),
            archived_messages: 3,
            estimated_tokens_before: 100,
            estimated_tokens_after: 40,
            snapshot_asset_id: Some("asset-1".to_string()),
            instruction_mode: Some("extend".to_string()),
            instruction_source: Some("host".to_string()),
            compaction_policy: Some(serde_json::json!({
                "instructions": "Preserve failing tests.",
                "instruction_mode": "extend",
                "instruction_source": "host"
            })),
        },
        AgentEvent::TranscriptProjected {
            session_id: "session-1".to_string(),
            policy: "clean_tool_repair".to_string(),
            reason: "tool_call_repair_squashed".to_string(),
            prefix_hash: "sha256:abc".to_string(),
            kept_count: 3,
            dropped_count: 2,
            provider_safety_blocked: false,
            redacted_count: 0,
            reclaimed_tokens: 0,
            roots_consulted: Vec::new(),
            redaction_pointers: Vec::new(),
        },
        AgentEvent::ReminderEmitted {
            session_id: "session-1".to_string(),
            reminder_id: "reminder-1".to_string(),
            tags: vec!["token_pressure".to_string()],
            body: "Refresh the compacted context before answering.".to_string(),
            role_hint: "developer".to_string(),
            rendered_role: "developer".to_string(),
            source: "stdlib_provider".to_string(),
            ttl_turns: Some(2),
        },
        AgentEvent::Handoff {
            session_id: "session-1".to_string(),
            artifact_id: "artifact-1".to_string(),
            handoff: Box::new(fixture_handoff()),
        },
        AgentEvent::FsWatch {
            session_id: "session-1".to_string(),
            subscription_id: "fsw-1".to_string(),
            events: vec![FsWatchEvent {
                kind: "modify".to_string(),
                paths: vec!["/tmp/project/src/lib.rs".to_string()],
                relative_paths: vec!["src/lib.rs".to_string()],
                raw_kind: "Modify(Any)".to_string(),
                error: None,
            }],
        },
        AgentEvent::WorkerUpdate {
            session_id: "session-1".into(),
            worker_id: "worker-1".into(),
            worker_name: "review".into(),
            worker_task: "review pr".into(),
            worker_mode: "delegated_stage".into(),
            event: harn_vm::agent_events::WorkerEvent::WorkerWaitingForInput,
            status: "awaiting_input".into(),
            metadata: serde_json::json!({
                "child_run_id": "run_x",
                "child_run_path": ".harn-runs/run_x",
            }),
            audit: Some(serde_json::json!({"run_id": "run_x"})),
        },
        AgentEvent::HitlRequested {
            session_id: "session-1".into(),
            request_id: "hitl_question_session-1_1".into(),
            kind: "question".into(),
            payload: serde_json::json!({"prompt": "Approve deploy?"}),
        },
        AgentEvent::HitlResolved {
            session_id: "session-1".into(),
            request_id: "hitl_question_session-1_1".into(),
            kind: "question".into(),
            outcome: "answered".into(),
        },
    ]
}

#[tokio::test(flavor = "current_thread")]
async fn standard_session_update_fixtures_match_acp_schema_v0_12_2_discriminators() {
    let actual = collect_notifications(standard_fixture_events()).await;
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/acp/session_update_standard.json"
    ))
    .expect("fixture json");
    assert_eq!(serde_json::Value::Array(actual.clone()), expected);

    for notification in actual {
        let session_update = notification["params"]["update"]["sessionUpdate"]
            .as_str()
            .expect("sessionUpdate");
        assert!(
            ACP_SESSION_UPDATE_VARIANTS.contains(&session_update),
            "{session_update} is not a standard ACP v0.12.2 SessionUpdate"
        );
        if session_update == "plan" {
            assert!(notification["params"]["update"].get("entries").is_some());
            assert!(notification["params"]["update"].get("plan").is_none());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn protocol_conformance_standard_session_update_fixture_is_adapter_generated() {
    let actual = collect_notifications(standard_fixture_events()).await;
    crate::protocol_fixture_tests::assert_fixture_documents_match(
        "conformance/protocols/fixtures/acp/session_update_adapter_standard.valid.json",
        actual,
    );
}

fn agent_event_ext_fixture_events() -> Vec<AgentEvent> {
    let read_only_start = CompositionRunEnvelope::read_only(
        "cmp-1",
        "harn",
        composition_snippet_hash("harn", "tool.search(\"agent_events\")"),
        "sha256:manifest-readonly",
    );
    let read_only_finish = CompositionRunEnvelope {
        stdout: Some("2 files scanned".to_string()),
        result: Some(serde_json::json!({"matches": ["crates/harn-vm/src/lib.rs"]})),
        duration_ms: Some(18),
        ..read_only_start.clone()
    };
    let mut failed_start = CompositionRunEnvelope::read_only(
        "cmp-2",
        "harn",
        composition_snippet_hash("harn", "write_file(\"src/lib.rs\", \"...\")"),
        "sha256:manifest-write",
    );
    failed_start.requested_side_effect_ceiling = SideEffectLevel::WorkspaceWrite;
    let mut failed_error = failed_start.clone();
    failed_error.failure_category = Some(CompositionFailureCategory::PolicyDenied);
    failed_error.error = Some("workspace writes require approval".to_string());
    failed_error.duration_ms = Some(3);

    vec![
        AgentEvent::IterationStart {
            session_id: "session-1".to_string(),
            iteration: 0,
            provider: String::new(),
            model: String::new(),
        },
        AgentEvent::IterationEnd {
            session_id: "session-1".to_string(),
            iteration: 0,
            iteration_info: serde_json::json!({
                "tool_calls": 2,
                "tool_names": ["read_file", "grep"]
            }),
        },
        AgentEvent::CompositionStart {
            session_id: "session-1".to_string(),
            run: read_only_start,
        },
        AgentEvent::CompositionChildCall {
            session_id: "session-1".to_string(),
            call: CompositionChildCall {
                run_id: "cmp-1".to_string(),
                tool_call_id: "tool-cmp-1".to_string(),
                tool_name: "tool.search".to_string(),
                operation_index: 0,
                requested_side_effect_level: SideEffectLevel::ReadOnly,
                annotations: Some(ToolAnnotations {
                    kind: ToolKind::Search,
                    side_effect_level: SideEffectLevel::ReadOnly,
                    ..ToolAnnotations::default()
                }),
                policy_context: serde_json::json!({
                    "ceiling": "read_only",
                    "approval": "not_required"
                }),
                raw_input: serde_json::json!({"query": "agent_events"}),
            },
        },
        AgentEvent::CompositionChildResult {
            session_id: "session-1".to_string(),
            result: CompositionChildResult {
                run_id: "cmp-1".to_string(),
                tool_call_id: "tool-cmp-1".to_string(),
                tool_name: "tool.search".to_string(),
                operation_index: 0,
                status: ToolCallStatus::Completed,
                raw_output: Some(serde_json::json!({"count": 1})),
                executor: Some(ToolExecutor::HarnBuiltin),
                duration_ms: Some(11),
                execution_duration_ms: Some(9),
                ..CompositionChildResult::default()
            },
        },
        AgentEvent::CompositionFinish {
            session_id: "session-1".to_string(),
            run: read_only_finish,
        },
        AgentEvent::CompositionStart {
            session_id: "session-1".to_string(),
            run: failed_start,
        },
        AgentEvent::CompositionChildCall {
            session_id: "session-1".to_string(),
            call: CompositionChildCall {
                run_id: "cmp-2".to_string(),
                tool_call_id: "tool-cmp-2".to_string(),
                tool_name: "tool.write_file".to_string(),
                operation_index: 0,
                requested_side_effect_level: SideEffectLevel::WorkspaceWrite,
                annotations: Some(ToolAnnotations {
                    kind: ToolKind::Edit,
                    side_effect_level: SideEffectLevel::WorkspaceWrite,
                    ..ToolAnnotations::default()
                }),
                policy_context: serde_json::json!({
                    "ceiling": "workspace_write",
                    "approval": "denied"
                }),
                raw_input: serde_json::json!({
                    "path": "src/lib.rs",
                    "content": "..."
                }),
            },
        },
        AgentEvent::CompositionChildResult {
            session_id: "session-1".to_string(),
            result: CompositionChildResult {
                run_id: "cmp-2".to_string(),
                tool_call_id: "tool-cmp-2".to_string(),
                tool_name: "tool.write_file".to_string(),
                operation_index: 0,
                status: ToolCallStatus::Failed,
                error: Some("workspace writes require approval".to_string()),
                error_category: Some(ToolCallErrorCategory::PermissionDenied),
                duration_ms: Some(3),
                execution_duration_ms: Some(0),
                ..CompositionChildResult::default()
            },
        },
        AgentEvent::CompositionError {
            session_id: "session-1".to_string(),
            run: failed_error,
        },
        AgentEvent::CompassRoutingDecision {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-edit-1".to_string(),
            mode: "rewrite".to_string(),
            action: "rewritten".to_string(),
            persona: "fixer".to_string(),
            original_tool: "str_replace".to_string(),
            routed_tool: "edit_safe_text_patch".to_string(),
            target_tool: "edit_safe_text_patch".to_string(),
            path: Some("src/lib.rs".to_string()),
        },
        AgentEvent::SessionClosed {
            session_id: "session-1".to_string(),
            reason: "timeout".to_string(),
            status: "timeout".to_string(),
            metadata: serde_json::json!({"idle_ms": 5000}),
        },
        AgentEvent::JudgeDecision {
            session_id: "session-1".to_string(),
            iteration: 0,
            verdict: "continue".to_string(),
            reasoning: "needs one more concrete action".to_string(),
            next_step: Some("run the verifier".to_string()),
            judge_duration_ms: 42,
            trigger: Some("stalled".to_string()),
            reason: Some("no_source_write".to_string()),
            confirm: Some(false),
            converted_from: Some("cosmetic_only".to_string()),
        },
        AgentEvent::StructuralValidatorDecision {
            session_id: "session-1".to_string(),
            iteration: 1,
            rule: "non_empty_when_writes_expected".to_string(),
            diagnostic: "Assistant emitted no tool calls while writable tools were available."
                .to_string(),
            recommended_action:
                "Emit the concrete write or edit tool call needed for the task, or only mark the task done after that work is complete."
                    .to_string(),
            vetoed: true,
            skipped: false,
            reason: None,
            on_failure: "regenerate_with_feedback".to_string(),
            attempts: 0,
            max_attempts: 3,
        },
        AgentEvent::TypedCheckpoint {
            session_id: "session-1".to_string(),
            checkpoint: serde_json::json!({
                "type": "stage_gate",
                "stage": "verify"
            }),
        },
        AgentEvent::FeedbackInjected {
            session_id: "session-1".to_string(),
            kind: "protocol_violation".to_string(),
            content: "missed required tool call; reissuing".to_string(),
            streak: None,
        },
        AgentEvent::BudgetExhausted {
            session_id: "session-1".to_string(),
            max_iterations: 8,
            kind: Some("max_iterations".to_string()),
            cost_usd: Some(0.12),
            wall_clock_ms: Some(1_500),
        },
        AgentEvent::BudgetCircuitBreaker {
            session_id: "session-1".to_string(),
            kind: "consecutive_failures".to_string(),
            consecutive_count: 3,
            paused_for_ms: 2_000,
        },
        AgentEvent::LoopStuck {
            session_id: "session-1".to_string(),
            max_nudges: 3,
            last_iteration: 4,
            tail_excerpt: "still thinking...".to_string(),
        },
        AgentEvent::DaemonWatchdogTripped {
            session_id: "session-1".to_string(),
            attempts: 5,
            elapsed_ms: 12_000,
        },
        AgentEvent::LoopControlDecision {
            session_id: "session-1".to_string(),
            iteration: 6,
            action: "extend".to_string(),
            old_limit: 8,
            new_limit: 10,
            reason: "verification still running".to_string(),
            status: "working".to_string(),
        },
        AgentEvent::ToolFormatOverride {
            session_id: "session-1".to_string(),
            provider: "openrouter".to_string(),
            model: "qwen/qwen3-coder".to_string(),
            requested_format: "native".to_string(),
            recommended_format: "text".to_string(),
            catalog_parity: "native_unreliable".to_string(),
            override_reason: Some("cross-check provider regression".to_string()),
        },
        AgentEvent::ToolCallAudit {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "read_file".to_string(),
            audit: serde_json::json!({
                "summary": "Read project context",
                "consent": "not_required"
            }),
            receipt: Some(fixture_tool_call_receipt()),
        },
    ]
}

/// Pipeline-loop milestone events ride on the ACP `ExtNotification`
/// channel via `_harn/agentEvent`. The fixture pins the wire shape
/// per kind so any drift in field names (e.g. snake_case vs.
/// camelCase) or payload structure trips a build-time failure
/// rather than silently breaking downstream client decoders. Every kind
/// in the fixture must also appear in `HARN_AGENT_EVENT_KINDS` so
/// the capability advertisement stays honest.
#[tokio::test(flavor = "current_thread")]
async fn agent_event_ext_notification_fixtures_are_pinned() {
    let actual = collect_notifications(agent_event_ext_fixture_events()).await;
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/acp/agent_event_ext_notifications.json"
    ))
    .expect("fixture json");
    assert_eq!(serde_json::Value::Array(actual.clone()), expected);

    for notification in actual {
        assert_eq!(
            notification["method"].as_str().expect("method"),
            HARN_AGENT_EVENT_METHOD,
            "every pipeline-loop milestone notification must use the \
                 advertised _harn/agentEvent method"
        );
        assert!(
            notification["params"]["sessionId"].is_string(),
            "sessionId must be a top-level string on every agent event"
        );
        let kind = notification["params"]["kind"]
            .as_str()
            .expect("kind discriminator");
        assert!(
            HARN_AGENT_EVENT_KINDS.contains(&kind),
            "{kind} is not advertised in HARN_AGENT_EVENT_KINDS — clients \
                 cannot subscribe to undocumented kinds"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn protocol_conformance_agent_event_fixture_is_adapter_generated() {
    let actual = collect_notifications(agent_event_ext_fixture_events()).await;
    crate::protocol_fixture_tests::assert_fixture_documents_match(
        "conformance/protocols/fixtures/acp/agent_event_ext_notifications.valid.json",
        actual,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compass_routing_decision_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::CompassRoutingDecision {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-edit-1".to_string(),
        mode: "rewrite".to_string(),
        action: "rewritten".to_string(),
        persona: "fixer".to_string(),
        original_tool: "str_replace".to_string(),
        routed_tool: "edit_safe_text_patch".to_string(),
        target_tool: "edit_safe_text_patch".to_string(),
        path: Some("src/lib.rs".to_string()),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "compass_routing_decision");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["toolCallId"], "tool-edit-1");
    assert_eq!(params["mode"], "rewrite");
    assert_eq!(params["action"], "rewritten");
    assert_eq!(params["persona"], "fixer");
    assert_eq!(params["originalTool"], "str_replace");
    assert_eq!(params["routedTool"], "edit_safe_text_patch");
    assert_eq!(params["targetTool"], "edit_safe_text_patch");
    assert_eq!(params["path"], "src/lib.rs");
    assert!(
        HARN_AGENT_EVENT_KINDS.contains(&"compass_routing_decision"),
        "compass_routing_decision must be advertised so clients can subscribe"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feedback_injected_streak_reaches_acp_payload() {
    let actual = collect_notifications(vec![AgentEvent::FeedbackInjected {
        session_id: "session-1".to_string(),
        kind: "no_progress_streak_nudge".to_string(),
        content: "No progress last turn. Emit exactly one tool call now.".to_string(),
        streak: Some(2),
    }])
    .await;

    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0]["method"], HARN_AGENT_EVENT_METHOD);
    assert_eq!(actual[0]["params"]["kind"], "feedback_injected");
    assert_eq!(
        actual[0]["params"]["payload"],
        serde_json::json!({
            "feedbackKind": "no_progress_streak_nudge",
            "content": "No progress last turn. Emit exactly one tool call now.",
            "streak": 2,
        }),
    );
}

#[tokio::test(flavor = "current_thread")]
async fn step_judge_decision_agent_event_marks_skipped_reason() {
    let actual = collect_notifications(vec![AgentEvent::StepJudgeDecision {
        session_id: "session-1".to_string(),
        iteration: 1,
        verdict: "pass".to_string(),
        reasoning: String::new(),
        critique: String::new(),
        confidence: 1.0,
        judge_duration_ms: 0,
        vetoed: false,
        skipped: true,
        reason: Some("low_iteration_budget".to_string()),
        judge_error: false,
        on_veto: "replace".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
        provider: String::new(),
        model: String::new(),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "step_judge_decision");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["skipped"], true);
    assert_eq!(params["reason"], "low_iteration_budget");
    assert_eq!(params["vetoed"], false);
    // A genuine budget skip is NOT a swallowed judge error.
    assert_eq!(params["judgeError"], false);
}

#[tokio::test(flavor = "current_thread")]
async fn step_judge_decision_agent_event_surfaces_judge_unavailable() {
    // When the step-judge model errors and fail-open lets the turn through,
    // the decision must carry the distinct `judgeError` marker so a
    // fail-open swallow is observable, not indistinguishable from a real pass.
    let actual = collect_notifications(vec![AgentEvent::StepJudgeDecision {
        session_id: "session-1".to_string(),
        iteration: 1,
        verdict: "pass".to_string(),
        reasoning: "judge backend 503".to_string(),
        critique: String::new(),
        confidence: 0.0,
        judge_duration_ms: 0,
        vetoed: false,
        skipped: true,
        reason: Some("judge_unavailable".to_string()),
        judge_error: true,
        on_veto: "replace".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
        provider: String::new(),
        model: String::new(),
    }])
    .await;

    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "step_judge_decision");
    assert_eq!(params["verdict"], "pass");
    assert_eq!(params["reason"], "judge_unavailable");
    assert_eq!(params["judgeError"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn structural_validator_decision_agent_event_carries_retry_shape() {
    let actual = collect_notifications(vec![AgentEvent::StructuralValidatorDecision {
        session_id: "session-1".to_string(),
        iteration: 2,
        rule: "non_empty_when_writes_expected".to_string(),
        diagnostic: "Assistant emitted no tool calls while writable tools were available."
            .to_string(),
        recommended_action: "Emit the concrete write or edit tool call needed for the task."
            .to_string(),
        vetoed: true,
        skipped: false,
        reason: None,
        on_failure: "regenerate_with_feedback".to_string(),
        attempts: 1,
        max_attempts: 3,
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "structural_validator_decision");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["rule"], "non_empty_when_writes_expected");
    assert_eq!(params["onFailure"], "regenerate_with_feedback");
    assert_eq!(params["attempts"], 1);
    assert_eq!(params["maxAttempts"], 3);
    assert_eq!(params["vetoed"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn input_guardrail_verdict_agent_event_carries_tripwire_shape() {
    let actual = collect_notifications(vec![AgentEvent::InputGuardrailVerdict {
        session_id: "session-1".to_string(),
        iteration: 1,
        tripwire: true,
        reason: "private key exfiltration".to_string(),
        label: "secret_exfiltration".to_string(),
        confidence: 0.99,
        confidence_threshold: 0.8,
        classifier_kind: Some("custom".to_string()),
        model: None,
        error: None,
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "input_guardrail_verdict");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["tripwire"], true);
    assert_eq!(params["reason"], "private key exfiltration");
    assert_eq!(params["label"], "secret_exfiltration");
    assert_eq!(params["confidenceThreshold"], 0.8);
    assert_eq!(params["classifierKind"], "custom");
}

#[tokio::test(flavor = "current_thread")]
async fn tool_format_override_agent_event_uses_camel_case_fields() {
    let actual = collect_notifications(vec![AgentEvent::ToolFormatOverride {
        session_id: "session-1".to_string(),
        provider: "openrouter".to_string(),
        model: "qwen/qwen3-coder".to_string(),
        requested_format: "native".to_string(),
        recommended_format: "text".to_string(),
        catalog_parity: "native_unreliable".to_string(),
        override_reason: Some("cross-check provider regression".to_string()),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "tool_format_override");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["provider"], "openrouter");
    assert_eq!(params["model"], "qwen/qwen3-coder");
    assert_eq!(params["requestedFormat"], "native");
    assert_eq!(params["recommendedFormat"], "text");
    assert_eq!(params["catalogParity"], "native_unreliable");
    assert_eq!(params["overrideReason"], "cross-check provider regression");
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_progress_notification_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::McpNotification {
        session_id: "session-1".to_string(),
        server: "filesystem".to_string(),
        method: "notifications/progress".to_string(),
        direction: "notification".to_string(),
        params: serde_json::json!({
            "progressToken": "tok-1",
            "progress": 42.0,
            "total": 100.0,
            "server": "filesystem",
            "tool": "search_files"
        }),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "mcp_notification");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["server"], "filesystem");
    assert_eq!(params["method"], "notifications/progress");
    assert_eq!(params["direction"], "notification");
    assert_eq!(params["params"]["progress"], 42.0);
    assert_eq!(params["params"]["progressToken"], "tok-1");
    assert!(
        HARN_AGENT_EVENT_KINDS.contains(&"mcp_notification"),
        "mcp_notification must be advertised so clients can subscribe"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_elicitation_request_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::McpNotification {
        session_id: "session-1".to_string(),
        server: "deploy-bot".to_string(),
        method: "elicitation/create".to_string(),
        direction: "request".to_string(),
        params: serde_json::json!({
            "message": "Confirm production deploy?",
            "requestedSchema": {"type": "object"}
        }),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "mcp_notification");
    assert_eq!(params["direction"], "request");
    assert_eq!(params["method"], "elicitation/create");
    assert_eq!(params["params"]["message"], "Confirm production deploy?");
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_catalog_changed_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::McpCatalogChanged {
        session_id: "session-1".to_string(),
        server: Some("github".to_string()),
        reason: "list_changed".to_string(),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "mcp_catalog_changed");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["server"], "github");
    assert_eq!(params["reason"], "list_changed");
    assert!(
        HARN_AGENT_EVENT_KINDS.contains(&"mcp_catalog_changed"),
        "mcp_catalog_changed must be advertised so clients can subscribe"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_catalog_changed_allows_serverless_allowlist_update() {
    let actual = collect_notifications(vec![AgentEvent::McpCatalogChanged {
        session_id: "session-1".to_string(),
        server: None,
        reason: "allowlist_updated".to_string(),
    }])
    .await;

    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "mcp_catalog_changed");
    assert!(params["server"].is_null());
    assert_eq!(params["reason"], "allowlist_updated");
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_auth_required_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::McpAuthRequired {
        session_id: "session-1".to_string(),
        server: "notion".to_string(),
        resource: "https://mcp.notion.com".to_string(),
        scope: Some("read write".to_string()),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "mcp_auth_required");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["server"], "notion");
    assert_eq!(params["resource"], "https://mcp.notion.com");
    assert_eq!(params["scope"], "read write");
    assert!(
        HARN_AGENT_EVENT_KINDS.contains(&"mcp_auth_required"),
        "mcp_auth_required must be advertised so clients can subscribe"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_auth_required_omits_absent_scope() {
    let actual = collect_notifications(vec![AgentEvent::McpAuthRequired {
        session_id: "session-1".to_string(),
        server: "notion".to_string(),
        resource: "https://mcp.notion.com".to_string(),
        scope: None,
    }])
    .await;

    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "mcp_auth_required");
    assert!(params["scope"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn harn_extension_session_update_fixtures_are_pinned() {
    let actual = collect_notifications(extension_fixture_events()).await;
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/acp/session_update_extensions.json"
    ))
    .expect("fixture json");
    assert_eq!(serde_json::Value::Array(actual.clone()), expected);

    for notification in actual {
        let session_update = notification["params"]["update"]["sessionUpdate"]
            .as_str()
            .expect("sessionUpdate");
        assert!(
            HARN_SESSION_UPDATE_EXTENSIONS.contains(&session_update),
            "{session_update} is not advertised as a Harn ACP extension"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_session_update_keeps_payload_under_harn_meta() {
    let actual = collect_notifications(vec![AgentEvent::Artifact {
        session_id: "session-1".to_string(),
        artifact_id: "artifact-table-1".to_string(),
        kind: "table".to_string(),
        title: None,
        mime_type: "application/vnd.harn.table+json".to_string(),
        spec: serde_json::json!({
            "columns": ["name", "count"],
            "rows": [{"name": "a", "count": 2}]
        }),
        fallback: "name | count\na | 2".to_string(),
        size_bytes: 66,
        provenance: serde_json::json!({"fixture": true}),
        metadata: serde_json::json!({"scope": "test"}),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], "session/update");
    assert_eq!(notification["params"]["sessionId"], "session-1");
    let update = &notification["params"]["update"];
    assert_eq!(update["sessionUpdate"], "artifact");
    assert!(
        update.get("spec").is_none(),
        "artifact extension fields must not leak onto the ACP update root"
    );
    let harn_meta = update_harn_meta(notification);
    assert_eq!(harn_meta["artifactId"], "artifact-table-1");
    assert_eq!(harn_meta["kind"], "table");
    assert!(harn_meta["title"].is_null());
    assert_eq!(harn_meta["mimeType"], "application/vnd.harn.table+json");
    assert_eq!(harn_meta["spec"]["columns"][0], "name");
    assert_eq!(harn_meta["fallback"], "name | count\na | 2");
    assert_eq!(harn_meta["sizeBytes"], 66);
    assert_eq!(harn_meta["provenance"]["fixture"], true);
    assert_eq!(harn_meta["metadata"]["scope"], "test");
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_manifest_session_update_keeps_bundle_spec_under_harn_meta() {
    let spec = serde_json::json!({
        "schema_version": "harn.artifacts.v1",
        "kind": "artifact_manifest",
        "title": "Code findings report",
        "artifact_count": 2,
        "total_size_bytes": 42,
        "artifacts": [
            {
                "name": "findings.pdf",
                "relative_path": "artifacts/findings.pdf",
                "uri": "file:///tmp/findings.pdf",
                "mime_type": "application/pdf",
                "size_bytes": 40,
                "sha256": format!("sha256:{}", "a".repeat(64)),
            },
            {
                "name": "chart.png",
                "relative_path": "artifacts/chart.png",
                "uri": "file:///tmp/chart.png",
                "mime_type": "image/png",
                "size_bytes": 2,
                "sha256": format!("sha256:{}", "b".repeat(64)),
            },
        ],
        "metadata": {
            "contract_package": "@harn/documents",
            "contract_version": "0.1.3",
        },
    });
    let actual = collect_notifications(vec![AgentEvent::Artifact {
        session_id: "session-1".to_string(),
        artifact_id: "artifact-manifest-1".to_string(),
        kind: "artifact_manifest".to_string(),
        title: Some("Code findings report".to_string()),
        mime_type: "application/vnd.harn.artifact-manifest+json".to_string(),
        spec,
        fallback: "Code findings report: findings.pdf, chart.png".to_string(),
        size_bytes: 512,
        provenance: serde_json::json!({"generator": "artifact_emit"}),
        metadata: serde_json::json!({"scope": "bundle"}),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], "session/update");
    assert_eq!(notification["params"]["sessionId"], "session-1");
    let update = &notification["params"]["update"];
    assert_eq!(update["sessionUpdate"], "artifact");
    assert!(
        update.get("spec").is_none(),
        "artifact extension fields must not leak onto the ACP update root"
    );
    let harn_meta = update_harn_meta(notification);
    assert_eq!(harn_meta["artifactId"], "artifact-manifest-1");
    assert_eq!(harn_meta["kind"], "artifact_manifest");
    assert_eq!(harn_meta["title"], "Code findings report");
    assert_eq!(
        harn_meta["mimeType"],
        "application/vnd.harn.artifact-manifest+json"
    );
    assert_eq!(harn_meta["spec"]["schema_version"], "harn.artifacts.v1");
    assert_eq!(harn_meta["spec"]["kind"], "artifact_manifest");
    assert_eq!(harn_meta["spec"]["artifact_count"], 2);
    assert_eq!(
        harn_meta["spec"]["artifacts"][0]["mime_type"],
        "application/pdf"
    );
    assert_eq!(harn_meta["spec"]["artifacts"][1]["mime_type"], "image/png");
    assert_eq!(
        harn_meta["fallback"],
        "Code findings report: findings.pdf, chart.png"
    );
    assert_eq!(harn_meta["sizeBytes"], 512);
    assert_eq!(harn_meta["provenance"]["generator"], "artifact_emit");
    assert_eq!(harn_meta["metadata"]["scope"], "bundle");
}

#[tokio::test(flavor = "current_thread")]
async fn structured_plan_extension_fixture_is_pinned() {
    let plan = harn_vm::llm::plan::normalize_plan_tool_call(
        harn_vm::llm::plan::EMIT_PLAN_TOOL,
        &serde_json::json!({
            "summary": "Ship plan events.",
            "steps": [
                {"content": "Emit plan event.", "status": "completed"},
                {"content": "Verify fixtures.", "status": "pending"}
            ],
            "verification_commands": ["cargo test -p harn-serve acp"],
        }),
    );
    let actual = collect_notifications(vec![AgentEvent::Plan {
        session_id: "session-1".to_string(),
        plan,
    }])
    .await;
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/acp/session_update_plan_extension.json"
    ))
    .expect("fixture json");
    assert_eq!(serde_json::Value::Array(actual), expected);
}

#[tokio::test(flavor = "current_thread")]
async fn worker_update_serializes_to_session_update_with_lifecycle_metadata() {
    // Every typed `WorkerEvent` must round-trip onto the ACP
    // `session/update` stream as a `worker_update` entry. The
    // adapter pins a stable wire shape: status string, event
    // discriminator, terminal hint, plus the structured metadata
    // and audit fields hosts render without re-parsing.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    let cases = [
        (
            harn_vm::agent_events::WorkerEvent::WorkerSpawned,
            "running",
            false,
        ),
        (
            harn_vm::agent_events::WorkerEvent::WorkerProgressed,
            "progressed",
            false,
        ),
        (
            harn_vm::agent_events::WorkerEvent::WorkerWaitingForInput,
            "awaiting_input",
            false,
        ),
        (
            harn_vm::agent_events::WorkerEvent::WorkerCompleted,
            "completed",
            true,
        ),
        (
            harn_vm::agent_events::WorkerEvent::WorkerFailed,
            "failed",
            true,
        ),
        (
            harn_vm::agent_events::WorkerEvent::WorkerCancelled,
            "cancelled",
            true,
        ),
    ];

    for (worker_event, status, terminal) in cases {
        sink.handle_event(&AgentEvent::WorkerUpdate {
            session_id: "session-1".into(),
            worker_id: "worker-1".into(),
            worker_name: "review".into(),
            worker_task: "review pr".into(),
            worker_mode: "delegated_stage".into(),
            event: worker_event,
            status: worker_event.as_status().to_string(),
            metadata: serde_json::json!({
                "child_run_id": "run_x",
                "child_run_path": ".harn-runs/run_x",
            }),
            audit: Some(serde_json::json!({"run_id": "run_x"})),
        });
        let line = rx.recv().await.expect("acp worker_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(payload["method"], "session/update");
        assert_eq!(payload["params"]["sessionId"], "session-1");
        let update = &payload["params"]["update"];
        assert_eq!(update["sessionUpdate"], "worker_update");
        // Vendor-extension fields ride under `_meta.harn` per harn#905.
        let harn_meta = update_harn_meta(&payload);
        assert_eq!(harn_meta["workerId"], "worker-1");
        assert_eq!(harn_meta["workerName"], "review");
        assert_eq!(harn_meta["workerTask"], "review pr");
        assert_eq!(harn_meta["workerMode"], "delegated_stage");
        assert_eq!(harn_meta["event"], worker_event.as_str());
        assert_eq!(harn_meta["status"], status);
        assert_eq!(harn_meta["terminal"], terminal);
        assert_eq!(harn_meta["metadata"]["child_run_id"], "run_x");
        assert_eq!(harn_meta["audit"]["run_id"], "run_x");
        assert!(update.get("workerId").is_none());
        assert!(update.get("audit").is_none());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn worker_update_omits_audit_when_absent() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::WorkerUpdate {
        session_id: "session-1".into(),
        worker_id: "w".into(),
        worker_name: "n".into(),
        worker_task: "t".into(),
        worker_mode: "delegated_stage".into(),
        event: harn_vm::agent_events::WorkerEvent::WorkerSpawned,
        status: "running".into(),
        metadata: serde_json::json!({}),
        audit: None,
    });
    let line = rx.recv().await.expect("acp worker_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let harn_meta = update_harn_meta(&payload);
    assert!(harn_meta.get("audit").is_none());
    assert!(payload["params"]["update"].get("audit").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn handoff_event_serializes_as_session_update() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&harn_vm::agent_events::AgentEvent::Handoff {
        session_id: "session-1".to_string(),
        artifact_id: "artifact-1".to_string(),
        handoff: Box::new(
            HandoffArtifact {
                id: "handoff-1".to_string(),
                source_persona: "merge_captain".to_string(),
                target_persona_or_human: HandoffTargetRecord {
                    kind: "persona".to_string(),
                    id: Some("review_captain".to_string()),
                    label: Some("review_captain".to_string()),
                    uri: None,
                },
                task: "Review the patch".to_string(),
                reason: "Merge queue requires review".to_string(),
                ..Default::default()
            }
            .normalize(),
        ),
    });
    let line = rx.recv().await.expect("acp handoff notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(payload["method"], "session/update");
    assert_eq!(payload["params"]["update"]["sessionUpdate"], "handoff");
    // Vendor-extension fields ride under `_meta.harn` per harn#905.
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["handoffId"], "handoff-1");
    assert_eq!(
        harn_meta["handoff"]["target_persona_or_human"]["label"],
        "review_captain"
    );
    assert!(payload["params"]["update"].get("handoffId").is_none());
    assert!(payload["params"]["update"].get("handoff").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn forwarded_agent_events_serialize_as_session_updates() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    let handoff = HandoffArtifact {
        id: "handoff-1".to_string(),
        source_persona: "merge_captain".to_string(),
        target_persona_or_human: HandoffTargetRecord {
            kind: "persona".to_string(),
            id: Some("review_captain".to_string()),
            label: Some("review_captain".to_string()),
            uri: None,
        },
        task: "Review the patch".to_string(),
        reason: "Merge queue requires review".to_string(),
        ..Default::default()
    }
    .normalize();

    let events = vec![
        AgentEvent::AgentMessageChunk {
            session_id: "session-1".to_string(),
            content: "hello".to_string(),
        },
        AgentEvent::AgentThoughtChunk {
            session_id: "session-1".to_string(),
            content: "thinking".to_string(),
        },
        AgentEvent::ToolCall {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "read".to_string(),
            kind: Some(ToolKind::Read),
            status: ToolCallStatus::Pending,
            raw_input: serde_json::json!({"path": "README.md"}),
            parsing: None,
            audit: None,
        },
        AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "read".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: Some(serde_json::json!({"ok": true})),
            error: None,
            duration_ms: Some(7),
            execution_duration_ms: Some(5),
            error_category: None,
            executor: Some(ToolExecutor::HarnBuiltin),
            parsing: None,

            raw_input: None,
            raw_input_partial: None,
            audit: None,
        },
        AgentEvent::Plan {
            session_id: "session-1".to_string(),
            plan: serde_json::json!([{"step": "edit", "status": "pending"}]),
        },
        AgentEvent::SkillActivated {
            session_id: "session-1".to_string(),
            skill_name: "rust".to_string(),
            iteration: 1,
            reason: "matched".to_string(),
        },
        AgentEvent::SkillDeactivated {
            session_id: "session-1".to_string(),
            skill_name: "rust".to_string(),
            iteration: 2,
        },
        AgentEvent::SkillScopeTools {
            session_id: "session-1".to_string(),
            skill_name: "rust".to_string(),
            allowed_tools: vec!["read".to_string()],
        },
        AgentEvent::SkillNarrow {
            session_id: "session-1".to_string(),
            reason: "unused across 5 turns".to_string(),
            removed_tools: vec!["write".to_string()],
            remaining_tools: vec!["read".to_string()],
            policy: serde_json::Value::Null,
            removed_tool_details: serde_json::Value::Null,
            kept_tool_details: serde_json::Value::Null,
        },
        AgentEvent::StanceTransition {
            session_id: "session-1".to_string(),
            phase: "write_access_granted".to_string(),
            escape_tool: "request_write_access".to_string(),
            allowed_tools: vec![
                "look".to_string(),
                "search".to_string(),
                "request_write_access".to_string(),
            ],
            justification: "User asked me to make the change.".to_string(),
            consent: "express".to_string(),
            reason: "The user explicitly asked for the edit.".to_string(),
        },
        AgentEvent::ToolSearchQuery {
            session_id: "session-1".to_string(),
            tool_use_id: "search-1".to_string(),
            name: "tool_search".to_string(),
            query: serde_json::json!({"q": "read"}),
            strategy: "semantic".to_string(),
            mode: "client".to_string(),
        },
        AgentEvent::ToolSearchResult {
            session_id: "session-1".to_string(),
            tool_use_id: "search-1".to_string(),
            promoted: vec!["read".to_string()],
            strategy: "semantic".to_string(),
            mode: "client".to_string(),
        },
        AgentEvent::TranscriptCompacted {
            session_id: "session-1".to_string(),
            mode: "auto".to_string(),
            reason: "threshold".to_string(),
            strategy: "summary".to_string(),
            archived_messages: 3,
            estimated_tokens_before: 100,
            estimated_tokens_after: 40,
            snapshot_asset_id: Some("asset-1".to_string()),
            instruction_mode: None,
            instruction_source: None,
            compaction_policy: None,
        },
        AgentEvent::TranscriptProjected {
            session_id: "session-1".to_string(),
            policy: "clean_tool_repair".to_string(),
            reason: "tool_call_repair_squashed".to_string(),
            prefix_hash: "sha256:abc".to_string(),
            kept_count: 3,
            dropped_count: 2,
            provider_safety_blocked: false,
            redacted_count: 0,
            reclaimed_tokens: 0,
            roots_consulted: Vec::new(),
            redaction_pointers: Vec::new(),
        },
        AgentEvent::Handoff {
            session_id: "session-1".to_string(),
            artifact_id: "artifact-1".to_string(),
            handoff: Box::new(handoff),
        },
        AgentEvent::FsWatch {
            session_id: "session-1".to_string(),
            subscription_id: "fsw-1".to_string(),
            events: vec![FsWatchEvent {
                kind: "modify".to_string(),
                paths: vec!["/tmp/project/src/lib.rs".to_string()],
                relative_paths: vec!["src/lib.rs".to_string()],
                raw_kind: "Modify(Any)".to_string(),
                error: None,
            }],
        },
    ];
    let expected_updates = [
        "agent_message_chunk",
        "agent_thought_chunk",
        "tool_call",
        "tool_call_update",
        "plan",
        "skill_activated",
        "skill_deactivated",
        "skill_scope_tools",
        "skill_narrow",
        "stance_transition",
        "tool_search_query",
        "tool_search_result",
        "transcript_compacted",
        "transcript_projected",
        "handoff",
        "fs_watch",
    ];

    for event in &events {
        sink.handle_event(event);
    }

    for expected in expected_updates {
        let line = rx.recv().await.expect("ACP event notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(payload["method"], "session/update");
        assert_eq!(payload["params"]["sessionId"], "session-1");
        assert_eq!(payload["params"]["update"]["sessionUpdate"], expected);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn skill_narrow_serializes_policy_details_under_harn_meta() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::SkillNarrow {
        session_id: "session-1".to_string(),
        reason: "unused across 5 turns".to_string(),
        removed_tools: vec!["read_docs".to_string()],
        remaining_tools: vec!["run".to_string()],
        policy: serde_json::json!({"mode": "safe", "prune_classes": ["read_only"]}),
        removed_tool_details: serde_json::json!([
            {"name": "read_docs", "class": "read_only", "reason": "unused_prunable_class"}
        ]),
        kept_tool_details: serde_json::json!([
            {"name": "run", "class": "mutating", "reason": "class_kept"}
        ]),
    });

    let line = rx.recv().await.expect("ACP event notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["policy"]["mode"], "safe");
    assert_eq!(harn_meta["removedToolDetails"][0]["name"], "read_docs");
    assert_eq!(harn_meta["keptToolDetails"][0]["class"], "mutating");
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_serializes_error_category_in_camel_case() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-7".to_string(),
        tool_name: "read".to_string(),
        status: ToolCallStatus::Failed,
        raw_output: None,
        error: Some("missing required arg `path`".to_string()),
        duration_ms: None,
        execution_duration_ms: None,
        error_category: Some(ToolCallErrorCategory::SchemaValidation),
        executor: None,
        parsing: None,

        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(
        payload["params"]["update"]["sessionUpdate"],
        "tool_call_update"
    );
    assert_eq!(payload["params"]["update"]["status"], "failed");
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["errorCategory"], "schema_validation");
    assert_eq!(harn_meta["error"], "missing required arg `path`");
    assert!(payload["params"]["update"].get("errorCategory").is_none());
    assert!(payload["params"]["update"].get("error").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_omits_error_category_when_none() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-7".to_string(),
        tool_name: "read".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"ok": true})),
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        executor: None,
        parsing: None,

        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(payload["params"]["update"].get("_meta").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_carries_parsing_flag_through_to_acp_wire() {
    // Harn#692/#904: candidate parser state is Harn metadata on
    // the ACP wire so clients can render the in-flight chip without
    // extending the root ACP tool-call shape.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "session-1".to_string(),
        tool_call_id: "text-cand-1".to_string(),
        tool_name: "edit".to_string(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({}),
        parsing: Some(true),
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(payload["params"]["update"]["sessionUpdate"], "tool_call");
    assert_eq!(update_harn_meta(&payload)["parsing"], true);
    assert!(payload["params"]["update"].get("parsing").is_none());

    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "text-cand-1".to_string(),
        tool_name: "edit".to_string(),
        status: ToolCallStatus::Failed,
        raw_output: None,
        error: Some("malformed args".to_string()),
        duration_ms: None,
        execution_duration_ms: None,
        error_category: Some(ToolCallErrorCategory::ParseAborted),
        executor: None,
        parsing: Some(false),

        raw_input: None,

        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(
        payload["params"]["update"]["sessionUpdate"],
        "tool_call_update"
    );
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["parsing"], false);
    assert_eq!(harn_meta["errorCategory"], "parse_aborted");
    assert!(payload["params"]["update"].get("parsing").is_none());
    assert!(payload["params"]["update"].get("errorCategory").is_none());

    // Default `parsing: None` must not surface Harn metadata at all.
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "read".to_string(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({}),
        parsing: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(payload["params"]["update"].get("_meta").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_serializes_executor_per_acp_wire_format() {
    // Harn#691/#904: clients render badges off Harn executor metadata.
    // The wire shape must distinguish bare-string variants from the
    // McpServer object-with-serverName form so a UI can branch on
    // `typeof executor === "string"`.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    let cases = [
        (ToolExecutor::HarnBuiltin, serde_json::json!("harn_builtin")),
        (ToolExecutor::HostBridge, serde_json::json!("host_bridge")),
        (
            ToolExecutor::McpServer {
                server_name: "linear".into(),
            },
            serde_json::json!({"kind": "mcp_server", "serverName": "linear"}),
        ),
        (
            ToolExecutor::ProviderNative,
            serde_json::json!("provider_native"),
        ),
    ];

    for (executor, expected) in cases {
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "demo".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: Some(executor),
            parsing: None,

            raw_input: None,
            raw_input_partial: None,
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            payload["params"]["update"]["sessionUpdate"],
            "tool_call_update"
        );
        assert_eq!(update_harn_meta(&payload)["executor"], expected);
        assert!(payload["params"]["update"].get("executor").is_none());
    }

    // `executor: None` must not surface Harn metadata.
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-2".to_string(),
        tool_name: "demo".to_string(),
        status: ToolCallStatus::InProgress,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        executor: None,
        parsing: None,

        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(payload["params"]["update"].get("_meta").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_streams_raw_input_and_raw_input_partial_per_acp_wire_format() {
    // #693/#904: parsed raw input remains canonical `rawInput`;
    // unparseable raw bytes are Harn metadata under `_meta.harn`.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    // Parsed partial value → `rawInput` populated, `rawInputPartial` absent.
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-streaming".to_string(),
        tool_name: "search".to_string(),
        status: ToolCallStatus::Pending,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        executor: None,
        raw_input: Some(serde_json::json!({"q": "hello"})),
        raw_input_partial: None,
        audit: None,

        parsing: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(payload["params"]["update"]["rawInput"]["q"], "hello");
    assert!(payload["params"]["update"].get("_meta").is_none());

    // Unparseable partial bytes → `rawInputPartial` populated, `rawInput` absent.
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-streaming".to_string(),
        tool_name: "search".to_string(),
        status: ToolCallStatus::Pending,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: Some(r#"{"q":"hel"#.to_string()),
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(payload["params"]["update"].get("rawInput").is_none());
    assert_eq!(
        update_harn_meta(&payload)["rawInputPartial"],
        r#"{"q":"hel"#
    );
    assert!(payload["params"]["update"].get("rawInputPartial").is_none());

    // Terminal updates (None / None) must not introduce these keys.
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-streaming".to_string(),
        tool_name: "search".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"ok": true})),
        error: None,
        duration_ms: Some(12),
        execution_duration_ms: Some(8),
        error_category: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(payload["params"]["update"].get("rawInput").is_none());
    assert!(update_harn_meta(&payload).get("rawInputPartial").is_none());
    assert_eq!(payload["params"]["update"]["status"], "completed");
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_includes_audit_when_mutation_session_is_active() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    let policy = ToolApprovalPolicy {
        require_approval: vec!["edit_*".into()],
        write_path_allowlist: vec!["src/**".into()],
        ..Default::default()
    };
    let audit = MutationSessionRecord {
        session_id: "session_42".into(),
        parent_session_id: Some("session_root".into()),
        run_id: Some("run_42".into()),
        worker_id: Some("worker_3".into()),
        execution_kind: Some("worker".into()),
        mutation_scope: "apply_workspace".into(),
        approval_policy: Some(policy),
    };
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "edit_file".to_string(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({"path": "src/main.rs"}),
        parsing: None,
        audit: Some(audit),
    });
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let audit_value = &update_harn_meta(&payload)["audit"];
    assert_eq!(audit_value["session_id"], "session_42");
    assert_eq!(audit_value["parent_session_id"], "session_root");
    assert_eq!(audit_value["run_id"], "run_42");
    assert_eq!(audit_value["worker_id"], "worker_3");
    assert_eq!(audit_value["execution_kind"], "worker");
    assert_eq!(audit_value["mutation_scope"], "apply_workspace");
    assert_eq!(
        audit_value["approval_policy"]["require_approval"][0],
        "edit_*"
    );
    assert_eq!(
        audit_value["approval_policy"]["write_path_allowlist"][0],
        "src/**"
    );
    assert!(payload["params"]["update"].get("audit").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_omits_audit_when_no_mutation_session() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "read".to_string(),
        kind: Some(ToolKind::Read),
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({"path": "README.md"}),
        parsing: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(
        payload["params"]["update"].get("_meta").is_none(),
        "got: {payload}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_includes_audit_when_mutation_session_is_active() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    let audit = MutationSessionRecord {
        session_id: "session_42".into(),
        run_id: Some("run_42".into()),
        mutation_scope: "apply_workspace".into(),
        execution_kind: Some("workflow".into()),
        ..Default::default()
    };
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "edit_file".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"text": "ok"})),
        error: None,
        duration_ms: Some(11),
        execution_duration_ms: Some(7),
        error_category: None,
        executor: Some(ToolExecutor::HostBridge),
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: Some(audit),
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let update = &payload["params"]["update"];
    assert_eq!(update["sessionUpdate"], "tool_call_update");
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["audit"]["session_id"], "session_42");
    assert_eq!(harn_meta["audit"]["run_id"], "run_42");
    assert_eq!(harn_meta["audit"]["mutation_scope"], "apply_workspace");
    assert_eq!(harn_meta["audit"]["execution_kind"], "workflow");
    assert_eq!(harn_meta["executor"], "host_bridge");
    assert_eq!(harn_meta["durationMs"], 11);
    assert_eq!(harn_meta["executionDurationMs"], 7);
    assert!(update.get("audit").is_none());
    assert!(update.get("executor").is_none());
    assert!(update.get("durationMs").is_none());
    assert!(update.get("executionDurationMs").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_omits_audit_when_no_mutation_session() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "read".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(
        payload["params"]["update"].get("_meta").is_none(),
        "got: {payload}"
    );
}

/// harn#905 conformance: vendor-extension session-update fields
/// must travel under `update._meta.harn` and **must not** appear at
/// the update root. Canonical ACP fields (`sessionUpdate`, `content`,
/// etc.) stay at their canonical locations. This test pins the
/// contract field-by-field for every `HARN_SESSION_UPDATE_EXTENSIONS`
/// variant the adapter emits so a regression in any one variant
/// fails this single test.
#[tokio::test(flavor = "current_thread")]
async fn vendor_extension_session_update_fields_live_under_meta_harn() {
    let actual = collect_notifications(extension_fixture_events()).await;

    let expectations: &[(&str, &[&str])] = &[
        (
            "artifact",
            &[
                "artifactId",
                "kind",
                "title",
                "mimeType",
                "spec",
                "fallback",
                "sizeBytes",
                "provenance",
                "metadata",
            ],
        ),
        ("skill_activated", &["skillName", "iteration", "reason"]),
        ("skill_deactivated", &["skillName", "iteration"]),
        ("skill_scope_tools", &["skillName", "allowedTools"]),
        (
            "skill_narrow",
            &["reason", "removedTools", "remainingTools"],
        ),
        (
            "stance_transition",
            &[
                "phase",
                "escapeTool",
                "allowedTools",
                "justification",
                "consent",
                "reason",
            ],
        ),
        (
            "tool_search_query",
            &["toolUseId", "name", "query", "strategy", "mode"],
        ),
        (
            "tool_search_result",
            &["toolUseId", "promoted", "strategy", "mode"],
        ),
        (
            "transcript_compacted",
            &[
                "mode",
                "strategy",
                "archivedMessages",
                "estimatedTokensBefore",
                "estimatedTokensAfter",
                "snapshotAssetId",
                "instructionMode",
                "instructionSource",
                "compactionPolicy",
            ],
        ),
        (
            "transcript_projected",
            &[
                "policy",
                "reason",
                "prefixHash",
                "keptCount",
                "droppedCount",
                "providerSafetyBlocked",
            ],
        ),
        ("reminder_emitted", &["reminder"]),
        ("handoff", &["handoffId", "artifactId", "handoff"]),
        ("fs_watch", &["subscriptionId", "events"]),
        (
            "worker_update",
            &[
                "workerId",
                "workerName",
                "workerTask",
                "workerMode",
                "event",
                "status",
                "terminal",
                "metadata",
                "audit",
            ],
        ),
        ("hitl_request", &["requestId", "kind", "payload"]),
        ("hitl_resolved", &["requestId", "kind", "outcome"]),
    ];

    assert_eq!(
        actual.len(),
        expectations.len(),
        "fixture event count must match expectations table"
    );

    for (notification, (variant, vendor_fields)) in actual.iter().zip(expectations.iter()) {
        let update = &notification["params"]["update"];
        assert_eq!(
            update["sessionUpdate"], *variant,
            "update[{variant}] must keep canonical sessionUpdate at the root"
        );
        let harn_meta = &update["_meta"]["harn"];
        assert!(
            harn_meta.is_object(),
            "update[{variant}] must carry _meta.harn object, got: {update}"
        );
        for field in *vendor_fields {
            assert!(
                harn_meta.get(field).is_some(),
                "update[{variant}]._meta.harn must contain `{field}`, got: {harn_meta}"
            );
            assert!(
                update.get(field).is_none(),
                "update[{variant}].`{field}` must not be emitted at the root, got: {update}"
            );
        }
        // No vendor field other than `_meta` and `sessionUpdate`
        // should be present at the update root.
        let update_obj = update.as_object().expect("update is object");
        for key in update_obj.keys() {
            assert!(
                    matches!(key.as_str(), "sessionUpdate" | "_meta"),
                    "update[{variant}] must not carry root key `{key}` (vendor extension); got: {update}"
                );
        }
    }
}

/// harn#905 conformance: `progress` and `log` are emitted by
/// `AcpBridge` (not `AcpAgentEventSink`), so cover them with a
/// dedicated bridge-side test. Both variants are entirely
/// vendor — every field other than `sessionUpdate` itself must
/// land under `_meta.harn`.
#[tokio::test(flavor = "current_thread")]
async fn bridge_progress_and_log_session_updates_namespace_vendor_fields() {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let bridge = Arc::new(super::super::AcpBridge {
                session_id: "session-1".to_string(),
                output: AcpOutput::Channel(tx),
                pending: Arc::new(TokioMutex::new(HashMap::new())),
                next_id_counter: AtomicU64::new(1),
                cancellation: super::super::SessionCancellation::default(),
                script_name: std::sync::Mutex::new(String::new()),
                assistant_state: std::sync::Mutex::new(
                    harn_vm::visible_text::VisibleTextState::default(),
                ),
            });

            bridge.send_progress(
                "ingest",
                "loading",
                Some(3),
                Some(10),
                Some(serde_json::json!({"item": "row-7"})),
            );
            let line = rx.recv().await.expect("progress notification");
            let payload: serde_json::Value = serde_json::from_str(&line).expect("progress json");
            let update = &payload["params"]["update"];
            assert_eq!(update["sessionUpdate"], "progress");
            let harn_meta = &update["_meta"]["harn"];
            assert_eq!(harn_meta["phase"], "ingest");
            assert_eq!(harn_meta["message"], "loading");
            assert_eq!(harn_meta["progress"], 3);
            assert_eq!(harn_meta["total"], 10);
            assert_eq!(harn_meta["data"]["item"], "row-7");
            for forbidden in ["phase", "message", "progress", "total", "data"] {
                assert!(
                    update.get(forbidden).is_none(),
                    "progress.{forbidden} must live under _meta.harn, got: {update}"
                );
            }

            bridge.send_log(
                "warn",
                "deprecated builtin: foo",
                Some(serde_json::json!({"builtin": "foo"})),
            );
            let line = rx.recv().await.expect("log notification");
            let payload: serde_json::Value = serde_json::from_str(&line).expect("log json");
            let update = &payload["params"]["update"];
            assert_eq!(update["sessionUpdate"], "log");
            let harn_meta = &update["_meta"]["harn"];
            assert_eq!(harn_meta["level"], "warn");
            assert_eq!(harn_meta["message"], "deprecated builtin: foo");
            assert_eq!(harn_meta["fields"]["builtin"], "foo");
            for forbidden in ["level", "message", "fields"] {
                assert!(
                    update.get(forbidden).is_none(),
                    "log.{forbidden} must live under _meta.harn, got: {update}"
                );
            }

            // Optional fields are simply absent under `_meta.harn`,
            // not promoted back to the root.
            bridge.send_progress("ingest", "starting", None, None, None);
            let line = rx.recv().await.expect("minimal progress notification");
            let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
            let update = &payload["params"]["update"];
            let harn_meta = &update["_meta"]["harn"];
            assert!(harn_meta.get("progress").is_none());
            assert!(harn_meta.get("total").is_none());
            assert!(harn_meta.get("data").is_none());
            assert!(update.get("progress").is_none());

            bridge.send_plan(serde_json::json!([
                {"content": "Implement progress helper.", "status": "completed", "priority": "high"},
                {"content": "Run conformance.", "status": "pending"}
            ]));
            let line = rx.recv().await.expect("plan notification");
            let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
            let update = &payload["params"]["update"];
            assert_eq!(update["sessionUpdate"], "plan");
            assert_eq!(
                update["entries"],
                serde_json::json!([
                    {"content": "Implement progress helper.", "status": "completed", "priority": "high"},
                    {"content": "Run conformance.", "status": "pending"}
                ])
            );
            assert!(
                update.get("_meta").is_none(),
                "canonical plan updates must not carry Harn extension metadata: {update}"
            );
        })
        .await;
}

/// harn#905 conformance: `agent_message_chunk` is canonical, so the
/// content block and its `text` field stay at the canonical
/// location; only the harn-specific `visible_text` /
/// `visible_delta` content extensions move under `content._meta.harn`.
#[tokio::test(flavor = "current_thread")]
async fn agent_message_chunk_visible_text_lives_under_content_meta_harn() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::AgentMessageChunk {
        session_id: "session-1".to_string(),
        content: "hello".to_string(),
    });
    let line = rx.recv().await.expect("agent_message_chunk notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let content = &payload["params"]["update"]["content"];
    assert_eq!(content["type"], "text");
    assert_eq!(content["text"], "hello");
    assert_eq!(content["_meta"]["harn"]["visible_text"], "hello");
    assert_eq!(content["_meta"]["harn"]["visible_delta"], "hello");
    assert!(content.get("visible_text").is_none());
    assert!(content.get("visible_delta").is_none());
}

/// Pipeline-loop milestones used to be silently dropped by the ACP
/// adapter (no canonical `session/update` slot). They now ride on
/// the `_harn/agentEvent` `ExtNotification` channel — never on
/// `session/update`. This test pins the negative half of that
/// contract: even though the events are surfaced, they MUST NOT
/// pollute the canonical `session/update` stream that strict ACP
/// clients consume by closed enum.
#[test]
fn internal_agent_events_never_emit_session_updates() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    sink.handle_event(&AgentEvent::IterationStart {
        session_id: "session-1".to_string(),
        iteration: 1,
        provider: String::new(),
        model: String::new(),
    });
    sink.handle_event(&AgentEvent::BudgetExhausted {
        session_id: "session-1".to_string(),
        max_iterations: 3,
        kind: None,
        cost_usd: None,
        wall_clock_ms: None,
    });
    sink.handle_event(&AgentEvent::BudgetCircuitBreaker {
        session_id: "session-1".to_string(),
        kind: "consecutive_failures".to_string(),
        consecutive_count: 2,
        paused_for_ms: 0,
    });
    sink.handle_event(&AgentEvent::IterationEnd {
        session_id: "session-1".to_string(),
        iteration: 1,
        iteration_info: serde_json::json!({}),
    });
    sink.handle_event(&AgentEvent::FeedbackInjected {
        session_id: "session-1".to_string(),
        kind: "user".to_string(),
        content: "continue".to_string(),
        streak: None,
    });
    sink.handle_event(&AgentEvent::LoopStuck {
        session_id: "session-1".to_string(),
        max_nudges: 2,
        last_iteration: 4,
        tail_excerpt: "tail".to_string(),
    });
    sink.handle_event(&AgentEvent::LoopStuckSignal {
        session_id: "session-1".to_string(),
        payload: serde_json::json!({
            "schema": "burin.stuck_handoff.v1",
            "action": "handoff",
            "terminal": true,
            "pattern": "no_progress_terminator",
        }),
    });
    sink.handle_event(&AgentEvent::DaemonWatchdogTripped {
        session_id: "session-1".to_string(),
        attempts: 3,
        elapsed_ms: 10,
    });

    let mut count = 0;
    while let Ok(line) = rx.try_recv() {
        count += 1;
        let payload: serde_json::Value = serde_json::from_str(&line).expect("notification json");
        assert_ne!(
            payload["method"], "session/update",
            "pipeline-loop milestones must NOT ride on session/update — \
                 strict ACP clients use a closed enum and would reject any \
                 vendor-invented sessionUpdate kind. Got: {payload}"
        );
        assert_eq!(
            payload["method"], HARN_AGENT_EVENT_METHOD,
            "pipeline-loop milestones MUST ride on the advertised \
                 _harn/agentEvent ExtNotification method"
        );
    }
    assert_eq!(
        count, 8,
        "expected one ExtNotification per fed AgentEvent, got {count}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn loop_stuck_signal_forwards_pipeline_payload() {
    let actual = collect_notifications(vec![AgentEvent::LoopStuckSignal {
        session_id: "session-1".to_string(),
        payload: serde_json::json!({
            "schema": "burin.stuck_handoff.v1",
            "action": "handoff",
            "terminal": true,
            "pattern": "no_progress_terminator",
            "message": "I am stuck after repeated verification failures.",
        }),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "loop_stuck");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["schema"], "burin.stuck_handoff.v1");
    assert_eq!(params["action"], "handoff");
    assert_eq!(params["terminal"], true);
    assert_eq!(params["pattern"], "no_progress_terminator");
    assert_eq!(
        params["message"],
        "I am stuck after repeated verification failures."
    );
}

#[test]
fn progress_reported_entries_emit_canonical_acp_plan_update() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    let entries = serde_json::json!([
        {"content": "Implement progress helper.", "status": "completed", "priority": "high"},
        {"content": "Run conformance.", "status": "pending"}
    ]);

    sink.handle_event(&AgentEvent::ProgressReported {
        session_id: "session-1".to_string(),
        message: Some("Patched stdlib API.".to_string()),
        entries: entries.clone(),
        replace: true,
        metadata: serde_json::json!({"source": "agent_progress"}),
    });

    let line = rx.try_recv().expect("ACP plan notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("notification json");
    assert_eq!(payload["method"], "session/update");
    assert_eq!(payload["params"]["sessionId"], "session-1");
    let update = &payload["params"]["update"];
    assert_eq!(update["sessionUpdate"], "plan");
    assert_eq!(update["entries"], entries);
    for forbidden in ["_meta", "message", "metadata", "replace"] {
        assert!(
            update.get(forbidden).is_none(),
            "agent_progress entries must emit canonical ACP plan only; got {update}"
        );
    }
    assert!(
        rx.try_recv().is_err(),
        "agent_progress entries must emit exactly one ACP notification"
    );
}

#[test]
fn progress_reported_message_only_uses_harn_progress_extension() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    sink.handle_event(&AgentEvent::ProgressReported {
        session_id: "session-1".to_string(),
        message: Some("Working through verification.".to_string()),
        entries: serde_json::json!([]),
        replace: false,
        metadata: serde_json::json!({"source": "agent_progress"}),
    });

    let line = rx.try_recv().expect("ACP progress notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("notification json");
    assert_eq!(payload["method"], "session/update");
    assert_eq!(payload["params"]["sessionId"], "session-1");
    let update = &payload["params"]["update"];
    assert_eq!(update["sessionUpdate"], "progress");
    let harn_meta = &update["_meta"]["harn"];
    assert_eq!(harn_meta["phase"], "narration");
    assert_eq!(harn_meta["message"], "Working through verification.");
    for forbidden in ["phase", "message", "progress", "total", "data"] {
        assert!(
            update.get(forbidden).is_none(),
            "progress extension fields must stay under _meta.harn; got {update}"
        );
    }
    assert!(
        rx.try_recv().is_err(),
        "message-only agent_progress must emit exactly one ACP notification"
    );
}
