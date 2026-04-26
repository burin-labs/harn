use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::orchestration::{
    ArtifactRecord, CapabilityPolicy, ContextPolicy, MutationSessionRecord,
};

use super::*;

#[test]
fn worker_snapshot_round_trip_preserves_resume_fields() {
    let dir = std::env::temp_dir().join(format!("harn-worker-test-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("HARN_WORKER_STATE_DIR", &dir) };

    let snapshot_path = worker_snapshot_path("worker_test");
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
            transcript: Some(VmValue::Dict(Rc::new(BTreeMap::from([(
                "_type".to_string(),
                VmValue::String(Rc::from("transcript")),
            )])))),
        },
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
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
        transcript: Some(VmValue::Dict(Rc::new(BTreeMap::from([(
            "_type".to_string(),
            VmValue::String(Rc::from("transcript")),
        )])))),
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
            metadata: BTreeMap::new(),
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
    unsafe { std::env::remove_var("HARN_WORKER_STATE_DIR") };
}

#[test]
fn worker_summary_exposes_request_and_provenance() {
    let state = WorkerState {
        id: "worker_123".to_string(),
        name: "worker".to_string(),
        task: "latest task".to_string(),
        status: "completed".to_string(),
        created_at: "created".to_string(),
        started_at: "started".to_string(),
        finished_at: Some("finished".to_string()),
        awaiting_started_at: None,
        awaiting_since: None,
        mode: "sub_agent".to_string(),
        history: vec!["original task".to_string(), "latest task".to_string()],
        config: WorkerConfig::SubAgent {
            spec: Box::new(SubAgentRunSpec {
                name: "worker".to_string(),
                task: "latest task".to_string(),
                system: Some("system".to_string()),
                options: BTreeMap::new(),
                returns_schema: None,
                session_id: "session_worker".to_string(),
                parent_session_id: Some("session_parent".to_string()),
            }),
        },
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
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
        VmValue::Dict(Rc::new(BTreeMap::from([
            ("role".to_string(), VmValue::String(Rc::from("user"))),
            ("content".to_string(), VmValue::String(Rc::from("one"))),
        ]))),
        VmValue::Dict(Rc::new(BTreeMap::from([
            ("role".to_string(), VmValue::String(Rc::from("assistant"))),
            ("content".to_string(), VmValue::String(Rc::from("two"))),
        ]))),
        VmValue::Dict(Rc::new(BTreeMap::from([
            ("role".to_string(), VmValue::String(Rc::from("user"))),
            ("content".to_string(), VmValue::String(Rc::from("three"))),
        ]))),
        VmValue::Dict(Rc::new(BTreeMap::from([
            ("role".to_string(), VmValue::String(Rc::from("assistant"))),
            ("content".to_string(), VmValue::String(Rc::from("four"))),
        ]))),
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

    let compacted = compact_worker_transcript(transcript).await.unwrap();
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

    let dict = BTreeMap::from([("task".to_string(), VmValue::String(Rc::from("draft note")))]);
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

    let dict = BTreeMap::from([
        ("task".to_string(), VmValue::String(Rc::from("draft note"))),
        (
            "policy".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "tools".to_string(),
                VmValue::List(Rc::new(vec![
                    VmValue::String(Rc::from("read")),
                    VmValue::String(Rc::from("write")),
                ])),
            )]))),
        ),
        (
            "tools".to_string(),
            VmValue::List(Rc::new(vec![VmValue::String(Rc::from("read"))])),
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

    super::bridge::emit_worker_event(&snapshot, WorkerEvent::WorkerWaitingForInput)
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
