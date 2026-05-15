//! A2A worker-event conversion into task stream events.
use super::schema::*;
use super::*;

pub(super) struct A2aWorkerSink {
    pub(super) task_id: String,
    pub(super) tasks: TaskStore,
}

impl harn_vm::agent_events::AgentEventSink for A2aWorkerSink {
    fn handle_event(&self, event: &harn_vm::agent_events::AgentEvent) {
        let payload = match event {
            harn_vm::agent_events::AgentEvent::ToolCallUpdate {
                tool_call_id,
                tool_name,
                status,
                raw_output: Some(output),
                ..
            } if *status == harn_vm::agent_events::ToolCallStatus::Completed => {
                self.emit_tool_artifact(tool_call_id, tool_name, output);
                return;
            }
            harn_vm::agent_events::AgentEvent::WorkerUpdate {
                worker_id,
                worker_name,
                worker_task,
                worker_mode,
                event,
                status,
                metadata,
                audit,
                ..
            } => {
                let mut payload = json!({
                    "type": "worker_update",
                    "taskId": self.task_id,
                    "workerId": worker_id,
                    "workerName": worker_name,
                    "workerTask": worker_task,
                    "workerMode": worker_mode,
                    "event": event.as_str(),
                    "status": status,
                    "terminal": event.is_terminal(),
                    "metadata": metadata,
                });
                if let Some(audit) = audit {
                    payload["audit"] = audit.clone();
                }
                payload
            }
            harn_vm::agent_events::AgentEvent::Plan { plan, .. }
                if plan.get("schema_version").and_then(JsonValue::as_str)
                    == Some(harn_vm::llm::plan::PLAN_SCHEMA_VERSION) =>
            {
                json!({
                    "type": "harn_plan",
                    "taskId": self.task_id,
                    "entries": harn_vm::llm::plan::plan_entries(plan),
                    "plan": plan,
                })
            }
            harn_vm::agent_events::AgentEvent::ProgressReported {
                message, entries, ..
            } => {
                self.emit_progress_status(message.as_deref(), entries);
                return;
            }
            harn_vm::agent_events::AgentEvent::HitlRequested {
                request_id,
                kind,
                payload,
                ..
            } => {
                self.transition_input_required(request_id, kind, payload);
                return;
            }
            harn_vm::agent_events::AgentEvent::HitlResolved {
                request_id,
                kind,
                outcome,
                ..
            } => {
                self.resolve_input_required(request_id, kind, outcome);
                return;
            }
            _ => return,
        };
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(&self.task_id) else {
                return;
            };
            publish_locked(task, payload);
            task_to_json(task)
        };
        // No `deliver_push` here: worker_update events stream live to
        // active subscribers but don't fire push-config webhooks. Push
        // delivery is reserved for the canonical task lifecycle
        // transitions so high-volume worker traffic doesn't flood
        // outbound HTTP endpoints.
        let _ = task_for_push;
    }
}

impl A2aWorkerSink {
    /// Translate a completed tool call's output into an A2A
    /// `TaskArtifactUpdateEvent` and append the resulting artifact to
    /// the task's stored artifact list. Each tool call materialises as
    /// a single artifact (`lastChunk: true`, `append: false`) keyed by
    /// the model-issued `tool_call_id` so streaming subscribers and
    /// `tasks/get` callers see the same canonical shape.
    fn emit_tool_artifact(&self, tool_call_id: &str, tool_name: &str, output: &JsonValue) {
        let artifact = tool_output_artifact(tool_call_id, tool_name, output);
        let context_id = {
            let tasks = self.tasks.lock().expect("tasks poisoned");
            tasks
                .get(&self.task_id)
                .and_then(|task| task.context_id.clone())
        };
        let mut event = json!({
            "kind": "artifact-update",
            "taskId": self.task_id,
            "artifact": artifact.clone(),
            "append": false,
            "lastChunk": true,
        });
        if let Some(context_id) = context_id {
            event["contextId"] = JsonValue::String(context_id);
        }
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let Some(task) = tasks.get_mut(&self.task_id) else {
            return;
        };
        if task.status.is_terminal() {
            return;
        }
        task.artifacts.push(artifact);
        publish_locked(task, event);
    }

