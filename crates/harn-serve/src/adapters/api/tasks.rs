//! Task routes project the canonical task admission and lifecycle owner.

use super::*;
use harn_session_store::task::{
    AdmitTask, NewTaskControlEvent, StoredTaskRetry, TaskFailure, TaskRecord, TaskStatus,
};
use harn_session_store::StoreError;
use harn_vm::task_retry::{
    TaskAdmission, TaskRequestFingerprint, TaskRetryRequest, TaskRetryScope,
};

pub(super) async fn submit_task(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuthenticatedPrincipal>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    submit_task_inner(state, None, principal, headers, body).await
}

pub(super) async fn submit_task_inner(
    state: ApiState,
    path_session_id: Option<String>,
    principal: AuthenticatedPrincipal,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let key = match task_retry_key(&headers) {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    let Ok(mut input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    if !input.is_object() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "task request must be a JSON object",
        );
    }
    let body_session = input.get("session_id").and_then(Value::as_str);
    if path_session_id
        .as_deref()
        .zip(body_session)
        .is_some_and(|(path, body)| path != body)
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_session",
            "task session identities disagree",
        );
    }
    let Some(session_id) = path_session_id.or_else(|| body_session.map(str::to_owned)) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing_session",
            "session_id is required",
        );
    };
    // Both public submission routes have one accepted request shape.
    input["session_id"] = json!(session_id);
    if state.recover_interrupted_tasks().is_err() {
        return task_store::unavailable_response();
    }
    let admission_control = state.session_control.lock().await;
    let descriptor = match state.session_descriptor(&session_id) {
        Ok(descriptor) => descriptor,
        Err(StoreError::NotFound(_)) => {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found")
        }
        Err(_) => return task_store::unavailable_response(),
    };
    let retry = match key {
        Some(key) => {
            let actor = match principal.task_retry_actor() {
                Ok(actor) => actor,
                Err(_) => return task_store::unavailable_response(),
            };
            let scope = match TaskRetryScope::for_task_create(
                actor,
                Some(&descriptor.workspace_id),
                &session_id,
                key,
            ) {
                Ok(scope) => scope,
                Err(_) => return task_store::unavailable_response(),
            };
            Some(TaskRetryRequest::new(
                scope,
                TaskRequestFingerprint::from_value(&input),
            ))
        }
        None => None,
    };
    for field in ["persona_id", "branch_id", "parent_task_id"] {
        if input
            .get(field)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "task identity fields must be strings",
            );
        }
    }
    let task_id = format!("task_{}", Uuid::now_v7());
    let now = now_rfc3339();
    let message_input = input.get("input").cloned().unwrap_or_else(|| json!({}));
    let task = TaskRecord {
        id: task_id.clone(),
        session_id: session_id.clone(),
        workspace_id: descriptor.workspace_id.clone(),
        created_at: now.clone(),
        updated_at: now,
        status: TaskStatus::Submitted,
        input: message_input.clone(),
        metadata: input.get("metadata").cloned().unwrap_or_else(|| json!({})),
        created_by: "api".into(),
        persona_id: input["persona_id"].as_str().map(str::to_owned),
        branch_id: input["branch_id"].as_str().map(str::to_owned),
        parent_task_id: input["parent_task_id"].as_str().map(str::to_owned),
        assigned_agent_id: None,
        receipt_id: None,
        outcome_id: None,
        quota_id: None,
        started_at: None,
        completed_at: None,
        canceled_at: None,
        failure: None,
    };
    let store = match state.task_store() {
        Ok(store) => store,
        Err(_) => return task_store::unavailable_response(),
    };
    // Claim before persistence so recovery cannot interrupt the gap before dispatch.
    let claim = match store.claim_task_execution(&task_id) {
        Ok(claim) => claim,
        Err(_) => return task_store::unavailable_response(),
    };
    let message = message_resource(&session_id, Some(&task_id), message_input.clone());
    let admission = store.admit_task(AdmitTask {
        task,
        session: descriptor,
        retry: retry.map(|retry| StoredTaskRetry {
            binding_digest: retry.scope().digest().into(),
            request_fingerprint: retry.fingerprint().as_hex().into(),
        }),
        initial_events: vec![NewTaskControlEvent {
            event: "message.created".into(),
            payload: message,
        }],
        now_ms: (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64,
    });
    let task = match admission {
        Ok(TaskAdmission::Reused(task)) => {
            return (StatusCode::ACCEPTED, Json(task.resource())).into_response()
        }
        Ok(TaskAdmission::Conflict) => {
            return api_error(
                StatusCode::CONFLICT,
                "idempotency_key_reused",
                "idempotency key was already used for a different task request",
            )
        }
        Ok(TaskAdmission::Created(task)) => task,
        Err(StoreError::Conflict(_)) => {
            return api_error(StatusCode::CONFLICT, "session_closed", "session is closed")
        }
        Err(_) => return task_store::unavailable_response(),
    };
    let events = match store.task_events(Some(&task_id), None) {
        Ok(events) => events,
        Err(_) => return task_store::unavailable_response(),
    };
    for event in events {
        state.publish_task_event(event);
    }
    drop(admission_control);
    let prompt = prompt_text(&message_input);
    let task_state = state.clone();
    tokio::spawn(async move {
        let _claim = claim;
        let result = async {
            let control = task_state
                .restore_session(&session_id)
                .await
                .map_err(|_| "task session could not be restored".to_owned())?;
            task_state
                .require_open_session(&session_id)
                .map_err(|_| "task session is closed".to_owned())?;
            drop(control);
            let prompt =
                prompt.ok_or_else(|| "task input did not contain prompt text".to_owned())?;
            // Restore can wait on RPC acknowledgements. A cancellation during
            // that work must not enqueue a new prompt; ACP admission checks
            // the same durable task again where execution actually begins.
            let task = task_state
                .task_store()
                .map_err(|_| "durable task storage is unavailable".to_owned())?
                .get_task(&task_id)
                .map_err(|_| "durable task storage is unavailable".to_owned())?;
            if task.status != TaskStatus::Submitted {
                return Err("task is no longer submitted".to_owned());
            }
            task_state
                .acp
                .call(
                    "session/prompt",
                    json!({
                        "sessionId": session_id, "prompt": [{"type": "text", "text": prompt}],
                        "_meta": {"harn": {"task": TaskPromptTarget {task_id: task_id.clone()}}},
                    }),
                )
                .await
        }
        .await;
        let (status, failure) = match result {
            Ok(response) => prompt_task_outcome(response),
            Err(error) => (
                TaskStatus::Failed,
                Some(TaskFailure {
                    code: "task_failed".into(),
                    message: error,
                }),
            ),
        };
        if task_state.change_task(&task_id, status, failure).is_err() {
            eprintln!("[harn] could not commit task completion; its execution remains interrupted");
        }
    });
    (StatusCode::ACCEPTED, Json(task.resource())).into_response()
}

