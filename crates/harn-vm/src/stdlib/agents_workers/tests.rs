use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

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
            context_policy: ContextPolicy::default(),
            resume_workflow: false,
            persist_state: true,
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
    assert!(!loaded.carry_policy.resume_workflow);
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
