use super::protocol::server_with_api_key_policy;
use super::*;
#[tokio::test]
async fn send_message_dispatches_to_shared_core_export() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let request = harn_vm::jsonrpc::request(
        "1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [{"type": "text", "text": "hello"}]
            }
        }),
    );

    let processed = server.process_rpc(request, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };

    assert_eq!(response["result"]["status"]["state"], "completed");
    assert_eq!(
        response["result"]["history"][1]["parts"][0]["text"],
        "hello"
    );
}

#[tokio::test]
async fn send_message_round_trips_file_and_data_parts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn triage(message: dict) -> dict {
  return message
}
",
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let request = harn_vm::jsonrpc::request(
        "parts-1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [
                    {"type": "text", "text": "inspect attachments"},
                    {
                        "type": "file",
                        "file": {
                            "bytes": "AAEC/w==",
                            "mimeType": "application/octet-stream",
                            "name": "payload.bin"
                        }
                    },
                    {
                        "kind": "file",
                        "file": {
                            "uri": "https://example.test/report.pdf",
                            "mimeType": "application/pdf",
                            "name": "report.pdf"
                        }
                    },
                    {
                        "type": "data",
                        "data": {"ticket": "HARN-891", "priority": 2}
                    }
                ]
            }
        }),
    );

    let processed = server.process_rpc(request, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };

    assert_eq!(response["result"]["status"]["state"], "completed");
    let user_parts = response["result"]["history"][0]["parts"]
        .as_array()
        .expect("user parts");
    assert_eq!(user_parts[1]["type"], "file");
    assert_eq!(user_parts[1]["file"]["bytes"], "AAEC/w==");
    assert_eq!(
        user_parts[2]["file"]["uri"],
        "https://example.test/report.pdf"
    );
    assert_eq!(user_parts[3]["type"], "data");
    assert_eq!(user_parts[3]["data"]["ticket"], "HARN-891");

    let agent_parts = response["result"]["history"][1]["parts"]
        .as_array()
        .expect("agent parts");
    assert_eq!(agent_parts, user_parts);
    assert!(response["result"]["artifacts"]
        .as_array()
        .expect("artifacts")
        .iter()
        .any(|artifact| artifact["parts"][0]["type"] == "file"));
}

#[test]
fn response_artifacts_emit_file_and_data_parts() {
    let response = json!({
        "visible_text": "done",
        "artifacts": [
            {
                "_type": "artifact",
                "id": "artifact_file",
                "kind": "file",
                "title": "payload.bin",
                "data": {
                    "bytes": "AAEC/w==",
                    "mimeType": "application/octet-stream",
                    "name": "payload.bin"
                }
            },
            {
                "_type": "artifact",
                "id": "artifact_data",
                "kind": "data",
                "data": {"answer": 42}
            }
        ]
    });

    let parts = super::response_parts(&response);
    assert_eq!(parts[0], json!({"type": "text", "text": "done"}));
    assert_eq!(parts[1]["type"], "file");
    assert_eq!(parts[1]["file"]["bytes"], "AAEC/w==");
    assert_eq!(parts[1]["file"]["mimeType"], "application/octet-stream");
    assert_eq!(parts[2]["type"], "data");
    assert_eq!(parts[2]["data"]["answer"], 42);

    let artifacts = super::response_artifacts(&response, &parts);
    assert_eq!(artifacts[0]["artifactId"], "artifact_file");
    assert_eq!(artifacts[0]["parts"][0]["type"], "file");
    assert_eq!(artifacts[1]["parts"][0]["type"], "data");
}

#[tokio::test]
async fn send_message_surfaces_handoff_metadata() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
import "std/agents"

