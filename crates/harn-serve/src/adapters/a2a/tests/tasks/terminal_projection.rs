use super::*;
use crate::adapters::a2a::tasks::agent_terminal_projection;

#[test]
fn agent_result_terminal_projects_to_a2a_status_and_pause_metadata() {
    use harn_vm::agent_events::AgentTerminalKind as Kind;

    for (kind, expected) in [
        (Kind::Natural, TaskStatus::Completed),
        (Kind::UserCancelled, TaskStatus::Cancelled),
        (Kind::PolicyBudget, TaskStatus::Cancelled),
        (Kind::CompletionUnverified, TaskStatus::Failed),
        (Kind::PolicyNoProgress, TaskStatus::Cancelled),
        (Kind::PolicyGuardrail, TaskStatus::Cancelled),
        (Kind::PolicyStop, TaskStatus::Cancelled),
        (Kind::ProviderError, TaskStatus::Failed),
        (Kind::RuntimeError, TaskStatus::Failed),
        (Kind::Unknown, TaskStatus::Failed),
    ] {
        let projection = agent_terminal_projection(&json!({
            "terminal": {
                "kind": kind.as_str(),
                "reason": "test",
                "owner": kind.owner(),
            }
        }))
        .expect("typed terminal projection");
        assert_eq!(projection.status, expected, "kind={}", kind.as_str());
        assert_eq!(projection.harn_metadata["terminal"]["kind"], kind.as_str());
    }

    let suspended = agent_terminal_projection(&json!({
        "terminal": {"kind": "suspended", "reason": "ci", "owner": "agent"},
        "handle": {"id": "suspend-1", "conditions": {"event": "ci.finished"}},
    }))
    .expect("suspended projection");
    assert_eq!(suspended.status, TaskStatus::Working);
    assert_eq!(suspended.harn_metadata["pause"]["state"], "paused");
    assert_eq!(
        suspended.harn_metadata["pause"]["handle"]["id"],
        "suspend-1"
    );
}

#[test]
fn complete_task_uses_agent_result_terminal_instead_of_dispatch_success() {
    let (_dir, server) = test_server("pub fn run() -> string { return \"ok\" }\n");

    for (task_id, terminal, expected_status) in [
        (
            "task-policy",
            json!({"kind": "policy_budget", "reason": "max_iterations", "owner": "policy"}),
            "cancelled",
        ),
        (
            "task-provider",
            json!({"kind": "provider_error", "reason": "timeout", "owner": "provider"}),
            "failed",
        ),
        (
            "task-suspended",
            json!({"kind": "suspended", "reason": "ci", "owner": "agent"}),
            "working",
        ),
    ] {
        server.tasks.lock().expect("tasks").insert(
            task_id.to_string(),
            TaskState {
                id: task_id.to_string(),
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
        server.complete_task(
            task_id,
            CallResponse {
                function: "run".to_string(),
                value: json!({
                    "text": "result",
                    "terminal": terminal.clone(),
                    "handle": {"id": "suspend-1"},
                }),
                printed_output: String::new(),
                trace_id: harn_vm::TraceId("trace-terminal".to_string()),
                cached: false,
                duration_ms: 0,
            },
        );

        let task = server.task_json(task_id);
        assert_eq!(task["status"]["state"], expected_status);
        assert_eq!(task["metadata"]["harn"]["terminal"], terminal);
        if expected_status == "working" {
            assert_eq!(task["metadata"]["harn"]["pause"]["state"], "paused");
        }
    }
}
