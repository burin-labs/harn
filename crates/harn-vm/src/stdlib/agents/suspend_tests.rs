use super::*;
use super::{resume::*, resume_conditions::*};

use crate::orchestration::MutationSessionRecord;
use std::path::Path;
use std::sync::OnceLock;
use tokio::sync::{Mutex, MutexGuard};

/// Suspend tests mutate the process-wide `HARN_WORKER_STATE_DIR`
/// env var. Serialize them to keep snapshot paths from one test
/// from leaking into another's worker construction. Held across
/// `.await` points, so a tokio-aware mutex is required.
async fn suspend_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}

/// Set up an isolated `.harn/workers/...` directory and seed a single
/// worker into the local registry. Returns the seeded worker id and
/// the test directory (callers clean both up on completion).
fn seed_test_worker(name: &str) -> (String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("harn-suspend-test-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("HARN_WORKER_STATE_DIR", &dir) };
    let worker_id = format!("worker_{}", uuid::Uuid::now_v7());
    let snapshot_path = worker_snapshot_path(&worker_id);
    let state = Arc::new(parking_lot::Mutex::new(WorkerState {
        id: worker_id.clone(),
        name: name.to_string(),
        task: "do the thing".to_string(),
        status: "running".to_string(),
        created_at: uuid::Uuid::now_v7().to_string(),
        started_at: uuid::Uuid::now_v7().to_string(),
        finished_at: None,
        awaiting_started_at: None,
        awaiting_since: None,
        mode: "sub_agent".to_string(),
        history: vec!["do the thing".to_string()],
        config: WorkerConfig::SubAgent {
            spec: Box::new(SubAgentRunSpec {
                name: name.to_string(),
                task: "do the thing".to_string(),
                session_id: format!("session_{worker_id}"),
                ..Default::default()
            }),
        },
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        suspend_signal: Arc::new(AtomicBool::new(false)),
        suspension: None,
        request: Default::default(),
        latest_payload: None,
        latest_error: None,
        transcript: None,
        artifacts: Vec::new(),
        parent_worker_id: None,
        parent_stage_id: None,
        child_run_id: None,
        child_run_path: None,
        carry_policy: agents_workers::WorkerCarryPolicy {
            artifact_mode: "inherit".to_string(),
            transcript_mode: "inherit".to_string(),
            persist_state: true,
            ..Default::default()
        },
        execution: Default::default(),
        snapshot_path,
        audit: MutationSessionRecord::default().normalize(),
    }));
    WORKER_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(worker_id.clone(), state.clone());
    });
    (worker_id, dir)
}

fn teardown(dir: &Path, worker_id: &str) {
    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().remove(worker_id);
    });
    let _ = std::fs::remove_dir_all(dir);
    unsafe { std::env::remove_var("HARN_WORKER_STATE_DIR") };
}

fn handle_value(worker_id: &str) -> VmValue {
    VmValue::task_handle(worker_id.to_string())
}

fn message_value(role: &str, content: &str) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
        (
            "role".to_string(),
            VmValue::String(std::sync::Arc::from(role.to_string())),
        ),
        (
            "content".to_string(),
            VmValue::String(std::sync::Arc::from(content.to_string())),
        ),
    ])))
}

fn summary_status(summary: &VmValue) -> String {
    summary
        .as_dict()
        .and_then(|dict| dict.get("status"))
        .map(VmValue::display)
        .unwrap_or_default()
}

async fn resume_agent_for_test(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(resume_agent_builtin(
            crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
            args,
        ))
        .await
}

fn auto_resume_conditions(kind: &str) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
        "trigger".to_string(),
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "kind".to_string(),
                VmValue::String(std::sync::Arc::from(kind.to_string())),
            ),
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("github")),
            ),
        ]))),
    )])))
}

fn auto_resume_conditions_with_timeout(kind: &str, on_timeout: &str) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
        (
            "trigger".to_string(),
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
                (
                    "kind".to_string(),
                    VmValue::String(std::sync::Arc::from(kind.to_string())),
                ),
                (
                    "provider".to_string(),
                    VmValue::String(std::sync::Arc::from("github")),
                ),
            ]))),
        ),
        (
            "timeout".to_string(),
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
                ("duration_minutes".to_string(), VmValue::Int(1)),
                (
                    "on_timeout".to_string(),
                    VmValue::String(std::sync::Arc::from(on_timeout.to_string())),
                ),
            ]))),
        ),
    ])))
}

fn suspend_options(conditions: VmValue) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
        (
            "initiator".to_string(),
            VmValue::String(std::sync::Arc::from("self")),
        ),
        ("conditions".to_string(), conditions),
    ])))
}

fn auto_resume_trigger_id(summary: &VmValue) -> String {
    let json = crate::llm::vm_value_to_json(summary);
    json["suspension"]["auto_resume_trigger"]["id"]
        .as_str()
        .expect("auto-resume trigger id")
        .to_string()
}

fn worker_status_and_task(worker_id: &str) -> (String, String) {
    WORKER_REGISTRY.with(|registry| {
        let state = registry.borrow().get(worker_id).cloned().unwrap();
        let worker = state.lock();
        (worker.status.clone(), worker.task.clone())
    })
}

fn assert_error_code(error: VmError, code: Code) {
    let message = error.to_string();
    assert!(
        message.contains(code.as_str()),
        "expected {}, got: {message}",
        code.as_str()
    );
}

