use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::orchestration::{
    ArtifactRecord, CapabilityPolicy, ContextPolicy, MutationSessionRecord,
};

use super::*;

fn vm_string(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn vm_dict(pairs: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::dict(
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<crate::value::DictMap>(),
    )
}

fn vm_closure(name: &str) -> VmValue {
    VmValue::Closure(Arc::new(crate::value::VmClosure {
        func: Arc::new(crate::chunk::CompiledFunction {
            name: name.to_string(),
            type_params: Vec::new(),
            nominal_type_names: Vec::new(),
            params: Vec::new(),
            default_start: None,
            chunk: Arc::new(crate::chunk::Chunk::new()),
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks: false,
        }),
        env: crate::value::VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
    }))
}

/// Minimal `WorkerState` for persistence tests; only `config` and
/// `snapshot_path` vary per test.
fn minimal_worker_state(config: WorkerConfig, snapshot_path: String) -> WorkerState {
    WorkerState {
        id: "worker_test".to_string(),
        name: "worker".to_string(),
        task: "task".to_string(),
        status: "completed".to_string(),
        created_at: "created".to_string(),
        started_at: "started".to_string(),
        finished_at: None,
        awaiting_started_at: None,
        awaiting_since: None,
        mode: "workflow".to_string(),
        history: Vec::new(),
        config,
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        suspend_signal: Arc::new(AtomicBool::new(false)),
        suspension: None,
        request: WorkerRequestRecord::default(),
        latest_payload: None,
        latest_error: None,
        transcript: None,
        artifacts: Vec::new(),
        parent_worker_id: None,
        parent_stage_id: None,
        child_run_id: None,
        child_run_path: None,
        carry_policy: WorkerCarryPolicy {
            artifact_mode: "inherit".to_string(),
            transcript_mode: "inherit".to_string(),
            context_policy: ContextPolicy::default(),
            resume_workflow: true,
            persist_state: true,
            retriggerable: false,
            policy: None,
        },
        execution: WorkerExecutionProfile::default(),
        snapshot_path,
        audit: MutationSessionRecord::default().normalize(),
    }
}

fn temp_snapshot_path() -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("harn-worker-test-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("worker_test.json").to_string_lossy().into_owned();
    (dir, path)
}

#[test]
fn persist_worker_snapshot_rejects_closure_in_workflow_options() {
    // A closure in user-provided workflow options used to persist as a
    // display-string and rehydrate as a plain string — a latent type error
    // with zero signal at save time. The strict serializer must fail loud
    // at the persist seam and name the offending path.
    let (dir, snapshot_path) = temp_snapshot_path();
    let options = crate::value::DictMap::from_iter([
        ("custom_compactor".to_string(), vm_closure("compact")),
        ("model".to_string(), vm_string("haiku")),
    ]);
    let state = minimal_worker_state(
        WorkerConfig::Workflow {
            graph: Box::new(crate::orchestration::WorkflowGraph::default()),
            artifacts: Vec::new(),
            options,
        },
        snapshot_path.clone(),
    );

    let err = match super::config::persist_worker_state_snapshot(&state) {
        Ok(()) => panic!("expected persist to reject the closure"),
        Err(err) => err,
    };
    match err {
        VmError::Runtime(message) => assert!(
            message.contains("options.custom_compactor: closure is not serializable"),
            "got: {message}"
        ),
        other => panic!("expected Runtime error, got {other:?}"),
    }
    // Fail loud means fail before writing: no partial snapshot on disk.
    assert!(!std::path::Path::new(&snapshot_path).exists());

    // Nested values get the full path annotation.
    let options = crate::value::DictMap::from_iter([(
        "hooks".to_string(),
        VmValue::List(Arc::new(vec![vm_closure("hook")])),
    )]);
    let state = minimal_worker_state(
        WorkerConfig::Workflow {
            graph: Box::new(crate::orchestration::WorkflowGraph::default()),
            artifacts: Vec::new(),
            options,
        },
        snapshot_path,
    );
    let err = super::config::persist_worker_state_snapshot(&state).unwrap_err();
    match err {
        VmError::Runtime(message) => assert!(
            message.contains("options.hooks[0]: closure is not serializable"),
            "got: {message}"
        ),
        other => panic!("expected Runtime error, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn persist_worker_snapshot_strips_closures_from_sub_agent_options_with_warning() {
    // The top-level agent suspend path persists the *live* agent-loop
    // options, which legitimately carry callback closures (tool_caller,
    // tool handlers, custom compactors). Hard-erroring would break every
    // suspend that uses callbacks, so the persist seam strips them and
    // keeps the serializable siblings.
    let (dir, snapshot_path) = temp_snapshot_path();
    let options = crate::value::DictMap::from_iter([
        ("_tool_caller".to_string(), vm_closure("tool_caller")),
        ("max_iterations".to_string(), VmValue::Int(7)),
    ]);
    let state = minimal_worker_state(
        WorkerConfig::SubAgent {
            spec: Box::new(SubAgentRunSpec {
                name: "top-level-agent".to_string(),
                task: "task".to_string(),
                system: None,
                options,
                returns_schema: None,
                session_id: "session_1".to_string(),
                parent_session_id: None,
                reminder_propagation: Vec::new(),
                workspace_anchor: None,
            }),
        },
        snapshot_path.clone(),
    );

    super::config::persist_worker_state_snapshot(&state).unwrap();
    let contents = std::fs::read_to_string(&snapshot_path).unwrap();
    let payload: serde_json::Value = serde_json::from_str(&contents).unwrap();
    let spec_options = &payload["config"]["spec"]["options"];
    assert!(
        spec_options.get("_tool_caller").is_none(),
        "closure option must be stripped, got: {spec_options}"
    );
    assert_eq!(spec_options["max_iterations"], serde_json::json!(7));

    // The stripped snapshot still rehydrates.
    let loaded = super::config::load_worker_state_snapshot(&snapshot_path).unwrap();
    match loaded.config {
        WorkerConfig::SubAgent { spec } => {
            assert!(spec.options.get("_tool_caller").is_none());
            assert!(matches!(
                spec.options.get("max_iterations"),
                Some(VmValue::Int(7))
            ));
        }
        _ => panic!("expected sub-agent config"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn persist_worker_snapshot_redacts_secrets_and_round_trips_normal_options() {
    let (dir, snapshot_path) = temp_snapshot_path();
    // Placeholder values (not realistic secret shapes, to avoid tripping
    // push-protection scanners): redaction here fires on the sensitive field
    // NAMES `api_key` / `Authorization`, not on the value patterns. Token
    // value-pattern coverage (sk_live_/ghp_/AKIA/Bearer) lives in the
    // `redact` module's own tests.
    let secret = "fake-api-key-value-for-test";
    let bearer = "Bearer fake-bearer-token-for-test";
    let options = crate::value::DictMap::from_iter([
        ("api_key".to_string(), vm_string(secret)),
        (
            "headers".to_string(),
            vm_dict(vec![("Authorization", vm_string(bearer))]),
        ),
        ("endpoint".to_string(), vm_string("https://example.com/v1")),
        ("retries".to_string(), VmValue::Int(3)),
    ]);
    let state = minimal_worker_state(
        WorkerConfig::Workflow {
            graph: Box::new(crate::orchestration::WorkflowGraph::default()),
            artifacts: Vec::new(),
            options,
        },
        snapshot_path.clone(),
    );

    super::config::persist_worker_state_snapshot(&state).unwrap();
    let contents = std::fs::read_to_string(&snapshot_path).unwrap();
    assert!(
        !contents.contains(secret),
        "persisted snapshot must not contain the raw api key"
    );
    assert!(
        !contents.contains("fake-bearer-token-for-test"),
        "persisted snapshot must not contain the bearer token"
    );
    assert!(contents.contains(crate::redact::REDACTED_PLACEHOLDER));

    // Non-secret values survive the round trip unchanged.
    let loaded = super::config::load_worker_state_snapshot(&snapshot_path).unwrap();
    match loaded.config {
        WorkerConfig::Workflow { options, .. } => {
            assert!(matches!(
                options.get("endpoint"),
                Some(VmValue::String(url)) if url.as_str() == "https://example.com/v1"
            ));
            assert!(matches!(options.get("retries"), Some(VmValue::Int(3))));
        }
        _ => panic!("expected workflow config"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

fn uuid_v7_at_ms(ms: u64) -> String {
    format!(
        "{:08x}-{:04x}-7000-8000-000000000000",
        (ms >> 16) as u32,
        (ms & 0xffff) as u16
    )
}

#[test]
fn worker_timestamp_helpers_decode_uuid_v7_without_wall_clock() {
    let start = uuid_v7_at_ms(1_700_000_010_000);
    let finish = uuid_v7_at_ms(1_700_000_010_321);
    let earlier = uuid_v7_at_ms(1_700_000_009_999);

    assert_eq!(worker_timestamp_unix_ms(&start), Some(1_700_000_010_000));
    assert_eq!(worker_wall_ms(&start, Some(&finish)), Some(321));
    assert_eq!(worker_wall_ms(&start, Some(&earlier)), None);
    assert_eq!(worker_timestamp_unix_ms("not-a-uuid"), None);
    assert_eq!(worker_wall_ms(&start, None), None);
}

#[test]
fn worker_snapshot_round_trip_preserves_resume_fields() {
    // Use an explicit per-test snapshot path inside a unique temp dir instead of
    // routing through the process-global `HARN_WORKER_STATE_DIR` env var. Mutating
    // that var raced with any other test in this binary that reads it in parallel;
    // `persist_worker_state_snapshot` writes `state.snapshot_path` directly and
    // `load_worker_state_snapshot` accepts a full path, so the env var is unneeded.
    let dir = std::env::temp_dir().join(format!("harn-worker-test-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();

    let snapshot_path = dir.join("worker_test.json").to_string_lossy().into_owned();
    let state = WorkerState {
        id: "worker_test".to_string(),
        name: "worker".to_string(),
        task: "task".to_string(),
        status: "completed".to_string(),
        created_at: "created".to_string(),
        started_at: "started".to_string(),
        finished_at: Some("finished".to_string()),
        awaiting_started_at: None,
        awaiting_since: None,
        mode: "workflow".to_string(),
        history: vec!["task".to_string()],
        config: WorkerConfig::Stage {
            node: Box::new(crate::orchestration::WorkflowNode {
                kind: "stage".to_string(),
                ..Default::default()
            }),
            artifacts: Vec::new(),
            transcript: Some(VmValue::dict(crate::value::DictMap::from_iter([(
                "_type".to_string(),
                VmValue::String(arcstr::ArcStr::from("transcript")),
            )]))),
        },
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        suspend_signal: Arc::new(AtomicBool::new(false)),
        suspension: None,
        request: WorkerRequestRecord {
            task: "task".to_string(),
            system: Some("system".to_string()),
            payload: Some(serde_json::json!({
                "research_questions": ["question one"],
                "action_items": [{"id": "action_1", "title": "do the thing"}],
                "workflow_stages": ["research", "implement"],
                "verification_steps": ["cargo test -p harn-vm"],
            })),
            research_questions: vec![serde_json::json!("question one")],
            action_items: vec![serde_json::json!({"id": "action_1", "title": "do the thing"})],
            workflow_stages: vec![
                serde_json::json!("research"),
                serde_json::json!("implement"),
            ],
            verification_steps: vec![serde_json::json!("cargo test -p harn-vm")],
        },
        latest_payload: Some(serde_json::json!({"status": "completed"})),
        latest_error: None,
        transcript: Some(VmValue::dict(crate::value::DictMap::from_iter([(
            "_type".to_string(),
            VmValue::String(arcstr::ArcStr::from("transcript")),
        )]))),
        artifacts: vec![ArtifactRecord {
            type_name: "artifact".to_string(),
            id: "artifact_1".to_string(),
            kind: "summary".to_string(),
            title: Some("summary".to_string()),
            text: Some("done".to_string()),
            data: None,
            source: Some("test".to_string()),
            created_at: "now".to_string(),
            freshness: Some("fresh".to_string()),
            priority: Some(60),
            lineage: Vec::new(),
            relevance: Some(1.0),
            estimated_tokens: Some(1),
            stage: Some("stage".to_string()),
            metadata: std::collections::BTreeMap::new(),
        }],
        parent_worker_id: Some("parent".to_string()),
        parent_stage_id: Some("stage".to_string()),
        child_run_id: Some("run_1".to_string()),
        child_run_path: Some(".harn-runs/run_1.json".to_string()),
        carry_policy: WorkerCarryPolicy {
            artifact_mode: "none".to_string(),
            transcript_mode: "fork".to_string(),
            context_policy: ContextPolicy::default(),
            resume_workflow: false,
            persist_state: true,
            retriggerable: true,
            policy: Some(CapabilityPolicy {
                tools: vec!["read".to_string()],
                side_effect_level: Some("read_only".to_string()),
                ..Default::default()
            }),
        },
        execution: WorkerExecutionProfile::default(),
        snapshot_path: snapshot_path.clone(),
        audit: MutationSessionRecord {
            session_id: "session_worker_test".to_string(),
            parent_session_id: Some("session_parent".to_string()),
            run_id: Some("run_1".to_string()),
            worker_id: Some("worker_test".to_string()),
            execution_kind: Some("workflow".to_string()),
            mutation_scope: "apply_worktree".to_string(),
            approval_policy: None,
        }
        .normalize(),
    };

    super::config::persist_worker_state_snapshot(&state).unwrap();
    let loaded = super::config::load_worker_state_snapshot(&snapshot_path).unwrap();
    assert_eq!(loaded.id, "worker_test");
    assert_eq!(loaded.child_run_id.as_deref(), Some("run_1"));
    assert_eq!(
        loaded.child_run_path.as_deref(),
        Some(".harn-runs/run_1.json")
    );
    assert_eq!(loaded.carry_policy.artifact_mode, "none");
    assert_eq!(loaded.carry_policy.transcript_mode, "fork");
    assert!(!loaded.carry_policy.resume_workflow);
    assert!(loaded.carry_policy.retriggerable);
    assert_eq!(
        loaded.request.payload,
        Some(serde_json::json!({
            "research_questions": ["question one"],
            "action_items": [{"id": "action_1", "title": "do the thing"}],
            "workflow_stages": ["research", "implement"],
            "verification_steps": ["cargo test -p harn-vm"],
        }))
    );
    assert_eq!(
        loaded.request.action_items,
        vec![serde_json::json!({"id": "action_1", "title": "do the thing"})]
    );
    assert_eq!(
        loaded.carry_policy.policy,
        Some(CapabilityPolicy {
            tools: vec!["read".to_string()],
            side_effect_level: Some("read_only".to_string()),
            ..Default::default()
        })
    );
    assert_eq!(loaded.audit.session_id, "session_worker_test");
    assert_eq!(loaded.audit.mutation_scope, "apply_worktree");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parse_worker_config_rejects_unknown_top_level_options() {
    let config = vm_dict(vec![
        ("task", vm_string("do work")),
        ("node", vm_dict(vec![("kind", vm_string("stage"))])),
        ("typo", VmValue::Bool(true)),
    ]);

    let err = match super::config::parse_worker_config(&config) {
        Ok(_) => panic!("expected unknown option failure"),
        Err(err) => err,
    };

    match err {
        VmError::Runtime(message) => assert!(message.contains("typo"), "got: {message}"),
        other => panic!("expected Runtime error, got {other:?}"),
    }
}

#[test]
fn parse_worker_config_rejects_unknown_carry_options() {
    let config = vm_dict(vec![
        ("task", vm_string("do work")),
        ("node", vm_dict(vec![("kind", vm_string("stage"))])),
        ("carry", vm_dict(vec![("transcritp", vm_string("fork"))])),
    ]);

    let err = match super::config::parse_worker_config(&config) {
        Ok(_) => panic!("expected unknown carry option failure"),
        Err(err) => err,
    };

    match err {
        VmError::Runtime(message) => assert!(message.contains("transcritp"), "got: {message}"),
        other => panic!("expected Runtime error, got {other:?}"),
    }
}

#[test]
fn worker_summary_exposes_request_and_provenance() {
    let created_at = uuid_v7_at_ms(1_700_000_000_000);
    let started_at = uuid_v7_at_ms(1_700_000_000_250);
    let finished_at = uuid_v7_at_ms(1_700_000_001_750);
    let mut state = WorkerState {
        id: "worker_123".to_string(),
        name: "worker".to_string(),
        task: "latest task".to_string(),
        status: "completed".to_string(),
        created_at: created_at.clone(),
        started_at: started_at.clone(),
        finished_at: Some(finished_at.clone()),
        awaiting_started_at: None,
        awaiting_since: None,
        mode: "sub_agent".to_string(),
        history: vec!["original task".to_string(), "latest task".to_string()],
        config: WorkerConfig::SubAgent {
            spec: Box::new(SubAgentRunSpec {
                name: "worker".to_string(),
                task: "latest task".to_string(),
                system: Some("system".to_string()),
                options: crate::value::DictMap::new(),
                returns_schema: None,
                session_id: "session_worker".to_string(),
                parent_session_id: Some("session_parent".to_string()),
                reminder_propagation: Vec::new(),
                workspace_anchor: None,
            }),
        },
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        suspend_signal: Arc::new(AtomicBool::new(false)),
        suspension: None,
        request: WorkerRequestRecord {
            task: "original task".to_string(),
            system: Some("system".to_string()),
            payload: Some(serde_json::json!({
                "research_questions": ["What changed?"],
            })),
            research_questions: vec![serde_json::json!("What changed?")],
            action_items: Vec::new(),
            workflow_stages: Vec::new(),
            verification_steps: Vec::new(),
        },
        latest_payload: Some(serde_json::json!({"ok": true})),
        latest_error: None,
        transcript: None,
        artifacts: Vec::new(),
        parent_worker_id: Some("parent_worker".to_string()),
        parent_stage_id: Some("stage_1".to_string()),
        child_run_id: Some("run_123".to_string()),
        child_run_path: Some(".harn-runs/run_123.json".to_string()),
        carry_policy: WorkerCarryPolicy::default(),
        execution: WorkerExecutionProfile::default(),
        snapshot_path: ".harn/workers/worker_123.json".to_string(),
        audit: MutationSessionRecord {
            session_id: "session_worker".to_string(),
            parent_session_id: Some("session_parent".to_string()),
            ..Default::default()
        }
        .normalize(),
    };

    let summary = clone_worker_state(&state);
    assert_eq!(
        summary["request"]["task"],
        serde_json::json!("original task")
    );
    assert_eq!(
        summary["request"]["research_questions"][0],
        serde_json::json!("What changed?")
    );
    assert_eq!(
        summary["provenance"]["worker_id"],
        serde_json::json!("worker_123")
    );
    assert_eq!(
        summary["provenance"]["parent_session_id"],
        serde_json::json!("session_parent")
    );
    assert_eq!(summary["task"], serde_json::json!("latest task"));
    assert_eq!(summary["created_at"], serde_json::json!(created_at));
    assert_eq!(summary["started_at"], serde_json::json!(started_at));
    assert_eq!(summary["finished_at"], serde_json::json!(finished_at));
    assert_eq!(
        summary["created_at_ms"],
        serde_json::json!(1_700_000_000_000i64)
    );
    assert_eq!(
        summary["started_at_ms"],
        serde_json::json!(1_700_000_000_250i64)
    );
    assert_eq!(
        summary["finished_at_ms"],
        serde_json::json!(1_700_000_001_750i64)
    );
    assert_eq!(summary["wall_ms"], serde_json::json!(1_500i64));

    state.status = "suspended".to_string();
    state.suspension = Some(WorkerSuspension {
        reason: "waiting on review".to_string(),
        initiator: SuspendInitiator::Operator,
        suspended_at: "01978f25-0000-7000-8000-000000000000".to_string(),
        snapshot_ref: ".harn/workers/worker_123.json".to_string(),
        conditions: Some(serde_json::json!({
            "trigger": {"provider": "github", "kind": "comment"},
            "timeout": {"minutes": 30},
        })),
        ..Default::default()
    });

    let event_snapshot = super::bridge::worker_event_snapshot(&state);
    assert_eq!(
        event_snapshot.metadata["created_at_ms"],
        serde_json::json!(1_700_000_000_000i64)
    );
    assert_eq!(
        event_snapshot.metadata["started_at_ms"],
        serde_json::json!(1_700_000_000_250i64)
    );
    assert_eq!(
        event_snapshot.metadata["finished_at_ms"],
        serde_json::json!(1_700_000_001_750i64)
    );
    assert_eq!(
        event_snapshot.metadata["wall_ms"],
        serde_json::json!(1_500i64)
    );
    let suspension = &event_snapshot.metadata["suspension"];
    assert_eq!(suspension["handle"], serde_json::json!("worker_123"));
    assert_eq!(suspension["reason"], serde_json::json!("waiting on review"));
    assert_eq!(
        suspension["resume_by_mechanism"],
        serde_json::json!("manual")
    );
    assert_eq!(suspension["conditions"]["timeout"]["minutes"], 30);
}

#[test]
fn artifact_carry_policy_can_drop_all_artifacts() {
    let policy = WorkerCarryPolicy {
        artifact_mode: "none".to_string(),
        ..Default::default()
    };
    let artifacts = vec![ArtifactRecord {
        kind: "summary".to_string(),
        ..Default::default()
    }];
    let selected = apply_worker_artifact_policy(&artifacts, &policy);
    assert!(selected.is_empty());
}

#[test]
fn transcript_carry_policy_can_reset_or_fork_transcripts() {
    let transcript = crate::llm::helpers::new_transcript_with(
        Some("parent-transcript".to_string()),
        Vec::new(),
        None,
        None,
    );
    let reset = WorkerCarryPolicy {
        transcript_mode: "reset".to_string(),
        ..Default::default()
    };
    assert!(
        apply_worker_transcript_policy(Some(transcript.clone()), &reset)
            .unwrap()
            .is_none()
    );

    let fork = WorkerCarryPolicy {
        transcript_mode: "fork".to_string(),
        ..Default::default()
    };
    let forked = apply_worker_transcript_policy(Some(transcript), &fork)
        .unwrap()
        .expect("forked transcript");
    let dict = forked.as_dict().expect("transcript dict");
    assert_ne!(
        dict.get("id").map(VmValue::display).as_deref(),
        Some("parent-transcript")
    );
    assert_eq!(
        dict.get("metadata")
            .and_then(VmValue::as_dict)
            .and_then(|metadata| metadata.get("parent_transcript_id"))
            .map(VmValue::display)
            .as_deref(),
        Some("parent-transcript")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compact_transcript_mode_reduces_carried_messages() {
    let messages = vec![
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                "role".to_string(),
                VmValue::String(arcstr::ArcStr::from("user")),
            ),
            (
                "content".to_string(),
                VmValue::String(arcstr::ArcStr::from("one")),
            ),
        ])),
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                "role".to_string(),
                VmValue::String(arcstr::ArcStr::from("assistant")),
            ),
            (
                "content".to_string(),
                VmValue::String(arcstr::ArcStr::from("two")),
            ),
        ])),
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                "role".to_string(),
                VmValue::String(arcstr::ArcStr::from("user")),
            ),
            (
                "content".to_string(),
                VmValue::String(arcstr::ArcStr::from("three")),
            ),
        ])),
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                "role".to_string(),
                VmValue::String(arcstr::ArcStr::from("assistant")),
            ),
            (
                "content".to_string(),
                VmValue::String(arcstr::ArcStr::from("four")),
            ),
        ])),
    ];
    let transcript = crate::llm::helpers::new_transcript_with_events(
        Some("compact-transcript".to_string()),
        messages,
        None,
        None,
        vec![crate::llm::helpers::transcript_event(
            "worker_note",
            "system",
            "internal",
            "preserve me",
            None,
        )],
        Vec::new(),
        None,
    );

    let ctx = crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new());
    let compacted = compact_worker_transcript(&ctx, transcript).await.unwrap();
    let dict = compacted.as_dict().expect("transcript dict");
    let messages = crate::llm::helpers::transcript_message_list(dict).unwrap();

    assert!(messages.len() < 4);
    assert!(dict.get("summary").is_some());
    let events = dict
        .get("events")
        .and_then(|value| match value {
            VmValue::List(list) => Some(list),
            _ => None,
        })
        .expect("events");
    assert!(events.iter().filter_map(VmValue::as_dict).any(|event| {
        event.get("kind").map(VmValue::display).as_deref() == Some("worker_note")
    }));
}