pub fn triage(task: string) -> dict {
  let review = handoff({
    source_persona: "merge_captain",
    target_persona_or_human: {
      kind: "persona",
      id: "review_captain",
      label: "review_captain"
    },
    task: task,
    reason: "Need explicit code review before merge",
    evidence_refs: [{artifact_id: "artifact_diff", label: "Patch summary"}],
    files_or_entities_touched: ["crates/harn-vm/src/orchestration/handoffs.rs"],
    open_questions: ["Is the side-effect budget acceptable?"],
    blocked_on: ["review_captain approval"],
    requested_capabilities: ["review", "comment"],
    allowed_side_effects: ["comment_on_pr"],
    budget_remaining: {tokens: 900, tool_calls: 2},
    deadline_checkback: {checkback_at: "2026-04-24T10:00:00Z"},
    confidence: 0.74
  })
  return workflow_result_run(
    task,
    "triage",
    {visible_text: "handoff ready"},
    [handoff_artifact(review)],
    {}
  )
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let request = harn_vm::jsonrpc::request(
        "handoff-1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [{"type": "text", "text": "Review PR #461"}]
            }
        }),
    );

    let processed = server.process_rpc(request, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };

    assert_eq!(response["result"]["status"]["state"], "completed");
    assert!(response["result"]["metadata"]["handoff_ids"][0]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(
        response["result"]["metadata"]["handoffs"][0]["source_persona"],
        "merge_captain"
    );
    assert_eq!(
        response["result"]["metadata"]["handoffs"][0]["target_persona_or_human"]["label"],
        "review_captain"
    );
}

#[tokio::test]
async fn streaming_send_and_resubscribe_replay_task_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let request = harn_vm::jsonrpc::request(
        "stream-1",
        "message/stream",
        json!({
            "function": "triage",
            "message": {
                "parts": [{"type": "text", "text": "stream me"}]
            }
        }),
    );

    let processed = server
        .clone()
        .process_rpc(request, AuthRequest::default())
        .await;
    let RpcOutcome::Sse(mut rx) = processed.outcome else {
        panic!("expected sse response");
    };
    let mut events = Vec::new();
    while let Some(event) = tokio::time::timeout(std::time::Duration::from_secs(2), rx.next())
        .await
        .expect("stream event")
    {
        let done = event
            .pointer("/result/status/state")
            .and_then(JsonValue::as_str)
            == Some("completed");
        events.push(event);
        if done {
            break;
        }
    }

    let task_id = events[0]["result"]["taskId"].as_str().expect("task id");
    assert!(events.iter().any(|event| {
        event
            .pointer("/result/status/state")
            .and_then(JsonValue::as_str)
            == Some("working")
    }));
    assert!(events.iter().any(|event| {
        event
            .pointer("/result/message/parts/0/text")
            .and_then(JsonValue::as_str)
            == Some("stream me")
    }));

    let resubscribe =
        harn_vm::jsonrpc::request("resub-1", "tasks/resubscribe", json!({"id": task_id}));
    let processed = server
        .process_rpc(resubscribe, AuthRequest::default())
        .await;
    let RpcOutcome::Sse(replay_rx) = processed.outcome else {
        panic!("expected replay stream");
    };
    let replayed = replay_rx.collect::<Vec<_>>().await;
    assert!(replayed.iter().any(|event| {
        event
            .pointer("/result/status/state")
            .and_then(JsonValue::as_str)
            == Some("completed")
    }));
}

#[tokio::test]
async fn streaming_agent_progress_emits_status_update_before_completion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
import { agent_progress } from "std/agent/progress"

pub fn triage(task: string) -> string {
  agent_progress({
    message: "Agent is checking progress.",
    entries: [
      {content: "Inspect code.", status: "completed", priority: "high"},
      {content: "Run A2A stream.", status: "in_progress"},
    ],
  })
  return task
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let request = harn_vm::jsonrpc::request(
        "stream-progress-1",
        "message/stream",
        json!({
            "function": "triage",
            "message": {
                "parts": [{"type": "text", "text": "stream progress"}]
            }
        }),
    );

    let processed = server.process_rpc(request, AuthRequest::default()).await;
    let RpcOutcome::Sse(mut rx) = processed.outcome else {
        panic!("expected sse response");
    };
    let mut events = Vec::new();
    while let Some(event) = tokio::time::timeout(std::time::Duration::from_secs(2), rx.next())
        .await
        .expect("stream event")
    {
        let done = event
            .pointer("/result/status/state")
            .and_then(JsonValue::as_str)
            == Some("completed");
        events.push(event);
        if done {
            break;
        }
    }

    let progress = events
        .iter()
        .find(|event| {
            event.pointer("/result/kind").and_then(JsonValue::as_str) == Some("status-update")
        })
        .expect("progress status update");
    assert_eq!(
        progress.pointer("/result/type").and_then(JsonValue::as_str),
        Some("status")
    );
    assert_eq!(
        progress
            .pointer("/result/status/state")
            .and_then(JsonValue::as_str),
        Some("working")
    );
    assert_eq!(
        progress
            .pointer("/result/final")
            .and_then(JsonValue::as_bool),
        Some(false)
    );
    assert_eq!(
        progress
            .pointer("/result/status/message/parts/0/text")
            .and_then(JsonValue::as_str),
        Some(
            "Agent is checking progress.\n\nPlan:\n- [x] Inspect code. (priority: high)\n- [ ] Run A2A stream. (in progress)"
        )
    );
    assert!(events.iter().any(|event| {
        event
            .pointer("/result/status/state")
            .and_then(JsonValue::as_str)
            == Some("completed")
    }));
}