#[test]
fn resume_conditions_parse_round_trips_each_shape() {
    let trigger = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
        "trigger".to_string(),
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "id".to_string(),
                VmValue::String(std::sync::Arc::from("resume-review")),
            ),
            (
                "kind".to_string(),
                VmValue::String(std::sync::Arc::from("review.approved")),
            ),
            (
                "provider".to_string(),
                VmValue::String(std::sync::Arc::from("github")),
            ),
            (
                "handler".to_string(),
                VmValue::String(std::sync::Arc::from("worker://auto-resume")),
            ),
            (
                "match".to_string(),
                VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                    "events".to_string(),
                    VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                        std::sync::Arc::from("review.approved"),
                    )])),
                )]))),
            ),
        ]))),
    )])));
    let trigger_json = crate::llm::vm_value_to_json(
        &parse_resume_conditions_value(Some(&trigger)).expect("parse trigger"),
    );
    assert_eq!(trigger_json["trigger"]["kind"], "review.approved");

    let timeout = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
        "timeout".to_string(),
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            ("duration_minutes".to_string(), VmValue::Int(15)),
            (
                "on_timeout".to_string(),
                VmValue::String(std::sync::Arc::from("resume_with_input")),
            ),
        ]))),
    )])));
    let timeout_json = crate::llm::vm_value_to_json(
        &parse_resume_conditions_value(Some(&timeout)).expect("parse timeout"),
    );
    assert_eq!(timeout_json["timeout"]["duration_minutes"], 15);
    assert_eq!(timeout_json["timeout"]["on_timeout"], "resume_with_input");

    let event = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
        "on_event".to_string(),
        VmValue::String(std::sync::Arc::from("operator.resume")),
    )])));
    let event_json = crate::llm::vm_value_to_json(
        &parse_resume_conditions_value(Some(&event)).expect("parse event"),
    );
    assert_eq!(event_json["on_event"], "operator.resume");
}

#[test]
fn resume_conditions_parse_reports_harn_sus_002_field() {
    let invalid = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
        "timeout".to_string(),
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
            "duration_minutes".to_string(),
            VmValue::Int(0),
        )]))),
    )])));
    let error = parse_resume_conditions_value(Some(&invalid)).expect_err("invalid timeout");
    assert!(
        error.to_string().contains("HARN-SUS-002")
            && error.to_string().contains("timeout.duration_minutes"),
        "expected HARN-SUS-002 timeout field error, got: {error}"
    );

    let unknown_timeout = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
        "timeout".to_string(),
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            ("duration_minutes".to_string(), VmValue::Int(1)),
            ("extra".to_string(), VmValue::Bool(true)),
        ]))),
    )])));
    let unknown_timeout_error =
        parse_resume_conditions_value(Some(&unknown_timeout)).expect_err("unknown timeout key");
    assert!(
        unknown_timeout_error.to_string().contains("timeout.extra"),
        "expected HARN-SUS-002 timeout.extra field error, got: {unknown_timeout_error}"
    );

    let invalid_event = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
        "on_event".to_string(),
        VmValue::String(std::sync::Arc::from("bad channel")),
    )])));
    let event_error =
        parse_resume_conditions_value(Some(&invalid_event)).expect_err("invalid event topic");
    assert!(
        event_error.to_string().contains("HARN-SUS-002")
            && event_error.to_string().contains("on_event"),
        "expected HARN-SUS-002 on_event field error, got: {event_error}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn suspended_subagent_snapshot_includes_active_suspension_metadata() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-suspend-snapshot");

    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("waiting on external review")),
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                "initiator".to_string(),
                VmValue::String(std::sync::Arc::from("parent")),
            )]))),
        ],
    )
    .await
    .expect("suspend worker");

    let snapshot = snapshot_suspended_subagents();
    let item = snapshot
        .iter()
        .find(|item| item["handle"] == worker_id)
        .expect("suspended worker should be in unsettled snapshot");
    assert_eq!(item["status"], "suspended");
    assert_eq!(item["reason"], "waiting on external review");
    assert_eq!(item["initiator"], "parent");
    assert!(
        item["age_ms"].as_i64().unwrap_or(-1) >= 0,
        "snapshot age must be a non-negative duration"
    );
    assert!(
        item["snapshot_ref"]
            .as_str()
            .is_some_and(|path| !path.is_empty()),
        "suspended workers should expose their durable snapshot path"
    );

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn panic_suspend_worker_suspends_running_worker_and_emits_event() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-panic-running");

    let outcome = panic_suspend_worker(None, &worker_id, "build_storm")
        .await
        .expect("panic suspend running worker");
    assert_eq!(outcome, PanicSuspendOutcome::Suspended);

    // Verify the worker is in the suspended state with the panic
    // reason recorded and the WorkerSuspension envelope installed.
    with_worker_state(&worker_id, |state| {
        let worker = state.lock();
        assert_eq!(worker.status, "suspended");
        assert!(worker.suspend_signal.load(Ordering::SeqCst));
        let suspension = worker.suspension.as_ref().expect("suspension envelope");
        assert_eq!(suspension.reason, "build_storm");
        assert_eq!(suspension.initiator, SuspendInitiator::Triggered);
        Ok(())
    })
    .unwrap();

    // Verify the unsettled snapshot also exposes the panic suspension
    // (the same observability surface operators use to see paused
    // workers via `harn agents list --suspended`).
    let snapshot = snapshot_suspended_subagents();
    let item = snapshot
        .iter()
        .find(|item| item["handle"] == worker_id)
        .expect("panic-suspended worker visible in unsettled snapshot");
    assert_eq!(item["status"], "suspended");
    assert_eq!(item["reason"], "build_storm");
    assert_eq!(item["initiator"], "triggered");

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn panic_suspend_worker_skips_already_suspended_and_terminal_and_unknown() {
    let _guard = suspend_test_lock().await;

    // Already-suspended: first suspend via the normal path, then
    // verify the panic broadcast is a no-op (no double-suspend, no
    // overwritten reason, no second event).
    let (already_suspended, dir1) = seed_test_worker("worker-panic-already-suspended");
    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&already_suspended),
            VmValue::String(std::sync::Arc::from("operator pause")),
        ],
    )
    .await
    .expect("operator-suspend first");

    let outcome = panic_suspend_worker(None, &already_suspended, "build_storm")
        .await
        .expect("panic against already-suspended");
    assert_eq!(outcome, PanicSuspendOutcome::AlreadySuspended);

    // Reason from the original suspension MUST be preserved — the
    // panic broadcast must never silently rewrite a pre-existing
    // suspension reason.
    with_worker_state(&already_suspended, |state| {
        let suspension = state
            .lock()
            .suspension
            .clone()
            .expect("original suspension still present");
        assert_eq!(suspension.reason, "operator pause");
        Ok(())
    })
    .unwrap();

    // Terminal (manually flipped): a worker that is no longer running
    // must be skipped with a `not_running` outcome.
    let (terminal, dir2) = seed_test_worker("worker-panic-terminal");
    with_worker_state(&terminal, |state| {
        state.lock().status = "completed".to_string();
        Ok(())
    })
    .unwrap();
    let outcome = panic_suspend_worker(None, &terminal, "build_storm")
        .await
        .expect("panic against terminal worker");
    assert_eq!(outcome, PanicSuspendOutcome::NotRunning);

    // Unknown: stale worker id never crashes the broadcast.
    let outcome = panic_suspend_worker(None, "worker-does-not-exist", "build_storm")
        .await
        .expect("panic against unknown worker");
    assert_eq!(outcome, PanicSuspendOutcome::Unknown);

    teardown(&dir1, &already_suspended);
    teardown(&dir2, &terminal);
}

