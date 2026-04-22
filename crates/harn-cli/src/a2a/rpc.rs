use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use super::task::{
    add_push_config, cancel_task, complete_task, create_task, delete_push_config, fail_task,
    is_task_cancelled, list_tasks, mark_task_working, task_snapshot, TaskStore,
};
use super::{execute_pipeline, write_http_response, write_sse_event, write_sse_header};

// A2A-standard JSON-RPC error codes (aligned with A2A protocol spec v1.0)
pub(super) const A2A_TASK_NOT_FOUND: i64 = -32001;
pub(super) const A2A_TASK_NOT_CANCELABLE: i64 = -32002;
pub(super) const A2A_UNSUPPORTED_OPERATION: i64 = -32003;
#[allow(dead_code)]
pub(super) const A2A_INVALID_PARAMS: i64 = -32602;
#[allow(dead_code)]
pub(super) const A2A_INTERNAL_ERROR: i64 = -32603;
pub(super) const A2A_VERSION_NOT_SUPPORTED: i64 = -32009;

/// Build a JSON-RPC success response wrapping a task's JSON representation.
pub(super) fn task_rpc_response(
    rpc_id: &serde_json::Value,
    task_json: serde_json::Value,
) -> serde_json::Value {
    harn_vm::jsonrpc::response(rpc_id.clone(), task_json)
}

/// Build a JSON-RPC error response.
pub(super) fn error_response(
    rpc_id: &serde_json::Value,
    code: i64,
    message: &str,
) -> serde_json::Value {
    harn_vm::jsonrpc::error_response(rpc_id.clone(), code, message)
}

/// Extract message text and context_id from a JSON-RPC params object.
pub(super) fn extract_message_params(parsed: &serde_json::Value) -> (String, Option<String>) {
    let task_text = parsed
        .pointer("/params/message/parts")
        .and_then(|parts| parts.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();

    let context_id = parsed
        .pointer("/params/contextId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    (task_text, context_id)
}

fn workflow_id_param<'a>(
    parsed: &'a serde_json::Value,
    method: &str,
) -> Result<&'a str, serde_json::Value> {
    parsed
        .pointer("/params/workflowId")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error_response(
                &parsed["id"],
                -32602,
                &format!("{method}: missing workflowId"),
            )
        })
}

fn workflow_name_param<'a>(
    parsed: &'a serde_json::Value,
    method: &str,
) -> Result<&'a str, serde_json::Value> {
    parsed
        .pointer("/params/name")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error_response(&parsed["id"], -32602, &format!("{method}: missing name")))
}