#[test]
fn signed_card_adds_signature_envelope() {
    let mut card = json!({"id": "agent", "skills": []});
    sign_card(&mut card, "secret");

    assert!(card["signatures"][0]["protected"].as_str().unwrap().len() > 16);
    assert!(card["signatures"][0]["signature"].as_str().unwrap().len() > 16);
}

use harn_vm::agent_events::AgentEventSink as _;

#[test]
fn a2a_worker_sink_publishes_worker_update_to_task_stream() {
    // The per-task `AgentEventSink` translates canonical worker
    // lifecycle events into A2A task events of type
    // `worker_update`. This is the A2A side of the ACP/A2A parity
    // contract — same canonical AgentEvent, mapped onto each
    // protocol's wire shape from a single source.
    let task_id = "task-1".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: None,
        status: TaskStatus::Working,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    sink.handle_event(&harn_vm::agent_events::AgentEvent::WorkerUpdate {
        session_id: super::a2a_worker_session_id(&task_id),
        worker_id: "worker-9".into(),
        worker_name: "review".into(),
        worker_task: "review pr".into(),
        worker_mode: "delegated_stage".into(),
        event: harn_vm::agent_events::WorkerEvent::WorkerWaitingForInput,
        status: "awaiting_input".into(),
        metadata: serde_json::json!({"awaiting_started_at": "0193..."}),
        audit: Some(serde_json::json!({"run_id": "run_x"})),
    });

    // Chat chunks are ignored — the sink is intentionally narrow so
    // task-stream extension events don't duplicate task history.
    sink.handle_event(&harn_vm::agent_events::AgentEvent::AgentMessageChunk {
        session_id: super::a2a_worker_session_id(&task_id),
        content: "ignored".into(),
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    let worker_events: Vec<&JsonValue> = task
        .events
        .iter()
        .filter(|event| event.get("type").and_then(JsonValue::as_str) == Some("worker_update"))
        .collect();
    assert_eq!(worker_events.len(), 1, "events: {:?}", task.events);
    let event = worker_events[0];
    assert_eq!(event["taskId"], task_id);
    assert_eq!(event["workerId"], "worker-9");
    assert_eq!(event["status"], "awaiting_input");
    assert_eq!(event["terminal"], false);
    assert_eq!(event["audit"]["run_id"], "run_x");
}

#[test]
fn a2a_worker_sink_publishes_plan_extension_to_task_stream() {
    let task_id = "task-plan".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: None,
        status: TaskStatus::Working,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };
    let plan = harn_vm::llm::plan::normalize_plan_tool_call(
        harn_vm::llm::plan::UPDATE_PLAN_TOOL,
        &serde_json::json!({
            "explanation": "Plan the task.",
            "plan": [{"step": "Inspect files.", "status": "pending"}],
        }),
    );

    sink.handle_event(&harn_vm::agent_events::AgentEvent::Plan {
        session_id: super::a2a_worker_session_id(&task_id),
        plan,
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    let event = task
        .events
        .iter()
        .find(|event| event.get("type").and_then(JsonValue::as_str) == Some("harn_plan"))
        .expect("harn_plan event");
    assert_eq!(event["taskId"], task_id);
    assert_eq!(event["entries"][0]["content"], "Inspect files.");
    assert_eq!(event["plan"]["schema_version"], "harn.plan.v1");
}

#[test]
fn a2a_worker_sink_publishes_progress_as_status_update() {
    let task_id = "task-progress".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: Some("ctx-progress".to_string()),
        status: TaskStatus::Working,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    sink.handle_event(&harn_vm::agent_events::AgentEvent::ProgressReported {
        session_id: super::a2a_worker_session_id(&task_id),
        message: Some("Patched stdlib API.".to_string()),
        entries: serde_json::json!([
            {"content": "Implement progress helper.", "status": "completed", "priority": "high"},
            {"content": "Run conformance.", "status": "in_progress"}
        ]),
        replace: true,
        metadata: serde_json::json!({"source": "agent_progress"}),
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    assert_eq!(task.status, TaskStatus::Working);
    let event = task
        .events
        .iter()
        .find(|event| event.get("kind").and_then(JsonValue::as_str) == Some("status-update"))
        .expect("status-update event");
    assert_eq!(event["type"], "status");
    assert_eq!(event["taskId"], task_id);
    assert_eq!(event["contextId"], "ctx-progress");
    assert_eq!(event["final"], false);
    assert_eq!(event["status"]["state"], "working");
    assert!(event["status"]["message"]["id"].is_string());
    assert_eq!(event["status"]["message"]["role"], "agent");
    assert_eq!(event["status"]["message"]["parts"][0]["kind"], "text");
    assert_eq!(event["status"]["message"]["parts"][0]["type"], "text");
    assert_eq!(
        event["status"]["message"]["parts"][0]["text"],
        "Patched stdlib API.\n\nPlan:\n- [x] Implement progress helper. (priority: high)\n- [ ] Run conformance. (in progress)"
    );
}

#[test]
fn a2a_worker_sink_publishes_message_only_progress_status() {
    let task_id = "task-progress-message".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: None,
        status: TaskStatus::Working,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    sink.handle_event(&harn_vm::agent_events::AgentEvent::ProgressReported {
        session_id: super::a2a_worker_session_id(&task_id),
        message: Some("Working through verification.".to_string()),
        entries: serde_json::json!([]),
        replace: true,
        metadata: serde_json::json!({}),
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    let event = task
        .events
        .iter()
        .find(|event| event.get("kind").and_then(JsonValue::as_str) == Some("status-update"))
        .expect("status-update event");
    assert_eq!(event["status"]["state"], "working");
    assert_eq!(
        event["status"]["message"]["parts"][0]["text"],
        "Working through verification."
    );
    assert!(event.get("contextId").is_none());
}

#[test]
fn a2a_worker_sink_does_not_override_terminal_task_with_progress() {
    let task_id = "task-progress-terminal".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: None,
        status: TaskStatus::Completed,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    sink.handle_event(&harn_vm::agent_events::AgentEvent::ProgressReported {
        session_id: super::a2a_worker_session_id(&task_id),
        message: Some("This should not revive the task.".to_string()),
        entries: serde_json::json!([
            {"content": "Ignored progress.", "status": "in_progress"}
        ]),
        replace: true,
        metadata: serde_json::json!({}),
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(
        task.events.is_empty(),
        "terminal task should not publish progress events: {:?}",
        task.events
    );
}

#[tokio::test(flavor = "current_thread")]
async fn worker_event_emitted_during_dispatch_streams_to_task_subscribers() {
    // End-to-end: a Harn function that emits a `WorkerUpdate`
    // through the canonical sink registry must surface as a task
    // event on the A2A SSE stream. This is the integration that
    // closes harn#703's A2A leg — verifying the dispatch wraps
    // execution in the agent-session id the sink subscribes to.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn run(task: string) -> string {
  return task
}
",
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));

    let task_id = "task-stream-worker".to_string();
    let session_id = super::a2a_worker_session_id(&task_id);
    // Pre-stage a task so the A2aWorkerSink has somewhere to
    // deliver. Subscribe before emitting so the SSE channel
    // captures the event live.
    {
        let mut tasks = server.tasks.lock().expect("tasks");
        tasks.insert(
            task_id.clone(),
            TaskState {
                id: task_id.clone(),
                context_id: None,
                status: TaskStatus::Working,
                history: Vec::new(),
                artifacts: Vec::new(),
                metadata: BTreeMap::new(),
                events: Vec::new(),
                subscribers: Vec::new(),
                cancel_token: None,
            },
        );
    }
    let mut subscriber = server.subscribe(&task_id).expect("subscriber");
    let sink: Arc<dyn harn_vm::agent_events::AgentEventSink> = Arc::new(super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: server.tasks.clone(),
    });
    harn_vm::agent_events::register_sink(session_id.clone(), sink);
    let _sink_cleanup = SessionSinkCleanup(session_id.clone());
    // Push the session so emit_event routes correctly even though
    // we're not going through the full dispatch wrapper here. In
    // production, `invoke_function` does this via the
    // `agent_session_id` request field.
    harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
    let _guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());

    harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::WorkerUpdate {
        session_id: session_id.clone(),
        worker_id: "w-1".into(),
        worker_name: "review".into(),
        worker_task: "review pr".into(),
        worker_mode: "delegated_stage".into(),
        event: harn_vm::agent_events::WorkerEvent::WorkerCompleted,
        status: "completed".into(),
        metadata: serde_json::json!({"finished_at": "0193..."}),
        audit: None,
    });

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), subscriber.next())
        .await
        .expect("worker event emitted")
        .expect("subscriber stream open");
    assert_eq!(
        event.pointer("/result/type").and_then(JsonValue::as_str),
        Some("worker_update"),
        "got: {event}"
    );
    assert_eq!(
        event.pointer("/result/event").and_then(JsonValue::as_str),
        Some("WorkerCompleted")
    );
    assert_eq!(
        event.pointer("/result/status").and_then(JsonValue::as_str),
        Some("completed")
    );
    assert_eq!(
        event
            .pointer("/result/terminal")
            .and_then(JsonValue::as_bool),
        Some(true)
    );
}

struct SessionSinkCleanup(String);

impl Drop for SessionSinkCleanup {
    fn drop(&mut self) {
        harn_vm::agent_events::clear_session_sinks(&self.0);
    }
}

#[test]
fn task_status_renders_a2a_0_3_0_state_strings() {
    // The wire-level state names follow A2A 0.3.0's hyphenated
    // schema. Pin them so a typo can't silently regress the public
    // surface of the SSE / push-config payloads.
    assert_eq!(TaskStatus::Submitted.as_str(), "submitted");
    assert_eq!(TaskStatus::Working.as_str(), "working");
    assert_eq!(TaskStatus::InputRequired.as_str(), "input-required");
    assert_eq!(TaskStatus::AuthRequired.as_str(), "auth-required");
    assert_eq!(TaskStatus::Completed.as_str(), "completed");
    assert_eq!(TaskStatus::Failed.as_str(), "failed");
    assert_eq!(TaskStatus::Cancelled.as_str(), "cancelled");
    assert_eq!(TaskStatus::Rejected.as_str(), "rejected");

    // Terminal states cannot be cancelled or transitioned out of.
    // `input-required` and `auth-required` are pause states — the
    // task is alive and the client is expected to act on it.
    assert!(TaskStatus::Completed.is_terminal());
    assert!(TaskStatus::Failed.is_terminal());
    assert!(TaskStatus::Cancelled.is_terminal());
    assert!(TaskStatus::Rejected.is_terminal());
    assert!(!TaskStatus::Submitted.is_terminal());
    assert!(!TaskStatus::Working.is_terminal());
    assert!(!TaskStatus::InputRequired.is_terminal());
    assert!(!TaskStatus::AuthRequired.is_terminal());
}

#[test]
fn hitl_requested_event_transitions_task_into_input_required() {
    // A2A 0.3.0 `input-required` is the wire signal a client uses
    // to know the task is paused on a HITL waitpoint. Our sink
    // listens for the canonical `AgentEvent::HitlRequested` emitted
    // by the HITL primitives in `harn-vm` and flips task status
    // accordingly. `HitlResolved` flips it back to `working` so
    // subscribers can observe the resume before the task ultimately
    // completes / fails.
    let task_id = "task-hitl".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: None,
        status: TaskStatus::Working,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    sink.handle_event(&harn_vm::agent_events::AgentEvent::HitlRequested {
        session_id: super::a2a_worker_session_id(&task_id),
        request_id: "hitl_question_t1_1".into(),
        kind: "question".into(),
        payload: serde_json::json!({"prompt": "Approve?"}),
    });

    {
        let tasks = tasks.lock().expect("tasks");
        let task = tasks.get(&task_id).expect("task");
        assert_eq!(task.status, TaskStatus::InputRequired);
        let hitl_event = task
            .events
            .iter()
            .find(|event| event.get("type").and_then(JsonValue::as_str) == Some("hitl"))
            .expect("hitl event");
        assert_eq!(hitl_event["phase"], "requested");
        assert_eq!(hitl_event["kind"], "question");
        assert_eq!(hitl_event["requestId"], "hitl_question_t1_1");
        assert_eq!(hitl_event["payload"]["prompt"], "Approve?");
        let status_event = task
            .events
            .iter()
            .filter_map(|event| {
                if event.get("type").and_then(JsonValue::as_str) == Some("status") {
                    event.pointer("/status/state").and_then(JsonValue::as_str)
                } else {
                    None
                }
            })
            .next_back()
            .expect("status event");
        assert_eq!(status_event, "input-required");
    }

    sink.handle_event(&harn_vm::agent_events::AgentEvent::HitlResolved {
        session_id: super::a2a_worker_session_id(&task_id),
        request_id: "hitl_question_t1_1".into(),
        kind: "question".into(),
        outcome: "answered".into(),
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    assert_eq!(task.status, TaskStatus::Working);
    let resolved_event = task
        .events
        .iter()
        .rfind(|event| event.get("type").and_then(JsonValue::as_str) == Some("hitl"))
        .expect("resolved hitl event");
    assert_eq!(resolved_event["phase"], "resolved");
    assert_eq!(resolved_event["outcome"], "answered");
}

#[test]
fn hitl_requested_event_does_not_override_terminal_task() {
    // The waitpoint emit can race with cancellation/completion.
    // Once a task is terminal, a stray `HitlRequested` must not
    // reanimate it into `input-required`.
    let task_id = "task-terminal".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: None,
        status: TaskStatus::Cancelled,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    sink.handle_event(&harn_vm::agent_events::AgentEvent::HitlRequested {
        session_id: super::a2a_worker_session_id(&task_id),
        request_id: "late".into(),
        kind: "question".into(),
        payload: serde_json::json!({}),
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    assert_eq!(task.status, TaskStatus::Cancelled);
    // No HITL event is published either — the late emission is
    // dropped wholesale rather than partially recorded.
    assert!(
        task.events
            .iter()
            .all(|event| event.get("type").and_then(JsonValue::as_str) != Some("hitl")),
        "events: {:?}",
        task.events
    );
}

#[tokio::test]
async fn auth_policy_denial_returns_unauthorized_without_storing_task() {
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret-key",
    );
    let request = harn_vm::jsonrpc::request(
        "rej-1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [{"type": "text", "text": "hello"}]
            },
            "configuration": {"blocking": true}
        }),
    );

    let processed = server
        .clone()
        .process_rpc(request, AuthRequest::default())
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };

    assert_eq!(processed.status, Some(StatusCode::UNAUTHORIZED));
    assert_eq!(response["error"]["code"], -32000, "got: {response}");
    assert!(
        server.tasks.lock().expect("tasks poisoned").is_empty(),
        "auth failures should not persist caller-provided task content"
    );
    assert!(
        processed.auth_challenge.is_some(),
        "auth failures should advertise a challenge"
    );
}