#[test]
fn worker_policy_inherits_parent_ceiling_when_unspecified() {
    crate::orchestration::push_execution_policy(CapabilityPolicy {
        tools: vec!["read".to_string()],
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    });

    let dict = crate::value::DictMap::from_iter([(
        "task".to_string(),
        VmValue::String(arcstr::ArcStr::from("draft note")),
    )]);
    let resolved = super::policy::resolve_worker_policy(&dict).unwrap();

    crate::orchestration::pop_execution_policy();

    assert_eq!(
        resolved,
        Some(CapabilityPolicy {
            tools: vec!["read".to_string()],
            side_effect_level: Some("read_only".to_string()),
            ..Default::default()
        })
    );
}

#[test]
fn worker_policy_intersects_explicit_policy_and_tools_shorthand() {
    crate::orchestration::push_execution_policy(CapabilityPolicy {
        tools: vec!["read".to_string(), "write".to_string()],
        side_effect_level: Some("workspace_write".to_string()),
        ..Default::default()
    });

    let dict = crate::value::DictMap::from_iter([
        (
            "task".to_string(),
            VmValue::String(arcstr::ArcStr::from("draft note")),
        ),
        (
            "policy".to_string(),
            VmValue::dict(crate::value::DictMap::from_iter([(
                "tools".to_string(),
                VmValue::List(std::sync::Arc::new(vec![
                    VmValue::String(arcstr::ArcStr::from("read")),
                    VmValue::String(arcstr::ArcStr::from("write")),
                ])),
            )])),
        ),
        (
            "tools".to_string(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("read"),
            )])),
        ),
    ]);
    let resolved = super::policy::resolve_worker_policy(&dict).unwrap();

    crate::orchestration::pop_execution_policy();

    assert_eq!(
        resolved,
        Some(CapabilityPolicy {
            tools: vec!["read".to_string()],
            side_effect_level: Some("workspace_write".to_string()),
            ..Default::default()
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn emit_worker_event_routes_through_parent_session_sink() {
    // The bridge translation has been there for a while, but the
    // canonical AgentEvent path is new (harn#703). Lock in the
    // contract: an emitted worker lifecycle event must surface on the
    // parent agent-session sink, with status string and typed event
    // discriminator both populated, so ACP/A2A adapters subscribed to
    // the registry observe it without polling the bridge.
    use std::sync::Mutex;

    use crate::agent_events::{
        clear_session_sinks, register_sink, AgentEvent, AgentEventSink, WorkerEvent,
    };

    struct CapturingSink(Arc<Mutex<Vec<AgentEvent>>>);
    impl AgentEventSink for CapturingSink {
        fn handle_event(&self, event: &AgentEvent) {
            self.0
                .lock()
                .expect("captured sink mutex poisoned")
                .push(event.clone());
        }
    }

    let parent_session = "parent-session-emit-test".to_string();
    clear_session_sinks(&parent_session);
    let captured: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    register_sink(
        parent_session.clone(),
        Arc::new(CapturingSink(captured.clone())),
    );

    let snapshot = super::bridge::WorkerEventSnapshot {
        worker_id: "worker_e".to_string(),
        worker_name: "n".to_string(),
        worker_task: "do work".to_string(),
        worker_mode: "delegated_stage".to_string(),
        metadata: serde_json::json!({"started_at": "0193..."}),
        audit: MutationSessionRecord {
            parent_session_id: Some(parent_session.clone()),
            ..Default::default()
        }
        .normalize(),
    };

    super::bridge::emit_worker_event(None, &snapshot, WorkerEvent::WorkerWaitingForInput)
        .await
        .expect("emit");

    let received = captured.lock().unwrap().clone();
    assert_eq!(received.len(), 1, "got: {received:?}");
    match &received[0] {
        AgentEvent::WorkerUpdate {
            session_id,
            worker_id,
            event,
            status,
            metadata,
            audit,
            ..
        } => {
            assert_eq!(session_id, &parent_session);
            assert_eq!(worker_id, "worker_e");
            assert_eq!(*event, WorkerEvent::WorkerWaitingForInput);
            assert_eq!(status, "awaiting_input");
            assert_eq!(metadata["started_at"], serde_json::json!("0193..."));
            assert!(audit.is_some(), "audit JSON should be attached");
        }
        other => panic!("expected WorkerUpdate, got {other:?}"),
    }

    clear_session_sinks(&parent_session);
}

/// The `--approve auto` (headless) shape: a live auto-approve policy sits on
/// the parent's approval stack while NO mutation session is installed (a
/// top-level `agent_loop` never installs one). A background sub-agent
/// (`agent_fanout` child) must inherit that live approval policy via
/// `inherited_worker_audit` instead of defaulting to None — otherwise its
/// writes hit the host approval gate with no policy and are denied.
#[test]
fn inherited_worker_audit_falls_back_to_live_approval_policy() {
    use crate::orchestration::{
        clear_execution_policy_stacks, current_approval_policy, install_current_mutation_session,
        push_approval_policy, ToolApprovalPolicy,
    };

    // Isolate the thread-local approval stack + mutation session from any
    // sibling test that reused this worker thread.
    clear_execution_policy_stacks();
    install_current_mutation_session(None);

    // Auto-approve-everything: the `--approve auto` policy shape.
    let parent_policy = ToolApprovalPolicy {
        auto_approve: vec!["*".to_string()],
        ..ToolApprovalPolicy::default()
    };
    push_approval_policy(parent_policy.clone());
    assert_eq!(
        current_approval_policy(),
        Some(parent_policy.clone()),
        "precondition: parent policy is live on the approval stack",
    );

    let audit = inherited_worker_audit("sub_agent");
    assert_eq!(
        audit.approval_policy,
        Some(parent_policy),
        "background sub-agent audit must carry the parent's live approval policy",
    );
    assert_eq!(audit.execution_kind.as_deref(), Some("sub_agent"));

    // With NEITHER a mutation session NOR a live approval policy, approval
    // stays None — no spurious policy is synthesized.
    clear_execution_policy_stacks();
    install_current_mutation_session(None);
    assert_eq!(
        current_approval_policy(),
        None,
        "precondition: stack cleared"
    );
    let bare = inherited_worker_audit("sub_agent");
    assert_eq!(
        bare.approval_policy, None,
        "no mutation session and no live approval policy => approval stays None",
    );

    clear_execution_policy_stacks();
    install_current_mutation_session(None);
}

/// Companion coverage for `parse_worker_audit`: an audit dict that omits
/// `approval_policy` must inherit the live parent approval policy from the
/// stack (same `--approve auto` fallback) rather than deserializing to None.
#[test]
fn parse_worker_audit_falls_back_to_live_approval_policy() {
    use crate::orchestration::{
        clear_execution_policy_stacks, install_current_mutation_session, push_approval_policy,
        ToolApprovalPolicy,
    };

    clear_execution_policy_stacks();
    install_current_mutation_session(None);

    let parent_policy = ToolApprovalPolicy {
        auto_approve: vec!["*".to_string()],
        ..ToolApprovalPolicy::default()
    };
    push_approval_policy(parent_policy.clone());

    // Audit dict that carries a scope but deliberately omits approval_policy.
    let dict: crate::value::DictMap = vec![(
        "audit".to_string(),
        vm_dict(vec![("mutation_scope", vm_string("workspace_write"))]),
    )]
    .into_iter()
    .collect();
    let audit = super::audit::parse_worker_audit(&dict).expect("parse_worker_audit");
    assert_eq!(
        audit.approval_policy,
        Some(parent_policy),
        "parse_worker_audit must inherit the live approval policy when the dict omits it",
    );
    assert_eq!(audit.mutation_scope, "workspace_write");

    clear_execution_policy_stacks();
    install_current_mutation_session(None);
}