/// Handle a JSON-RPC request body, returning the JSON response string.
pub(super) async fn handle_jsonrpc(pipeline_path: &str, body: &str, store: &TaskStore) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            let resp = error_response(
                &serde_json::Value::Null,
                -32700,
                &format!("Parse error: {e}"),
            );
            return serde_json::to_string(&resp).unwrap_or_default();
        }
    };

    let rpc_id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = parsed.get("method").and_then(|m| m.as_str()).unwrap_or("");

    let resp = match method {
        "a2a.SendMessage" => {
            let (task_text, context_id) = extract_message_params(&parsed);

            if task_text.is_empty() {
                error_response(
                    &rpc_id,
                    -32602,
                    "Invalid params: no text part found in message",
                )
            } else {
                let push_config = parsed
                    .pointer("/params/configuration/pushNotificationConfig")
                    .cloned();
                let return_immediately = parsed
                    .pointer("/params/configuration/returnImmediately")
                    .and_then(serde_json::Value::as_bool)
                    .or_else(|| {
                        parsed
                            .pointer("/params/configuration/blocking")
                            .and_then(serde_json::Value::as_bool)
                            .map(|blocking| !blocking)
                    })
                    .unwrap_or(false);
                let task_id = create_task(store, &task_text, context_id, push_config);
                mark_task_working(store, &task_id);

                if is_task_cancelled(store, &task_id) {
                    let task_json = store.lock().unwrap().get(&task_id).unwrap().to_json();
                    task_rpc_response(&rpc_id, task_json)
                } else if return_immediately {
                    let pipeline = pipeline_path.to_string();
                    let task_text = task_text.clone();
                    let task_id_for_thread = task_id.clone();
                    let store_for_thread = store.clone();
                    std::thread::spawn(move || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("build A2A background runtime");
                        let result = runtime.block_on(execute_pipeline(&pipeline, &task_text));
                        match result {
                            Ok(output) => {
                                if !is_task_cancelled(&store_for_thread, &task_id_for_thread) {
                                    complete_task(&store_for_thread, &task_id_for_thread, &output);
                                }
                            }
                            Err(error) => {
                                fail_task(&store_for_thread, &task_id_for_thread, &error);
                            }
                        }
                    });
                    let task_json = store.lock().unwrap().get(&task_id).unwrap().to_json();
                    task_rpc_response(&rpc_id, task_json)
                } else {
                    match execute_pipeline(pipeline_path, &task_text).await {
                        Ok(output) => {
                            if is_task_cancelled(store, &task_id) {
                                let task_json =
                                    store.lock().unwrap().get(&task_id).unwrap().to_json();
                                task_rpc_response(&rpc_id, task_json)
                            } else {
                                complete_task(store, &task_id, &output);
                                let task_json =
                                    store.lock().unwrap().get(&task_id).unwrap().to_json();
                                task_rpc_response(&rpc_id, task_json)
                            }
                        }
                        Err(e) => {
                            fail_task(store, &task_id, &e);
                            error_response(&rpc_id, -32000, &format!("Pipeline error: {e}"))
                        }
                    }
                }
            }
        }
        "CreateTaskPushNotificationConfig" | "tasks/pushNotificationConfig/set" => {
            let task_id = parsed
                .pointer("/params/taskId")
                .or_else(|| parsed.pointer("/params/task_id"))
                .or_else(|| parsed.pointer("/params/id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let config = parsed
                .pointer("/params/pushNotificationConfig")
                .or_else(|| parsed.pointer("/params/config"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if task_id.is_empty() || !config.is_object() {
                error_response(
                    &rpc_id,
                    -32602,
                    "Invalid params: missing taskId or pushNotificationConfig",
                )
            } else {
                match add_push_config(store, task_id, config) {
                    Ok(config) => task_rpc_response(&rpc_id, config),
                    Err(msg) => error_response(&rpc_id, A2A_TASK_NOT_FOUND, &msg),
                }
            }
        }
        "GetTaskPushNotificationConfig" => {
            let task_id = parsed
                .pointer("/params/taskId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let config_id = parsed
                .pointer("/params/id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let config = task_snapshot(store, task_id).and_then(|task| {
                task.push_configs.into_iter().find(|config| {
                    config.get("id").and_then(serde_json::Value::as_str) == Some(config_id)
                })
            });
            match config {
                Some(config) => task_rpc_response(&rpc_id, config),
                None => error_response(
                    &rpc_id,
                    A2A_TASK_NOT_FOUND,
                    "Push notification config not found",
                ),
            }
        }
        "ListTaskPushNotificationConfigs" => {
            let task_id = parsed
                .pointer("/params/taskId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match task_snapshot(store, task_id) {
                Some(task) => task_rpc_response(
                    &rpc_id,
                    serde_json::json!({"configs": task.push_configs, "nextPageToken": ""}),
                ),
                None => error_response(&rpc_id, A2A_TASK_NOT_FOUND, "Task not found"),
            }
        }
        "DeleteTaskPushNotificationConfig" => {
            let task_id = parsed
                .pointer("/params/taskId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let config_id = parsed
                .pointer("/params/id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match delete_push_config(store, task_id, config_id) {
                Ok(()) => task_rpc_response(&rpc_id, serde_json::json!({})),
                Err(msg) => error_response(&rpc_id, A2A_TASK_NOT_FOUND, &msg),
            }
        }
        "a2a.GetTask" => {
            let task_id = parsed
                .pointer("/params/id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if task_id.is_empty() {
                error_response(&rpc_id, -32602, "Invalid params: missing task id")
            } else {
                let task_json = store.lock().unwrap().get(task_id).map(|t| t.to_json());
                match task_json {
                    Some(json) => task_rpc_response(&rpc_id, json),
                    None => error_response(
                        &rpc_id,
                        A2A_TASK_NOT_FOUND,
                        &format!("TaskNotFoundError: {task_id}"),
                    ),
                }
            }
        }
        "a2a.CancelTask" => {
            let task_id = parsed
                .pointer("/params/id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if task_id.is_empty() {
                error_response(&rpc_id, -32602, "Invalid params: missing task id")
            } else {
                match cancel_task(store, task_id) {
                    Ok(json) => task_rpc_response(&rpc_id, json),
                    Err(msg) => error_response(&rpc_id, A2A_TASK_NOT_CANCELABLE, &msg),
                }
            }
        }
        "a2a.ListTasks" => {
            let cursor = parsed.pointer("/params/cursor").and_then(|v| v.as_str());
            let limit = parsed
                .pointer("/params/limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let result = list_tasks(store, cursor, limit);
            task_rpc_response(&rpc_id, result)
        }
        "a2a.WorkflowSignal" | "harn.workflow.signal" => {
            let workflow_id = match workflow_id_param(&parsed, method) {
                Ok(value) => value,
                Err(response) => {
                    return serde_json::to_string(&response).unwrap_or_default();
                }
            };
            let name = match workflow_name_param(&parsed, method) {
                Ok(value) => value,
                Err(response) => {
                    return serde_json::to_string(&response).unwrap_or_default();
                }
            };
            let payload = parsed
                .pointer("/params/payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let base_dir = std::path::Path::new(pipeline_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            match harn_vm::workflow_signal_for_base(base_dir, workflow_id, name, payload) {
                Ok(result) => harn_vm::jsonrpc::response(rpc_id.clone(), result),
                Err(error) => error_response(&rpc_id, -32000, &error),
            }
        }
        "a2a.WorkflowQuery" | "harn.workflow.query" => {
            let workflow_id = match workflow_id_param(&parsed, method) {
                Ok(value) => value,
                Err(response) => {
                    return serde_json::to_string(&response).unwrap_or_default();
                }
            };
            let name = match workflow_name_param(&parsed, method) {
                Ok(value) => value,
                Err(response) => {
                    return serde_json::to_string(&response).unwrap_or_default();
                }
            };
            let base_dir = std::path::Path::new(pipeline_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            match harn_vm::workflow_query_for_base(base_dir, workflow_id, name) {
                Ok(result) => harn_vm::jsonrpc::response(rpc_id.clone(), result),
                Err(error) => error_response(&rpc_id, -32000, &error),
            }
        }
        "a2a.WorkflowUpdate" | "harn.workflow.update" => {
            let workflow_id = match workflow_id_param(&parsed, method) {
                Ok(value) => value,
                Err(response) => {
                    return serde_json::to_string(&response).unwrap_or_default();
                }
            };
            let name = match workflow_name_param(&parsed, method) {
                Ok(value) => value,
                Err(response) => {
                    return serde_json::to_string(&response).unwrap_or_default();
                }
            };
            let payload = parsed
                .pointer("/params/payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let timeout = parsed
                .pointer("/params/timeoutMs")
                .and_then(|value| value.as_u64())
                .unwrap_or(30_000);
            let base_dir = std::path::Path::new(pipeline_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            match harn_vm::workflow_update_for_base(
                base_dir,
                workflow_id,
                name,
                payload,
                std::time::Duration::from_millis(timeout),
            )
            .await
            {
                Ok(result) => harn_vm::jsonrpc::response(rpc_id.clone(), result),
                Err(error) => error_response(&rpc_id, -32000, &error),
            }
        }
        "a2a.WorkflowPause" | "harn.workflow.pause" => {
            let workflow_id = match workflow_id_param(&parsed, method) {
                Ok(value) => value,
                Err(response) => {
                    return serde_json::to_string(&response).unwrap_or_default();
                }
            };
            let base_dir = std::path::Path::new(pipeline_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            match harn_vm::workflow_pause_for_base(base_dir, workflow_id) {
                Ok(result) => harn_vm::jsonrpc::response(rpc_id.clone(), result),
                Err(error) => error_response(&rpc_id, -32000, &error),
            }
        }
        "a2a.WorkflowResume" | "harn.workflow.resume" => {
            let workflow_id = match workflow_id_param(&parsed, method) {
                Ok(value) => value,
                Err(response) => {
                    return serde_json::to_string(&response).unwrap_or_default();
                }
            };
            let base_dir = std::path::Path::new(pipeline_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            match harn_vm::workflow_resume_for_base(base_dir, workflow_id) {
                Ok(result) => harn_vm::jsonrpc::response(rpc_id.clone(), result),
                Err(error) => error_response(&rpc_id, -32000, &error),
            }
        }
        _ => error_response(
            &rpc_id,
            A2A_UNSUPPORTED_OPERATION,
            &format!("UnsupportedOperationError: {method}"),
        ),
    };

    serde_json::to_string(&resp).unwrap_or_default()
}

/// Handle `a2a.SendStreamingMessage`, sending SSE events for task status
/// updates and the final message.
pub(super) async fn handle_streaming_request(
    stream: &mut (impl AsyncWriteExt + AsyncReadExt + Unpin),
    pipeline_path: &str,
    body: &str,
    store: &TaskStore,
) {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            let resp = error_response(
                &serde_json::Value::Null,
                -32700,
                &format!("Parse error: {e}"),
            );
            let resp_bytes = serde_json::to_string(&resp).unwrap_or_default();
            let _ =
                write_http_response(stream, 200, "OK", "application/json", resp_bytes.as_bytes())
                    .await;
            return;
        }
    };

    let rpc_id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let (task_text, context_id) = extract_message_params(&parsed);

    if task_text.is_empty() {
        let resp = error_response(
            &rpc_id,
            -32602,
            "Invalid params: no text part found in message",
        );
        let resp_bytes = serde_json::to_string(&resp).unwrap_or_default();
        let _ =
            write_http_response(stream, 200, "OK", "application/json", resp_bytes.as_bytes()).await;
        return;
    }

    let task_id = create_task(store, &task_text, context_id, None);

    if write_sse_header(stream).await.is_err() {
        return;
    }

    let submitted_event = serde_json::json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "result": {
            "type": "status",
            "taskId": task_id,
            "status": {"state": "submitted"}
        }
    });
    if write_sse_event(stream, "message", &submitted_event)
        .await
        .is_err()
    {
        return;
    }

    mark_task_working(store, &task_id);
    let working_event = serde_json::json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "result": {
            "type": "status",
            "taskId": task_id,
            "status": {"state": "working"}
        }
    });
    if write_sse_event(stream, "message", &working_event)
        .await
        .is_err()
    {
        return;
    }

    match execute_pipeline(pipeline_path, &task_text).await {
        Ok(output) => {
            if is_task_cancelled(store, &task_id) {
                let cancelled_event = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "result": {
                        "type": "status",
                        "taskId": task_id,
                        "status": {"state": "cancelled"}
                    }
                });
                let _ = write_sse_event(stream, "message", &cancelled_event).await;
            } else {
                let message_id = Uuid::now_v7().to_string();
                let message_event = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "result": {
                        "type": "message",
                        "taskId": task_id,
                        "message": {
                            "id": message_id,
                            "role": "agent",
                            "parts": [{"type": "text", "text": output.trim_end()}]
                        }
                    }
                });
                let _ = write_sse_event(stream, "message", &message_event).await;

                complete_task(store, &task_id, &output);

                let completed_event = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "result": {
                        "type": "status",
                        "taskId": task_id,
                        "status": {"state": "completed"}
                    }
                });
                let _ = write_sse_event(stream, "message", &completed_event).await;
            }
        }
        Err(e) => {
            fail_task(store, &task_id, &e);
            let failed_event = serde_json::json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "result": {
                    "type": "status",
                    "taskId": task_id,
                    "status": {"state": "failed"},
                    "error": e
                }
            });
            let _ = write_sse_event(stream, "message", &failed_event).await;
        }
    }
}