#[tokio::test]
async fn auth_required_state_surfaces_when_script_raises_auth_error() {
    // Mid-task downstream auth failure: the script raises an
    // auth-classified error (e.g. an LLM/HTTP 401 surfaces through
    // `error_to_category`). The dispatch returns `Execution(...)`
    // wrapping the message; the adapter classifies it via
    // `harn_vm::value::classify_error_message` and flips the task
    // into the non-terminal `auth-required` state so the client
    // can refresh credentials and resubscribe.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn triage(task: string) -> string {
  // The auth classifier matches "401" (HTTP status code) and well-
  // known error identifier substrings. This message hits both so the
  // path is exercised regardless of which heuristic fires first.
  throw "downstream HTTP 401: invalid_api_key"
  return task
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let request = harn_vm::jsonrpc::request(
        "auth-1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [{"type": "text", "text": "hello"}]
            },
            "configuration": {"blocking": true}
        }),
    );

    let processed = server.process_rpc(request, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };

    assert_eq!(
        response["result"]["status"]["state"], "auth-required",
        "got: {response}"
    );
}

#[test]
fn artifact_metadata_includes_timestamp_and_kind() {
    let harn_artifact = json!({
        "_type": "artifact",
        "id": "report",
        "kind": "file",
        "title": "report.bin",
        "data": {
            "bytes": "AAEC/w==",
            "mimeType": "application/octet-stream",
            "name": "report.bin"
        }
    });

    let a2a_artifact = super::a2a_artifact_from_harn_artifact(&harn_artifact);
    let metadata = a2a_artifact["metadata"]
        .as_object()
        .expect("metadata object");
    let timestamp = metadata
        .get("timestamp")
        .and_then(JsonValue::as_str)
        .expect("timestamp string");
    // RFC3339: "YYYY-MM-DDTHH:MM:SS" plus zone — at minimum 19 chars.
    assert!(
        timestamp.len() >= 19 && timestamp.contains('T'),
        "timestamp not RFC3339: {timestamp}"
    );
    assert_eq!(
        metadata.get("artifact_kind").and_then(JsonValue::as_str),
        Some("file")
    );
    assert_eq!(a2a_artifact["artifactId"], "report");
    assert_eq!(a2a_artifact["name"], "report.bin");
}