#[tokio::test(flavor = "current_thread")]
async fn all_registered_worker_ids_returns_btreemap_order() {
    let _guard = suspend_test_lock().await;
    let (a, dir_a) = seed_test_worker("worker-broadcast-A");
    let (b, dir_b) = seed_test_worker("worker-broadcast-B");

    let ids = all_registered_worker_ids();
    // BTreeMap iteration order is sorted by key — deterministic
    // across replay, which the dispatcher relies on for the
    // panic-broadcast per-worker audit order.
    assert!(ids.contains(&a), "registry must include seeded worker A");
    assert!(ids.contains(&b), "registry must include seeded worker B");
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(
        ids, sorted,
        "all_registered_worker_ids must return a deterministic (sorted) order"
    );

    teardown(&dir_a, &a);
    teardown(&dir_b, &b);
}

#[tokio::test(flavor = "current_thread")]
async fn reset_agent_worker_state_clears_suspended_snapshot() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-reset-snapshot");

    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("waiting on external review")),
        ],
    )
    .await
    .expect("suspend worker");
    assert!(
        snapshot_suspended_subagents()
            .iter()
            .any(|item| item["handle"] == worker_id),
        "seeded suspension should be visible before reset"
    );

    reset_agent_worker_state();

    assert!(
        snapshot_suspended_subagents().is_empty(),
        "stdlib reset should not leave workers visible to later lifecycle snapshots"
    );
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::remove_var("HARN_WORKER_STATE_DIR") };
}