pub(super) fn prompt_task_outcome(response: Value) -> (TaskStatus, Option<TaskFailure>) {
    use harn_vm::agent_events::AgentLifecycleState;

    let response =
        match serde_json::from_value::<crate::adapters::acp::AcpSessionPromptResult>(response) {
            Ok(response) => response,
            Err(error) => {
                return (
                    TaskStatus::Failed,
                    Some(TaskFailure {
                        code: "invalid_prompt_outcome".into(),
                        message: format!("Invalid runtime prompt outcome: {error}"),
                    }),
                )
            }
        };
    let Some(terminal) = response.meta.and_then(|meta| meta.harn.terminal) else {
        return match response.stop_reason.as_str() {
            "end_turn" => (TaskStatus::Completed, None),
            "cancelled" => (TaskStatus::Canceled, None),
            _ => (
                TaskStatus::Failed,
                Some(TaskFailure {
                    code: "unmeasured_prompt_outcome".into(),
                    message: "Runtime prompt ended without a recognized terminal outcome".into(),
                }),
            ),
        };
    };
    let status = match terminal.kind.lifecycle_state() {
        AgentLifecycleState::Completed => TaskStatus::Completed,
        AgentLifecycleState::Cancelled | AgentLifecycleState::Stopped => TaskStatus::Canceled,
        _ => TaskStatus::Failed,
    };
    let failure = (status == TaskStatus::Failed).then(|| TaskFailure {
        code: terminal.kind.as_str().into(),
        message: terminal.message.unwrap_or(terminal.reason),
    });
    (status, failure)
}