#[tokio::test]
async fn send_message_surfaces_text_and_binary_outputs_as_separate_artifacts() {
    // Acceptance criterion for harn#892: a script that produces both
    // text and binary outputs must surface them as separate
    // `Artifact` objects on the resulting task — not collapse them
    // into the legacy empty `[]`.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn render_report(task: string) -> dict {
  return {
    visible_text: "summary for " + task,
    artifacts: [
      artifact({
        kind: "file",
        id: "report-bin",
        title: "report.bin",
        data: {
          bytes: "AAEC/w==",
          mimeType: "application/octet-stream",
          name: "report.bin"
        }
      }),
      artifact({
        kind: "data",
        id: "report-summary",
        title: "summary",
        data: {rows: 3, status: "ok"}
      })
    ]
  }
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let request = harn_vm::jsonrpc::request(
        "artifacts-1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "render_report"},
                "parts": [{"type": "text", "text": "audit-2026-05"}]
            }
        }),
    );

    let processed = server.process_rpc(request, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };

    assert_eq!(response["result"]["status"]["state"], "completed");
    let artifacts = response["result"]["artifacts"]
        .as_array()
        .expect("artifacts array");
    assert_eq!(artifacts.len(), 2, "got: {response}");

    let by_id: BTreeMap<&str, &JsonValue> = artifacts
        .iter()
        .map(|artifact| {
            (
                artifact["artifactId"].as_str().expect("artifactId"),
                artifact,
            )
        })
        .collect();

    let file_artifact = by_id.get("report-bin").expect("file artifact");
    assert_eq!(file_artifact["name"], "report.bin");
    assert_eq!(file_artifact["parts"][0]["type"], "file");
    assert_eq!(file_artifact["parts"][0]["file"]["bytes"], "AAEC/w==");
    assert_eq!(
        file_artifact["parts"][0]["file"]["mimeType"],
        "application/octet-stream"
    );
    assert!(
        file_artifact["metadata"]["timestamp"].is_string(),
        "missing timestamp on file artifact"
    );

    let data_artifact = by_id.get("report-summary").expect("data artifact");
    assert_eq!(data_artifact["parts"][0]["type"], "data");
    assert_eq!(data_artifact["parts"][0]["data"]["rows"], 3);
    assert!(
        data_artifact["metadata"]["timestamp"].is_string(),
        "missing timestamp on data artifact"
    );
}