#[tokio::test(flavor = "current_thread")]
async fn suspend_then_resume_then_close_is_idempotent_and_emits_events() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-suspend");

    // Idempotency: second suspend on an already-suspended worker is
    // a no-op summary read, and the suspension metadata recorded on
    // the first call is preserved verbatim.
    let first = suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("waiting on external review")),
        ],
    )
    .await
    .expect("first suspend");
    assert_eq!(summary_status(&first), "suspended");
    let first_suspension = first.as_dict().unwrap().get("suspension").cloned();
    assert!(first_suspension.is_some() && first_suspension.as_ref().unwrap().display() != "nil");

    let second = suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("different reason — should be ignored")),
        ],
    )
    .await
    .expect("second suspend");
    assert_eq!(summary_status(&second), "suspended");
    let second_suspension = second.as_dict().unwrap().get("suspension").cloned();
    // `VmValue` does not implement PartialEq for the dict branch, so
    // compare the JSON projection — the same coast-to-coast equality
    // contract clients see on the wire.
    let to_json = |value: Option<VmValue>| value.map(|v| crate::llm::vm_value_to_json(&v));
    assert_eq!(
        to_json(first_suspension),
        to_json(second_suspension),
        "idempotent suspend must preserve the original suspension metadata"
    );

    // Resume transitions back to running and wires the resume input
    // into the worker's task slot.
    let resumed = resume_agent_for_test(vec![
        handle_value(&worker_id),
        VmValue::String(std::sync::Arc::from("next: write the report")),
    ])
    .await
    .expect("resume");
    assert_eq!(summary_status(&resumed), "running");
    assert!(
        resumed.as_dict().unwrap().get("suspension").is_none()
            || resumed
                .as_dict()
                .unwrap()
                .get("suspension")
                .unwrap()
                .display()
                == "nil"
    );
    assert_eq!(
        resumed.as_dict().unwrap().get("task").map(VmValue::display),
        Some("next: write the report".to_string())
    );

    // Closing a (re-)running worker still works the same way as
    // before — proves that the close path is not gated on a
    // specific upstream status.
    let closed = close_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![handle_value(&worker_id)],
    )
    .await
    .expect("close");
    assert_eq!(summary_status(&closed), "cancelled");

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn suspend_registers_auto_resume_trigger_and_operator_resume_unregisters() {
    let _guard = suspend_test_lock().await;
    crate::triggers::clear_trigger_registry();
    crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
    let (worker_id, dir) = seed_test_worker("worker-auto-resume-register");

    let suspended = suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("waiting for review")),
            suspend_options(auto_resume_conditions("review.approved")),
        ],
    )
    .await
    .expect("suspend with auto-resume trigger");
    assert_eq!(summary_status(&suspended), "suspended");
    let trigger_id = auto_resume_trigger_id(&suspended);
    let binding = crate::triggers::resolve_live_trigger_binding(&trigger_id, None)
        .expect("registered auto-resume binding");
    assert_eq!(binding.kind, "auto_resume");
    assert_eq!(binding.handler.kind(), "auto_resume");
    assert_eq!(binding.match_events, vec!["review.approved".to_string()]);

    let resumed = resume_agent_for_test(vec![handle_value(&worker_id)])
        .await
        .expect("operator resume");
    assert_eq!(summary_status(&resumed), "running");
    let snapshot = crate::triggers::snapshot_trigger_bindings()
        .into_iter()
        .find(|binding| binding.id == trigger_id)
        .expect("auto-resume binding snapshot");
    assert_eq!(snapshot.state, crate::triggers::TriggerState::Terminated);

    teardown(&dir, &worker_id);
    crate::triggers::clear_trigger_registry();
    crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
}

#[tokio::test(flavor = "current_thread")]
async fn top_level_suspend_registers_auto_resume_trigger() {
    let _guard = suspend_test_lock().await;
    crate::triggers::clear_trigger_registry();
    crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
    let dir = std::env::temp_dir().join(format!(
        "harn-top-level-suspend-test-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("HARN_WORKER_STATE_DIR", &dir) };

    let suspended = top_level_agent_suspend_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            VmValue::String(std::sync::Arc::from("session-top-level-auto-resume")),
            VmValue::String(std::sync::Arc::from("continue the top-level task")),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(BTreeMap::new())),
            VmValue::String(std::sync::Arc::from("waiting for review")),
            auto_resume_conditions("review.approved"),
        ],
    )
    .await
    .expect("top-level suspend with auto-resume trigger");
    assert_eq!(summary_status(&suspended), "suspended");
    let worker_id = suspended
        .as_dict()
        .and_then(|dict| dict.get("id"))
        .map(VmValue::display)
        .expect("worker id");
    let trigger_id = auto_resume_trigger_id(&suspended);
    let binding = crate::triggers::resolve_live_trigger_binding(&trigger_id, None)
        .expect("registered top-level auto-resume binding");
    assert_eq!(binding.kind, "auto_resume");
    assert_eq!(binding.handler.kind(), "auto_resume");
    assert_eq!(binding.match_events, vec!["review.approved".to_string()]);

    teardown(&dir, &worker_id);
    crate::triggers::clear_trigger_registry();
    crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
}