pub(super) async fn list_tasks(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let tasks = state.ready_task_store().and_then(|store| {
        store.list_tasks(query.session_id.as_deref(), query.workspace_id.as_deref())
    });
    match tasks {
        Ok(tasks) => Json(list_response(limit_values(
            tasks.iter().map(TaskRecord::resource).collect(),
            query.limit,
        )))
        .into_response(),
        Err(_) => task_store::unavailable_response(),
    }
}

pub(super) async fn get_task(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
) -> Response {
    task_store::task_response(
        state
            .ready_task_store()
            .and_then(|store| store.get_task(&task_id)),
    )
}

pub(super) async fn append_task_message(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    body: Bytes,
) -> Response {
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    if input.get("kind").and_then(Value::as_str) != Some("input") {
        return api_error(StatusCode::BAD_REQUEST, "unsupported_task_message_kind", "task messages accept kind=input; use the task cancel and permission-response endpoints for controls");
    }
    let Some(message_input) = input.get("message").cloned() else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing_message",
            "message is required",
        );
    };
    let Some(content) = prompt_text(&message_input) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing_message_text",
            "an input task message requires at least one non-empty text part",
        );
    };
    let task = match state
        .ready_task_store()
        .and_then(|store| store.get_task(&task_id))
    {
        Ok(task) => task,
        Err(error) => return task_store::task_response(Err(error)),
    };
    if !task.status.is_active() {
        return api_error(
            StatusCode::CONFLICT,
            "task_terminal",
            "cannot steer a terminal task",
        );
    }
    // The steer's own reply waits until the running turn takes it in; the
    // API answers once it is admitted, which publishes `message.created`.
    let message_id = format!("msg_inj_{}", uuid::Uuid::now_v7().simple());
    let mut admitted = state.events_tx.subscribe();
    let call = state.acp.call_structured("session/prompt", json!({
        "sessionId": task.session_id, "delivery": "steer", "messageId": message_id,
        "prompt": [{"type": "text", "text": content}],
        "_meta": {"harn": {"taskMessage": {"taskId": task_id, "message": message_input}, "actor": {"kind": "agents_api", "taskId": task_id,
            "metadata": message_input.get("metadata").cloned().unwrap_or_else(|| json!({}))}}},
    }));
    tokio::pin!(call);
    let mut watching = true;
    loop {
        tokio::select! {
            result = &mut call => return match result {
                Ok(receipt) => match receipt.pointer("/_meta/harn/taskMessage") {
                    Some(message) => (StatusCode::CREATED, Json(message.clone())).into_response(),
                    None => task_store::unavailable_response(),
                },
                Err(error) if error.code == Some(crate::adapters::acp::TASK_ADMISSION_STORAGE_UNAVAILABLE_RPC_CODE) => task_store::unavailable_response(),
                Err(error) => api_error(StatusCode::CONFLICT, "task_not_steerable", &error.message),
            },
            event = admitted.recv(), if watching => match event {
                Ok(event) if event.event == "message.created" && event.payload["id"] == message_id => {
                    return (StatusCode::CREATED, Json(event.payload)).into_response();
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => watching = false,
            },
        }
    }
}

pub(super) async fn cancel_task(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
) -> Response {
    let task = match state
        .ready_task_store()
        .and_then(|store| store.get_task(&task_id))
    {
        Ok(task) => task,
        Err(error) => return task_store::task_response(Err(error)),
    };
    if task.status == TaskStatus::Canceled {
        return Json(task.resource()).into_response();
    }
    if !task.status.is_active() {
        return api_error(
            StatusCode::CONFLICT,
            "task_not_cancelable",
            &format!(
                "task is already terminal with status {}",
                task.status.as_str()
            ),
        );
    }
    let canceled = match state.change_task(&task_id, TaskStatus::Canceled, None) {
        Ok(task) => task,
        Err(_) => return task_store::unavailable_response(),
    };
    if canceled.status != TaskStatus::Canceled {
        return api_error(
            StatusCode::CONFLICT,
            "task_not_cancelable",
            "task became terminal before cancellation",
        );
    }
    state.acp.send_raw(
        json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {
            "sessionId": task.session_id,
            "_meta":{"harn":{"task":crate::adapters::acp::TaskPromptTarget {task_id}}}
        }}),
    );
    Json(canceled.resource()).into_response()
}
