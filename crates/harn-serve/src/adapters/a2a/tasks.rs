//! A2A task lifecycle, cancellation, subscription, and push config state.
use super::auth::deliver_push_configs;
use super::events::*;
use super::schema::*;
use super::*;

impl A2aServer {
    pub(super) async fn prepare_task(
        &self,
        params: &JsonValue,
        auth: AuthRequest,
    ) -> Result<PreparedTask, A2aPrepareError> {
        let parts = message_parts(params)?;
        let text = message_text(params, &parts);
        let function = select_function(&self.catalog, params)?;
        let function_decl = self
            .catalog
            .function(&function)
            .expect("selected function exists");
        // Scope preflight: refuse the task before any state is allocated
        // when the caller's credential cannot satisfy the function's
        // `@scopes(...)` attribute. The REST adapter maps the carried
        // `FORBIDDEN` status to HTTP 403; JSON-RPC peers see `-32003`
        // with a structured `data` body.
        if !function_decl.required_scopes.is_empty() {
            let decision = self
                .core
                .auth_policy()
                .authorize_with_scopes(&auth, &function_decl.required_scopes)
                .await;
            if let AuthorizationDecision::MissingScope { required, granted } = decision {
                return Err(scope_mismatch_prepare_error(&required, &granted));
            }
        }
        let arguments = message_arguments(function_decl, params, &parts, &text)?;
        let task_id = Uuid::now_v7().to_string();
        let cancel_token = Arc::new(AtomicBool::new(false));
        let context_id = params
            .get("contextId")
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        let trace_id = auth
            .headers
            .get(A2A_TRACE_HEADER)
            .cloned()
            .or_else(|| context_id.clone())
            .map(harn_vm::TraceId);
        let actor_chain = actor_chain_param(params)?;
        let metadata = actor_chain
            .as_ref()
            .map(actor_chain_task_metadata)
            .unwrap_or_default();
        let push_config = params
            .pointer("/configuration/pushNotificationConfig")
            .cloned();

        let mut task = TaskState {
            id: task_id.clone(),
            context_id,
            status: TaskStatus::Submitted,
            history: vec![TaskMessage {
                id: Uuid::now_v7().to_string(),
                role: "user".to_string(),
                parts: parts.clone(),
            }],
            artifacts: a2a_artifacts_from_parts(&parts),
            metadata,
            events: Vec::new(),
            subscribers: Vec::new(),
            cancel_token: Some(cancel_token.clone()),
        };
        task.events
            .push(status_event(&task_id, TaskStatus::Submitted));
        self.tasks
            .lock()
            .expect("tasks poisoned")
            .insert(task_id.clone(), task);

        if let Some(push_config) = push_config {
            if let Err(error) = self.add_push_config(&task_id, push_config).await {
                self.tasks.lock().expect("tasks poisoned").remove(&task_id);
                return Err(A2aPrepareError::new(-32603, error));
            }
        }

        Ok(PreparedTask {
            id: task_id,
            function,
            arguments,
            auth,
            caller: caller_label(params),
            trace_id,
            actor_chain,
            cancel_token,
        })
    }