#[tokio::test(flavor = "current_thread")]
async fn matching_trigger_event_auto_resumes_worker_and_unregisters() {
    // Auto-resume respawns the worker with `spawn_local`, so the test must
    // run inside a `LocalSet` just like the runtime path.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let _guard = suspend_test_lock().await;
            crate::triggers::clear_trigger_registry();
            crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
            let log = crate::event_log::install_memory_for_current_thread(128);
            let (worker_id, dir) = seed_test_worker("worker-auto-resume-dispatch");

            let suspended = suspend_agent_builtin(
                crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
                vec![
                    handle_value(&worker_id),
                    VmValue::String(std::sync::Arc::from("waiting for review")),
                    suspend_options(auto_resume_conditions("review.approved")),
                ],
            )
            .await
            .expect("suspend with auto-resume trigger");
            let trigger_id = auto_resume_trigger_id(&suspended);
            let event = crate::triggers::TriggerEvent::new(
                crate::triggers::ProviderId::from("github"),
                "review.approved",
                None,
                "delivery-auto-resume",
                None,
                BTreeMap::new(),
                crate::triggers::ProviderPayload::Extension(
                    crate::triggers::ExtensionProviderPayload {
                        provider: "github".to_string(),
                        schema_name: "ReviewEvent".to_string(),
                        raw: serde_json::json!({"decision": "approved"}),
                    },
                ),
                crate::triggers::SignatureStatus::Unsigned,
            );
            // The dispatcher scopes this VM as the async-builtin context
            // for the respawned worker, so it needs the stdlib surface the
            // worker uses after resume.
            let mut base_vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut base_vm);
            let dispatcher = crate::triggers::Dispatcher::with_event_log(base_vm, log);
            let outcomes = dispatcher
                .dispatch_event(event)
                .await
                .expect("dispatch auto-resume event");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(
                outcomes[0].status,
                crate::triggers::DispatchStatus::Succeeded
            );
            assert_eq!(outcomes[0].handler_kind, "auto_resume");

            let (status, task) = worker_status_and_task(&worker_id);
            assert!(
                matches!(status.as_str(), "running" | "completed"),
                "matching trigger should have resumed the worker, got status: {status}"
            );
            assert!(
                task.contains("approved"),
                "resume input should carry trigger payload, got: {task}"
            );
            let snapshot = crate::triggers::snapshot_trigger_bindings()
                .into_iter()
                .find(|binding| binding.id == trigger_id)
                .expect("auto-resume binding snapshot");
            assert_eq!(snapshot.state, crate::triggers::TriggerState::Terminated);

            teardown(&dir, &worker_id);
            crate::triggers::clear_trigger_registry();
            crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_resume_timeout_dispatches_synthetic_resume_input() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let _guard = suspend_test_lock().await;
            crate::triggers::clear_trigger_registry();
            crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
            let log = crate::event_log::install_memory_for_current_thread(128);
            drop(log);
            let clock = crate::triggers::test_util::clock::MockClock::at_wall_ms(0);
            let _clock_guard = crate::triggers::test_util::clock::install_override(clock.clone());
            let (worker_id, dir) = seed_test_worker("worker-auto-resume-timeout");

            let suspended = suspend_agent_builtin(
                crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
                vec![
                    handle_value(&worker_id),
                    VmValue::String(std::sync::Arc::from("waiting for review or timeout")),
                    suspend_options(auto_resume_conditions_with_timeout(
                        "review.approved",
                        "resume_with_input",
                    )),
                ],
            )
            .await
            .expect("suspend with auto-resume timeout");
            let trigger_id = auto_resume_trigger_id(&suspended);

            tokio::task::yield_now().await;
            clock.advance_std(std::time::Duration::from_mins(1)).await;
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;

            let (status, task) = worker_status_and_task(&worker_id);
            // The worker has resumed; whether it is still mid-turn
            // ("running") or has already driven to completion depends on
            // task scheduling, so don't pin the exact intermediate state
            // (that was wall-clock-racy). The dispatch wiring below is the
            // real assertion.
            assert!(
                matches!(status.as_str(), "running" | "completed"),
                "timeout should have resumed the worker, got status: {status}"
            );
            assert!(
                task.contains("auto_resume.timeout"),
                "timeout resume input should name synthetic event, got: {task}"
            );
            let snapshot = crate::triggers::snapshot_trigger_bindings()
                .into_iter()
                .find(|binding| binding.id == trigger_id)
                .expect("auto-resume binding snapshot");
            assert_eq!(snapshot.state, crate::triggers::TriggerState::Terminated);

            teardown(&dir, &worker_id);
            crate::triggers::clear_trigger_registry();
            crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn resume_can_drop_transcript_history_to_summary() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-resume-summary-only");

    WORKER_REGISTRY.with(|registry| {
        let state = registry.borrow().get(&worker_id).cloned().unwrap();
        state.lock().transcript = Some(crate::llm::helpers::new_transcript_with(
            Some(format!("session_{worker_id}")),
            vec![
                message_value("user", "old request"),
                message_value("assistant", "old answer"),
            ],
            Some("Prior digest".to_string()),
            None,
        ));
    });
    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("park before fresh prompt")),
        ],
    )
    .await
    .expect("suspend");

    let resumed = resume_agent_for_test(vec![
        handle_value(&worker_id),
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "input".to_string(),
                VmValue::String(std::sync::Arc::from("fresh prompt")),
            ),
            ("continue_transcript".to_string(), VmValue::Bool(false)),
        ]))),
    ])
    .await
    .expect("resume with transcript reset");
    assert_eq!(summary_status(&resumed), "running");
    assert_eq!(
        resumed.as_dict().unwrap().get("task").map(VmValue::display),
        Some("fresh prompt".to_string())
    );

    let transcript = WORKER_REGISTRY.with(|registry| {
        registry
            .borrow()
            .get(&worker_id)
            .unwrap()
            .lock()
            .transcript
            .clone()
    });
    let transcript = transcript.expect("summary-only transcript");
    let dict = transcript.as_dict().expect("transcript dict");
    let messages = crate::llm::helpers::transcript_message_list(dict).expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0]
            .as_dict()
            .and_then(|message| message.get("content"))
            .map(VmValue::display),
        Some("Prior digest".to_string())
    );
    let resume_context = WORKER_REGISTRY
        .with(|registry| {
            let state = registry.borrow().get(&worker_id).cloned().unwrap();
            let worker = state.lock();
            match &worker.config {
                WorkerConfig::SubAgent { spec } => spec.options.get("_resume_continuity").cloned(),
                _ => None,
            }
        })
        .expect("resume continuity payload");
    let resume_context = crate::llm::vm_value_to_json(&resume_context);
    assert_eq!(
        resume_context["continue_transcript"],
        serde_json::json!(false)
    );
    assert_eq!(resume_context["reason"], "park before fresh prompt");
    assert_eq!(resume_context["suspended_at_turn"], 0);
    assert_eq!(resume_context["digest"], "Prior digest");
    assert_eq!(resume_context["input_rendered"], "fresh prompt");

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn suspend_then_close_transitions_to_cancelled() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-suspend-close");

    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("park me")),
        ],
    )
    .await
    .expect("suspend");

    let summary = close_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![handle_value(&worker_id)],
    )
    .await
    .expect("close");
    assert_eq!(summary_status(&summary), "cancelled");

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn suspended_worker_survives_process_restart_via_snapshot() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-suspend-restart");
    let snapshot_path = WORKER_REGISTRY.with(|registry| {
        registry
            .borrow()
            .get(&worker_id)
            .unwrap()
            .lock()
            .snapshot_path
            .clone()
    });

    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("checkpoint before restart")),
        ],
    )
    .await
    .expect("suspend");

    // Simulate process restart by evicting the in-memory registry
    // entry and cold-loading the persisted snapshot. The wire
    // contract is that suspended status survives the round trip;
    // `interrupted` would be a regression because cooperative
    // suspends are not synonymous with crash-while-running.
    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().remove(&worker_id);
    });
    let reloaded =
        agents_workers::load_worker_state_snapshot(&snapshot_path).expect("reload snapshot");
    assert_eq!(reloaded.status, "suspended");
    assert!(
        reloaded.suspension.is_some(),
        "snapshot must preserve suspension metadata"
    );
    assert_eq!(
        reloaded.suspension.as_ref().unwrap().reason,
        "checkpoint before restart"
    );

    // Rehydrate into the registry and warm-resume — the operator
    // wakes the worker back up after restart.
    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(
            worker_id.clone(),
            Arc::new(parking_lot::Mutex::new(reloaded)),
        );
    });
    let resumed = resume_agent_for_test(vec![handle_value(&worker_id)])
        .await
        .expect("resume after restart");
    assert_eq!(summary_status(&resumed), "running");

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn suspend_resume_links_lifecycle_spans_across_snapshot_reload() {
    let _guard = suspend_test_lock().await;
    crate::tracing::reset_tracing();
    crate::tracing::set_tracing_enabled(true);
    let (worker_id, dir) = seed_test_worker("worker-suspend-resume-trace-link");
    let snapshot_path = WORKER_REGISTRY.with(|registry| {
        registry
            .borrow()
            .get(&worker_id)
            .unwrap()
            .lock()
            .snapshot_path
            .clone()
    });

    let pipeline_span_id =
        crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "pipeline".to_string());
    let pipeline_span_link =
        crate::tracing::span_link(pipeline_span_id).expect("open pipeline span link");
    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("checkpoint before restart")),
        ],
    )
    .await
    .expect("suspend");
    crate::tracing::span_end(pipeline_span_id);

    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().remove(&worker_id);
    });
    let reloaded =
        agents_workers::load_worker_state_snapshot(&snapshot_path).expect("reload snapshot");
    let suspension_record = reloaded.suspension.as_ref().expect("snapshot suspension");
    let prior_span_link = suspension_record
        .prior_span_link
        .clone()
        .expect("snapshot preserves prior span link");
    let reloaded_pipeline_span_link = suspension_record
        .pipeline_span_link
        .clone()
        .expect("snapshot preserves pipeline span link");
    assert!(!prior_span_link.trace_id.is_empty());
    assert!(!prior_span_link.span_id.is_empty());
    assert_eq!(
        reloaded_pipeline_span_link.trace_id,
        pipeline_span_link.trace_id
    );
    assert_eq!(
        reloaded_pipeline_span_link.span_id,
        pipeline_span_link.span_id
    );

    WORKER_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(
            worker_id.clone(),
            Arc::new(parking_lot::Mutex::new(reloaded)),
        );
    });
    resume_agent_for_test(vec![handle_value(&worker_id)])
        .await
        .expect("resume after restart");

    let spans = crate::tracing::peek_spans();
    let suspension = spans
        .iter()
        .find(|span| span.kind == crate::tracing::SpanKind::Suspension)
        .expect("suspension span");
    let resume = spans
        .iter()
        .find(|span| span.kind == crate::tracing::SpanKind::Resume)
        .expect("resume span");
    assert_eq!(suspension.name, format!("suspend {worker_id}"));
    assert_eq!(resume.name, format!("resume {worker_id}"));
    assert_eq!(resume.parent_id, None);
    assert_eq!(resume.links.len(), 2);
    let suspension_link = resume
        .links
        .iter()
        .find(|link| {
            link.attributes.get("harn.link.kind").map(String::as_str) == Some("suspension")
        })
        .expect("resume links to suspension");
    let pipeline_link = resume
        .links
        .iter()
        .find(|link| link.attributes.get("harn.link.kind").map(String::as_str) == Some("pipeline"))
        .expect("resume links to pipeline");
    assert_eq!(suspension_link.trace_id, prior_span_link.trace_id);
    assert_eq!(suspension_link.span_id, prior_span_link.span_id);
    assert_eq!(pipeline_link.trace_id, pipeline_span_link.trace_id);
    assert_eq!(pipeline_link.span_id, pipeline_span_link.span_id);

    teardown(&dir, &worker_id);
    crate::tracing::set_tracing_enabled(false);
    crate::tracing::reset_tracing();
}