    fn emit_progress_status(&self, message: Option<&str>, entries: &JsonValue) {
        let Some(text) = render_progress_message(message, entries) else {
            return;
        };
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let Some(task) = tasks.get_mut(&self.task_id) else {
            return;
        };
        if task.status.is_terminal() {
            return;
        }
        task.status = TaskStatus::Working;
        let mut event = json!({
            "kind": "status-update",
            "type": "status",
            "taskId": self.task_id,
            "status": {
                "state": TaskStatus::Working.as_str(),
                "message": {
                    "id": Uuid::now_v7().to_string(),
                    "role": "agent",
                    "parts": [
                        {
                            "kind": "text",
                            "type": "text",
                            "text": text,
                        }
                    ],
                },
            },
            "final": false,
        });
        if let Some(context_id) = task.context_id.as_ref() {
            event["contextId"] = JsonValue::String(context_id.clone());
        }
        publish_locked(task, event);
    }

    /// Flip the task into `input-required` while a HITL primitive is
    /// blocked waiting for a response. The script remains suspended on
    /// a waitpoint; subscribers see two events — a structured `hitl`
    /// extension event carrying the request payload, then the canonical
    /// `status` transition. Idempotent for repeat HITL requests inside
    /// the same task: only the first transitions the status.
    ///
    /// No push-config webhook delivery here, mirroring the
    /// `worker_update` policy: HITL transitions stream live to active
    /// SSE subscribers and surface on `tasks/get`, but high-frequency
    /// status flips don't fan out to outbound webhook endpoints.
    fn transition_input_required(&self, request_id: &str, kind: &str, payload: &JsonValue) {
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let Some(task) = tasks.get_mut(&self.task_id) else {
            return;
        };
        // Don't override a terminal/cancelled task: the waitpoint
        // emit can race the cancel path. Once the task is dead it
        // must stay dead.
        if task.status.is_terminal() {
            return;
        }
        let hitl_event = json!({
            "type": "hitl",
            "taskId": self.task_id,
            "phase": "requested",
            "requestId": request_id,
            "kind": kind,
            "payload": payload,
        });
        publish_locked(task, hitl_event);
        if task.status != TaskStatus::InputRequired {
            task.status = TaskStatus::InputRequired;
            publish_locked(task, status_event(&self.task_id, TaskStatus::InputRequired));
        }
    }

    /// Companion to `transition_input_required`. Flip back to `working`
    /// once the waitpoint resolves so subscribers see the task resume
    /// (or terminate naturally on the next tick if the script returned
    /// from the HITL call). Only flips out of `input-required`; if a
    /// later `auth-required` / cancellation snuck in, leave it.
    fn resolve_input_required(&self, request_id: &str, kind: &str, outcome: &str) {
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let Some(task) = tasks.get_mut(&self.task_id) else {
            return;
        };
        let hitl_event = json!({
            "type": "hitl",
            "taskId": self.task_id,
            "phase": "resolved",
            "requestId": request_id,
            "kind": kind,
            "outcome": outcome,
        });
        publish_locked(task, hitl_event);
        if task.status == TaskStatus::InputRequired {
            task.status = TaskStatus::Working;
            publish_locked(task, status_event(&self.task_id, TaskStatus::Working));
        }
    }
}

fn render_progress_message(message: Option<&str>, entries: &JsonValue) -> Option<String> {
    let mut sections = Vec::new();
    if let Some(message) = message.map(str::trim).filter(|message| !message.is_empty()) {
        sections.push(message.to_string());
    }

    if let Some(entries) = entries.as_array().filter(|entries| !entries.is_empty()) {
        let mut lines = vec!["Plan:".to_string()];
        for entry in entries {
            let Some(content) = entry
                .get("content")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|content| !content.is_empty())
            else {
                continue;
            };
            let status = entry
                .get("status")
                .and_then(JsonValue::as_str)
                .unwrap_or("pending");
            let marker = if status == "completed" { "[x]" } else { "[ ]" };
            let mut line = format!("- {marker} {content}");
            let mut qualifiers = Vec::new();
            if !matches!(status, "pending" | "completed") {
                qualifiers.push(status.replace('_', " "));
            }
            if let Some(priority) = entry
                .get("priority")
                .and_then(JsonValue::as_str)
                .filter(|priority| !priority.is_empty())
            {
                qualifiers.push(format!("priority: {priority}"));
            }
            if !qualifiers.is_empty() {
                line.push_str(" (");
                line.push_str(&qualifiers.join(", "));
                line.push(')');
            }
            lines.push(line);
        }
        if lines.len() > 1 {
            sections.push(lines.join("\n"));
        }
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}