    pub(super) async fn run_task_to_completion(self: &Arc<Self>, task: &PreparedTask) {
        self.transition(&task.id, TaskStatus::Working);
        // Subscribe a per-task `AgentEventSink` that translates worker
        // lifecycle events into A2A task events. The session id used by
        // the inner dispatch (set via `agent_session_id` on the
        // CallRequest) must match — both sides are derived from the
        // task id so a single key wires emit -> sink -> task stream.
        let session_id = a2a_worker_session_id(&task.id);
        let sink: Arc<dyn harn_vm::agent_events::AgentEventSink> = Arc::new(A2aWorkerSink {
            task_id: task.id.clone(),
            tasks: self.tasks.clone(),
        });
        harn_vm::agent_events::register_sink(session_id.clone(), sink.clone());

        let result = self
            .executor
            .call(CallRequest {
                adapter: self.descriptor.id.clone(),
                function: task.function.clone(),
                arguments: task.arguments.clone(),
                auth: task.auth.clone(),
                caller: task.caller.clone(),
                replay_key: Some(task.id.clone()),
                trace_id: task.trace_id.clone(),
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: Some(task.cancel_token.clone()),
                agent_session_id: Some(session_id.clone()),
                agent_event_sink: Some(DispatchAgentEventSink::new(sink)),
                actor_chain: task.actor_chain.clone(),
                actor_chain_hop: Some(self.agent_name.clone()),
                progress: None,
                tenant_id: None,
                request_id: Some(task.id.clone()),
                auth_context: None,
                auth_principal: None,
            })
            .await;

        let sink_result = harn_vm::agent_events::flush_and_clear_session_sinks(&session_id).await;
        if self.is_cancelled(&task.id) {
            if let Err(error) = sink_result {
                self.record_task_persistence_error(&task.id, &error);
            }
            return;
        }

        if let Err(error) = sink_result {
            let message = match result {
                Ok(_) => format!("Failed to persist agent events: {error}"),
                Err(dispatch_error) => {
                    format!("{dispatch_error}; failed to persist agent events: {error}")
                }
            };
            self.fail_task(&task.id, &message);
            return;
        }

        match result {
            Ok(response) => self.complete_task(&task.id, response),
            // The dispatch core's `AuthPolicy.authorize` is what produces
            // `DispatchError::Unauthorized` and `Forbidden`. Both run
            // synchronously at the start of `core.dispatch` before any
            // script work, so the policy denial is "the server declined
            // this task" — A2A 0.3.0's `rejected` terminal state. Any
            // post-policy auth failure (e.g. an LLM/HTTP 401 raised by
            // the script itself) surfaces through `Execution(...)` with
            // an `auth`-classified message and maps to non-terminal
            // `auth-required` so the client can resupply credentials and
            // resubscribe.
            Err(error @ DispatchError::Unauthorized(_))
            | Err(error @ DispatchError::Forbidden { .. }) => {
                self.reject_task(&task.id, &error.to_string());
            }
            Err(DispatchError::Execution(message))
                if matches!(
                    harn_vm::value::classify_error_message(&message),
                    harn_vm::value::ErrorCategory::Auth
                ) =>
            {
                self.auth_required_task(&task.id, &message);
            }
            Err(error) => self.fail_task(&task.id, &error.to_string()),
        }
    }

    fn record_task_persistence_error(
        &self,
        task_id: &str,
        error: &harn_vm::agent_events::AgentEventSinkError,
    ) {
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let Some(task) = tasks.get_mut(task_id) else {
            return;
        };
        let harn = task
            .metadata
            .entry("harn".to_string())
            .or_insert_with(|| json!({}));
        if !harn.is_object() {
            *harn = json!({});
        }
        harn["persistenceError"] = JsonValue::String(error.to_string());
    }