/// Verify the suspension and resume spans carry the documented attribute
/// bag (`handle`, `reason`, `initiator`,
/// `pipeline_id` on suspend; `handle`, `initiator`,
/// `continue_transcript`, `had_resume_input`,
/// `linked_suspension_count` on resume). These attributes let OTel
/// consumers slice paused agents without joining back to the worker
/// registry. The test also doubles as a privacy check: resume_input
/// content must not appear on the resume span.
#[tokio::test(flavor = "current_thread")]
async fn suspend_resume_spans_carry_canonical_attribute_bag() {
    let _guard = suspend_test_lock().await;
    crate::tracing::reset_tracing();
    crate::tracing::set_tracing_enabled(true);
    let (worker_id, dir) = seed_test_worker("worker-suspend-resume-attributes");

    let pipeline_span_id =
        crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "pipeline".to_string());
    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("waiting on review")),
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                "initiator".to_string(),
                VmValue::String(std::sync::Arc::from("triggered")),
            )]))),
        ],
    )
    .await
    .expect("suspend");
    crate::tracing::span_end(pipeline_span_id);

    resume_agent_for_test(vec![
        handle_value(&worker_id),
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "input".to_string(),
                VmValue::String(std::sync::Arc::from(
                    "CONFIDENTIAL_DO_NOT_LEAK_INTO_SPAN_ATTRS",
                )),
            ),
            ("continue_transcript".to_string(), VmValue::Bool(false)),
        ]))),
    ])
    .await
    .expect("resume");

    let spans = crate::tracing::peek_spans();
    let suspension = spans
        .iter()
        .find(|span| span.kind == crate::tracing::SpanKind::Suspension)
        .expect("suspension span");
    assert_eq!(
        suspension.metadata.get("handle").and_then(|v| v.as_str()),
        Some(worker_id.as_str()),
        "suspend span exposes handle"
    );
    assert_eq!(
        suspension
            .metadata
            .get("worker_id")
            .and_then(|v| v.as_str()),
        Some(worker_id.as_str()),
        "suspend span exposes worker_id"
    );
    assert_eq!(
        suspension.metadata.get("reason").and_then(|v| v.as_str()),
        Some("waiting on review")
    );
    assert_eq!(
        suspension
            .metadata
            .get("initiator")
            .and_then(|v| v.as_str()),
        Some("triggered"),
        "initiator parsed and exposed"
    );
    assert_eq!(
        suspension
            .metadata
            .get("pipeline_id")
            .and_then(|v| v.as_str()),
        Some(pipeline_span_id.to_string()).as_deref(),
        "pipeline_id alias of pipeline_span_id is present"
    );
    assert_eq!(
        suspension
            .metadata
            .get("pipeline_span_id")
            .and_then(|v| v.as_str()),
        Some(pipeline_span_id.to_string()).as_deref(),
        "pipeline_span_id is present"
    );
    assert_eq!(
        suspension
            .metadata
            .get("has_conditions")
            .and_then(|v| v.as_bool()),
        Some(false)
    );

    let resume = spans
        .iter()
        .find(|span| span.kind == crate::tracing::SpanKind::Resume)
        .expect("resume span");
    assert_eq!(
        resume.metadata.get("handle").and_then(|v| v.as_str()),
        Some(worker_id.as_str())
    );
    assert!(
        resume.metadata.contains_key("initiator"),
        "resume span has initiator"
    );
    assert_eq!(
        resume
            .metadata
            .get("continue_transcript")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        resume
            .metadata
            .get("had_resume_input")
            .and_then(|v| v.as_bool()),
        Some(true),
        "resume span flags presence of resume_input without leaking value"
    );
    assert_eq!(
        resume
            .metadata
            .get("linked_suspension_count")
            .and_then(|v| v.as_u64()),
        Some(2)
    );

    // Privacy guard: the resume_input value itself must not be
    // serialised into any resume-span attribute.
    let serialized = serde_json::to_string(&resume.metadata).expect("serialize metadata");
    assert!(
        !serialized.contains("CONFIDENTIAL_DO_NOT_LEAK_INTO_SPAN_ATTRS"),
        "resume_input content leaked into span metadata: {serialized}"
    );

    teardown(&dir, &worker_id);
    crate::tracing::set_tracing_enabled(false);
    crate::tracing::reset_tracing();
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_returns_suspended_checkpoint_for_current_worker() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-loop-suspend");

    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("pause before another model call")),
        ],
    )
    .await
    .expect("suspend");

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
    let _runtime_context = crate::runtime_context::install_runtime_context_overlay(
        crate::runtime_context::RuntimeContextOverlay {
            worker_id: Some(worker_id.clone()),
            ..Default::default()
        },
    );

    let result = crate::stdlib::harn_entry::call_harn_export_by_name(
        &ctx,
        "std/agent/loop",
        "agent_loop",
        "agent_loop_suspend_test",
        &[
            VmValue::String(std::sync::Arc::from("continue the task")),
            VmValue::Nil,
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                "max_iterations".to_string(),
                VmValue::Int(3),
            )]))),
        ],
    )
    .await
    .expect("agent loop returns suspended checkpoint");
    let json = crate::llm::vm_value_to_json(&result);

    assert_eq!(json["status"], "suspended");
    assert_eq!(json["final_status"], "suspended");
    assert_eq!(json["reason"], "pause before another model call");
    assert_eq!(json["initiator"], "operator");
    assert_eq!(json["iterations_completed"], 0);
    assert_eq!(json["handle"]["id"], worker_id);
    assert!(
        serde_json::to_string(&json)
            .expect("serialize result")
            .contains("Worker suspended before the next turn: pause before another model call"),
        "suspend checkpoint should inject a resume-visible reminder"
    );

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn sub_agent_execution_preserves_suspended_loop_payload() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-sub-agent-suspend");

    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("checkpoint the child loop")),
        ],
    )
    .await
    .expect("suspend");

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
    let _runtime_context = crate::runtime_context::install_runtime_context_overlay(
        crate::runtime_context::RuntimeContextOverlay {
            worker_id: Some(worker_id.clone()),
            ..Default::default()
        },
    );

    let result = execute_sub_agent(
        &ctx,
        SubAgentRunSpec {
            name: "worker-sub-agent-suspend".to_string(),
            task: "continue the task".to_string(),
            session_id: format!("session_{worker_id}"),
            options: BTreeMap::from([("max_iterations".to_string(), VmValue::Int(3))]),
            ..Default::default()
        },
    )
    .await
    .expect("sub-agent execution returns suspended checkpoint");

    assert_eq!(result.payload["status"], "suspended");
    assert_eq!(result.payload["reason"], "checkpoint the child loop");
    assert_eq!(result.payload["handle"]["id"], worker_id);

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn resume_rejects_live_non_suspended_workers() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-resume-not-suspended");

    let err = resume_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![handle_value(&worker_id)],
    )
    .await
    .expect_err("running worker should reject warm resume");
    assert_error_code(err, Code::ResumeWorkerNotSuspended);

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn resume_rejects_closed_suspended_workers() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-resume-closed");

    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("park before close")),
        ],
    )
    .await
    .expect("suspend");
    close_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![handle_value(&worker_id)],
    )
    .await
    .expect("close");

    let err = resume_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![handle_value(&worker_id)],
    )
    .await
    .expect_err("closed worker should reject resume");
    assert_error_code(err, Code::ResumeWorkerClosed);

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn resume_rejects_empty_resume_input() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-empty-resume-input");

    suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("park before empty input")),
        ],
    )
    .await
    .expect("suspend");
    let err = resume_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("")),
        ],
    )
    .await
    .expect_err("empty resume input should be rejected");
    assert_error_code(err, Code::ResumeInputInvalid);

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn resume_missing_snapshot_reports_sus_004() {
    let _guard = suspend_test_lock().await;
    let dir = std::env::temp_dir().join(format!(
        "harn-missing-snapshot-test-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let missing = dir.join("missing-worker.json");

    let err = resume_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![VmValue::String(std::sync::Arc::from(
            missing.display().to_string(),
        ))],
    )
    .await
    .expect_err("missing snapshot should reject resume");
    assert_error_code(err, Code::ResumeSnapshotInvalid);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_warm_resume_reports_sus_006() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-concurrent-resume");
    let state = with_worker_state(&worker_id, Ok).unwrap();

    let ctx = crate::vm::AsyncBuiltinCtx::for_test(Vm::new());
    let err = warm_resume_worker(&ctx, state, WorkerResumeOptions::default())
        .await
        .expect_err("non-suspended warm resume should be a conflict");
    assert_error_code(err, Code::ConcurrentResumeConflict);

    teardown(&dir, &worker_id);
}

#[tokio::test(flavor = "current_thread")]
async fn raw_suspend_trigger_registration_reports_sus_007() {
    let _guard = suspend_test_lock().await;
    crate::triggers::clear_trigger_registry();
    crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
    let (worker_id, dir) = seed_test_worker("worker-trigger-registration-error");
    let invalid_trigger = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
        "trigger".to_string(),
        VmValue::Dict(std::sync::Arc::new(BTreeMap::new())),
    )])));

    let err = suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("bad trigger")),
            suspend_options(invalid_trigger),
        ],
    )
    .await
    .expect_err("invalid raw trigger registration should fail");
    assert_error_code(err, Code::ResumeTriggerRegistrationFailed);

    teardown(&dir, &worker_id);
    crate::triggers::clear_trigger_registry();
    crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
}

