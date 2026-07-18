use super::*;

struct FailingFlushSink;

impl harn_vm::agent_events::AgentEventSink for FailingFlushSink {
    fn handle_event(&self, _event: &harn_vm::agent_events::AgentEvent) {}

    fn flush(&self) -> harn_vm::agent_events::AgentEventSinkFlush<'_> {
        Box::pin(async {
            Err(harn_vm::agent_events::AgentEventSinkError::new(
                "a2a_test",
                "injected append failure",
            ))
        })
    }
}

fn test_task_params(text: &str) -> JsonValue {
    json!({
        "message": {
            "metadata": {"target_agent": "triage"},
            "parts": [{"type": "text", "text": text}]
        }
    })
}

#[tokio::test]
async fn waits_for_sink_failure_before_completing_and_clears_registration() {
    let (_dir, server) = test_server(
        r#"
import { agent_progress } from "std/agent/progress"

pub fn triage(task: string) -> string {
  agent_progress({message: "Persist this progress."})
  return task
}
"#,
    );
    let task = server
        .prepare_task(&test_task_params("hello"), AuthRequest::default())
        .await
        .unwrap_or_else(|_| panic!("prepare task"));
    let session_id = super::a2a_worker_session_id(&task.id);
    harn_vm::agent_events::register_sink(session_id.clone(), Arc::new(FailingFlushSink));

    server.run_task_to_completion(&task).await;

    let task_json = server.task_json(&task.id);
    assert_eq!(task_json["status"]["state"], "failed");
    assert!(task_json["history"]
        .as_array()
        .expect("task history")
        .iter()
        .any(|message| message.to_string().contains("injected append failure")));
    assert_eq!(
        harn_vm::agent_events::session_external_sink_count(&session_id),
        0
    );
}

#[tokio::test]
async fn cancellation_keeps_sink_failure_without_losing_cancelled_status() {
    let (_dir, server) = test_server(
        r#"
import { agent_progress } from "std/agent/progress"

pub fn triage(task: string) -> string {
  agent_progress({message: "Ready for cancellation."})
  while true {
    if is_cancelled() {
      return task
    }
  }
}
"#,
    );
    let task = server
        .prepare_task(&test_task_params("cancel me"), AuthRequest::default())
        .await
        .unwrap_or_else(|_| panic!("prepare task"));
    let session_id = super::a2a_worker_session_id(&task.id);
    harn_vm::agent_events::register_sink(session_id.clone(), Arc::new(FailingFlushSink));
    let mut events = server.subscribe(&task.id).expect("subscribe to task");
    let task_id = task.id.clone();
    let runner = {
        let server = server.clone();
        tokio::spawn(async move {
            server.run_task_to_completion(&task).await;
        })
    };

    while let Some(event) = events.next().await {
        if event.pointer("/result/kind").and_then(JsonValue::as_str) == Some("status-update") {
            break;
        }
    }
    let cancelled = server.cancel_task(&task_id).expect("cancel running task");
    assert_eq!(cancelled["status"]["state"], "cancelled");
    runner.await.expect("task runner");

    let task_json = server.task_json(&task_id);
    assert_eq!(task_json["status"]["state"], "cancelled");
    assert!(task_json["metadata"]["harn"]["persistenceError"]
        .as_str()
        .expect("cancelled task persistence error")
        .contains("injected append failure"));
    assert_eq!(
        harn_vm::agent_events::session_external_sink_count(&session_id),
        0
    );
}