    pub(super) fn transition(&self, task_id: &str, status: TaskStatus) {
        let event = status_event(task_id, status.clone());
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(task_id) else {
                return;
            };
            task.status = status;
            publish_locked(task, event);
            task_to_json(task)
        };
        self.deliver_push(task_for_push);
    }

    pub(super) fn complete_task(&self, task_id: &str, response: CallResponse) {
        let parts = response_parts(&response.value);
        let artifacts = response_artifacts(&response.value, &parts);
        let handoff_metadata = handoff_task_metadata(&response);
        let message = json!({
            "type": "message",
            "taskId": task_id,
            "message": {
                "id": Uuid::now_v7().to_string(),
                "role": "agent",
                "parts": parts
            }
        });
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(task_id) else {
                return;
            };
            task.history.push(TaskMessage {
                id: Uuid::now_v7().to_string(),
                role: "agent".to_string(),
                parts,
            });
            task.artifacts.extend(artifacts);
            if let Some(metadata) = handoff_metadata {
                task.metadata.extend(metadata);
            }
            publish_locked(task, message);
            task.status = TaskStatus::Completed;
            publish_locked(task, status_event(task_id, TaskStatus::Completed));
            task.cancel_token = None;
            task_to_json(task)
        };
        self.deliver_push(task_for_push);
    }

    pub(super) fn fail_task(&self, task_id: &str, message: &str) {
        self.terminate_task(task_id, TaskStatus::Failed, message);
    }

    /// Terminal — the dispatch core's `AuthPolicy` synchronously denied
    /// the caller before any script work could run. The A2A spec calls
    /// this `rejected`: the client cannot resume by re-authing or
    /// retrying, it has to adjust its request (or the server-side
    /// policy) and send a new task.
    pub(super) fn reject_task(&self, task_id: &str, message: &str) {
        self.terminate_task(task_id, TaskStatus::Rejected, message);
    }

    pub(super) fn terminate_task(&self, task_id: &str, status: TaskStatus, message: &str) {
        debug_assert!(
            status.is_terminal(),
            "terminate_task expects a terminal status"
        );
        let event = json!({
            "type": "status",
            "taskId": task_id,
            "status": {"state": status.as_str()},
            "error": message,
        });
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(task_id) else {
                return;
            };
            task.status = status;
            task.history.push(TaskMessage {
                id: Uuid::now_v7().to_string(),
                role: "agent".to_string(),
                parts: vec![json!({"type": "text", "text": message})],
            });
            publish_locked(task, event);
            task.cancel_token = None;
            task_to_json(task)
        };
        self.deliver_push(task_for_push);
    }

    /// Non-terminal — a downstream auth check failed mid-task (the
    /// script itself raised an `auth`-classified error). The client is
    /// expected to refresh its credentials and resubscribe; the task
    /// remains in the store so a follow-up `tasks/resubscribe` finds it.
    /// Subscribers are kept attached because the state is non-terminal.
    pub(super) fn auth_required_task(&self, task_id: &str, message: &str) {
        let event = json!({
            "type": "status",
            "taskId": task_id,
            "status": {"state": TaskStatus::AuthRequired.as_str()},
            "error": message,
        });
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(task_id) else {
                return;
            };
            task.status = TaskStatus::AuthRequired;
            task.history.push(TaskMessage {
                id: Uuid::now_v7().to_string(),
                role: "agent".to_string(),
                parts: vec![json!({"type": "text", "text": message})],
            });
            publish_locked(task, event);
            task.cancel_token = None;
            task_to_json(task)
        };
        self.deliver_push(task_for_push);
    }

    pub(super) fn cancel_task(&self, task_id: &str) -> Result<JsonValue, String> {
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let task = tasks
                .get_mut(task_id)
                .ok_or_else(|| format!("TaskNotFoundError: {task_id}"))?;
            if task.status.is_terminal() {
                return Err(format!(
                    "TaskNotCancelableError: task {} is in terminal state '{}'",
                    task_id,
                    task.status.as_str()
                ));
            }
            if let Some(cancel_token) = task.cancel_token.as_ref() {
                cancel_token.store(true, Ordering::SeqCst);
            }
            task.status = TaskStatus::Cancelled;
            publish_locked(task, status_event(task_id, TaskStatus::Cancelled));
            task.cancel_token = None;
            task_to_json(task)
        };
        self.deliver_push(task_for_push.clone());
        Ok(task_for_push)
    }

    pub(super) fn is_cancelled(&self, task_id: &str) -> bool {
        self.tasks
            .lock()
            .expect("tasks poisoned")
            .get(task_id)
            .is_some_and(|task| task.status == TaskStatus::Cancelled)
    }

    pub(super) fn subscribe(&self, task_id: &str) -> Option<UnboundedReceiver<JsonValue>> {
        let (tx, rx) = unbounded();
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let task = tasks.get_mut(task_id)?;
        for event in &task.events {
            let _ = tx.unbounded_send(wrap_event(JsonValue::Null, event.clone()));
        }
        if !task.status.is_terminal() {
            task.subscribers.push(tx);
        }
        Some(rx)
    }

    pub(super) fn task_json(&self, task_id: &str) -> JsonValue {
        self.tasks
            .lock()
            .expect("tasks poisoned")
            .get(task_id)
            .map(task_to_json)
            .unwrap_or(JsonValue::Null)
    }

    pub(super) fn list_tasks(&self) -> JsonValue {
        let tasks = self
            .tasks
            .lock()
            .expect("tasks poisoned")
            .values()
            .map(|task| {
                json!({
                    "id": task.id,
                    "status": {"state": task.status.as_str()},
                    "contextId": task.context_id,
                })
            })
            .collect::<Vec<_>>();
        json!({ "tasks": tasks })
    }

    pub(super) async fn add_push_config(
        &self,
        task_id: &str,
        mut config: JsonValue,
    ) -> Result<JsonValue, String> {
        if !self.push_config_task_known(task_id) {
            return Err(format!("TaskNotFoundError: {task_id}"));
        }
        if config.get("id").and_then(JsonValue::as_str).is_none() {
            config["id"] = JsonValue::String(Uuid::now_v7().to_string());
        }
        config["taskId"] = JsonValue::String(task_id.to_string());
        let config_id = config["id"].as_str().unwrap_or_default().to_string();

        self.append_push_config_event(
            A2A_PUSH_CONFIG_SET_KIND,
            json!({
                "taskId": task_id,
                "configId": config_id,
                "config": config,
            }),
        )
        .await?;
        self.apply_push_config_set(task_id, &config_id, config.clone());
        Ok(config)
    }

    pub(super) fn push_config(
        &self,
        task_id: &str,
        config_id: Option<&str>,
    ) -> Result<JsonValue, String> {
        let configs = self.push_configs_for_task(task_id)?;
        let config = if let Some(config_id) = config_id {
            configs.into_iter().find(|config| {
                config
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|id| id == config_id)
            })
        } else {
            configs.into_iter().next()
        };
        config.ok_or_else(|| format!("TaskPushNotificationConfigNotFoundError: {task_id}"))
    }

    pub(super) fn push_configs(&self, task_id: Option<&str>) -> Result<JsonValue, String> {
        let configs = if let Some(task_id) = task_id {
            self.push_configs_for_task(task_id)?
        } else {
            self.push_configs
                .lock()
                .expect("push configs poisoned")
                .values()
                .flat_map(|configs| configs.values().cloned())
                .collect::<Vec<_>>()
        };
        Ok(JsonValue::Array(configs))
    }

    pub(super) async fn delete_push_config(
        &self,
        task_id: &str,
        config_id: &str,
    ) -> Result<(), String> {
        if !self
            .push_configs
            .lock()
            .expect("push configs poisoned")
            .get(task_id)
            .is_some_and(|configs| configs.contains_key(config_id))
        {
            return Err(format!(
                "TaskPushNotificationConfigNotFoundError: {task_id}/{config_id}"
            ));
        }
        self.append_push_config_event(
            A2A_PUSH_CONFIG_DELETE_KIND,
            json!({
                "taskId": task_id,
                "configId": config_id,
            }),
        )
        .await?;
        self.apply_push_config_delete(task_id, config_id);
        Ok(())
    }

    pub(super) fn push_config_task_known(&self, task_id: &str) -> bool {
        self.tasks
            .lock()
            .expect("tasks poisoned")
            .contains_key(task_id)
            || self
                .push_configs
                .lock()
                .expect("push configs poisoned")
                .contains_key(task_id)
    }

    pub(super) fn push_configs_for_task(&self, task_id: &str) -> Result<Vec<JsonValue>, String> {
        if let Some(configs) = self
            .push_configs
            .lock()
            .expect("push configs poisoned")
            .get(task_id)
        {
            return Ok(configs.values().cloned().collect());
        }
        if self
            .tasks
            .lock()
            .expect("tasks poisoned")
            .contains_key(task_id)
        {
            return Ok(Vec::new());
        }
        Err(format!("TaskNotFoundError: {task_id}"))
    }

    pub(super) async fn append_push_config_event(
        &self,
        kind: &'static str,
        payload: JsonValue,
    ) -> Result<(), String> {
        let topic = push_config_topic();
        let log = self.core.event_log();
        log.append(&topic, LogEvent::new(kind, payload))
            .await
            .map_err(|error| format!("EventLogError: {error}"))?;
        log.flush()
            .await
            .map_err(|error| format!("EventLogError: {error}"))
    }

    pub(super) fn apply_push_config_set(&self, task_id: &str, config_id: &str, config: JsonValue) {
        self.push_configs
            .lock()
            .expect("push configs poisoned")
            .entry(task_id.to_string())
            .or_default()
            .insert(config_id.to_string(), config);
    }

    pub(super) fn apply_push_config_delete(&self, task_id: &str, config_id: &str) {
        if let Some(configs) = self
            .push_configs
            .lock()
            .expect("push configs poisoned")
            .get_mut(task_id)
        {
            configs.remove(config_id);
        }
    }

    pub(super) fn deliver_push(&self, task: JsonValue) {
        let task_id = task["id"].as_str().unwrap_or_default();
        let configs = self
            .push_configs
            .lock()
            .expect("push configs poisoned")
            .get(task_id)
            .map(|configs| configs.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if configs.is_empty() {
            return;
        }
        tokio::spawn(async move {
            for result in deliver_push_configs(configs, task).await {
                if let Err(error) = result {
                    tracing::warn!(
                        target: "harn_serve::a2a",
                        %error,
                        "failed to deliver A2A push notification"
                    );
                }
            }
        });
    }
}