#[tokio::test(flavor = "current_thread")]
async fn raw_suspend_timeout_action_reports_sus_008() {
    let _guard = suspend_test_lock().await;
    crate::triggers::clear_trigger_registry();
    crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
    let (worker_id, dir) = seed_test_worker("worker-timeout-action-error");

    let err = suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("bad timeout")),
            suspend_options(auto_resume_conditions_with_timeout(
                "review.approved",
                "explode",
            )),
        ],
    )
    .await
    .expect_err("unsupported raw timeout action should fail");
    assert_error_code(err, Code::ResumeTimeoutUnsupported);

    teardown(&dir, &worker_id);
    crate::triggers::clear_trigger_registry();
    crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
}

#[tokio::test(flavor = "current_thread")]
async fn suspend_rejects_terminal_workers() {
    let _guard = suspend_test_lock().await;
    let (worker_id, dir) = seed_test_worker("worker-suspend-terminal");
    WORKER_REGISTRY.with(|registry| {
        let state = registry.borrow().get(&worker_id).cloned().unwrap();
        state.lock().status = "completed".to_string();
    });

    let err = suspend_agent_builtin(
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
        vec![
            handle_value(&worker_id),
            VmValue::String(std::sync::Arc::from("late suspend")),
        ],
    )
    .await
    .expect_err("terminal worker should reject suspend");
    match err {
        VmError::Runtime(message) => assert!(
            message.contains(Code::SuspendWorkerNotRunning.as_str()),
            "expected HARN-SUS-001 terminal-rejection message, got: {message}"
        ),
        other => panic!("expected Runtime error, got {other:?}"),
    }

    teardown(&dir, &worker_id);
}