#[test]
fn tool_call_completed_emits_artifact_update_event() {
    // A `ToolCallUpdate` with `status: completed` and a `raw_output`
    // must materialise as an A2A `TaskArtifactUpdateEvent` on the
    // task's event stream and as an entry on `task.artifacts`. The
    // canonical `tool_call_id` becomes the artifact's stable id so
    // the streaming event and the eventual `tasks/get` shape share
    // identity.
    let task_id = "task-tool-output".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: Some("ctx-1".into()),
        status: TaskStatus::Working,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    sink.handle_event(&harn_vm::agent_events::AgentEvent::ToolCallUpdate {
        session_id: super::a2a_worker_session_id(&task_id),
        tool_call_id: "tc-42".into(),
        tool_name: "search_files".into(),
        status: harn_vm::agent_events::ToolCallStatus::Completed,
        raw_output: Some(json!({"matches": ["a.rs", "b.rs"]})),
        error: None,
        duration_ms: Some(12),
        execution_duration_ms: Some(10),
        error_category: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    assert_eq!(task.artifacts.len(), 1, "tool output not stored");
    let stored = &task.artifacts[0];
    assert_eq!(stored["artifactId"], "tool-tc-42");
    assert_eq!(stored["name"], "search_files");
    assert_eq!(stored["parts"][0]["type"], "data");
    assert_eq!(stored["parts"][0]["data"]["matches"][0], "a.rs");
    assert_eq!(stored["metadata"]["tool_call_id"], "tc-42");
    assert!(stored["metadata"]["timestamp"].is_string());

    let event = task
        .events
        .iter()
        .find(|event| event.get("kind").and_then(JsonValue::as_str) == Some("artifact-update"))
        .expect("artifact-update event");
    assert_eq!(event["taskId"], task_id);
    assert_eq!(event["contextId"], "ctx-1");
    assert_eq!(event["append"], false);
    assert_eq!(event["lastChunk"], true);
    assert_eq!(event["artifact"]["artifactId"], "tool-tc-42");
}

#[test]
fn agent_artifact_event_emits_artifact_update() {
    let task_id = "task-agent-artifact".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: Some("ctx-artifacts".into()),
        status: TaskStatus::Working,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    sink.handle_event(&harn_vm::agent_events::AgentEvent::Artifact {
        session_id: super::a2a_worker_session_id(&task_id),
        artifact_id: "artifact-chart-1".into(),
        kind: "vega-lite".into(),
        title: Some("Build throughput".into()),
        mime_type: "application/vnd.vegalite.v5+json".into(),
        spec: json!({
            "mark": "bar",
            "data": {"values": [{"name": "a", "count": 2}]},
            "encoding": {"x": {"field": "name"}, "y": {"field": "count"}}
        }),
        fallback: "Build throughput (bar chart)".into(),
        size_bytes: 128,
        provenance: json!({"source": "agent"}),
        metadata: json!({"unit": "builds"}),
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    assert_eq!(task.artifacts.len(), 1, "artifact event not stored");
    let stored = &task.artifacts[0];
    assert_eq!(stored["artifactId"], "artifact-chart-1");
    assert_eq!(stored["name"], "Build throughput");
    assert_eq!(stored["parts"][0]["type"], "data");
    assert_eq!(stored["parts"][0]["data"]["kind"], "vega-lite");
    assert_eq!(
        stored["parts"][0]["data"]["mimeType"],
        "application/vnd.vegalite.v5+json"
    );
    assert_eq!(stored["parts"][0]["data"]["spec"]["mark"], "bar");
    assert_eq!(stored["parts"][1]["type"], "text");
    assert_eq!(stored["parts"][1]["text"], "Build throughput (bar chart)");
    assert_eq!(stored["metadata"]["artifact_kind"], "vega-lite");
    assert_eq!(stored["metadata"]["size_bytes"], 128);
    assert_eq!(stored["metadata"]["provenance"]["source"], "agent");
    assert_eq!(stored["metadata"]["harn_metadata"]["unit"], "builds");
    assert!(stored["metadata"]["timestamp"].is_string());

    let event = task
        .events
        .iter()
        .find(|event| event.get("kind").and_then(JsonValue::as_str) == Some("artifact-update"))
        .expect("artifact-update event");
    assert_eq!(event["taskId"], task_id);
    assert_eq!(event["contextId"], "ctx-artifacts");
    assert_eq!(event["artifact"]["artifactId"], "artifact-chart-1");
}

#[test]
fn tool_call_pending_does_not_emit_artifact_update() {
    // Only terminal `Completed` updates with a `raw_output` payload
    // map to artifacts; intermediate streaming chunks (Pending /
    // InProgress / partial-parse) must stay silent so we don't pollute
    // the artifact list with placeholders.
    let task_id = "task-tool-pending".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: None,
        status: TaskStatus::Working,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    sink.handle_event(&harn_vm::agent_events::AgentEvent::ToolCallUpdate {
        session_id: super::a2a_worker_session_id(&task_id),
        tool_call_id: "tc-99".into(),
        tool_name: "search_files".into(),
        status: harn_vm::agent_events::ToolCallStatus::InProgress,
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

    sink.handle_event(&harn_vm::agent_events::AgentEvent::ToolCallUpdate {
        session_id: super::a2a_worker_session_id(&task_id),
        tool_call_id: "tc-100".into(),
        tool_name: "search_files".into(),
        status: harn_vm::agent_events::ToolCallStatus::Failed,
        raw_output: None,
        error: Some("boom".into()),
        duration_ms: Some(1),
        execution_duration_ms: Some(1),
        error_category: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    assert!(
        task.artifacts.is_empty(),
        "non-terminal tool calls must not emit artifacts"
    );
    assert!(
        !task
            .events
            .iter()
            .any(|event| event.get("kind").and_then(JsonValue::as_str) == Some("artifact-update")),
        "no artifact-update events should be emitted for non-Completed tool updates",
    );
}
