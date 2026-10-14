use super::*;
use harn_session_store::task::{
    TaskChange, TaskControlEvent, TaskFailure, TaskRecord, TaskSessionDescriptor, TaskStatus,
    TaskStore, TaskUpdate,
};
use harn_session_store::{StoreContention, StoreError, StoreResult};

pub(super) fn unavailable_response() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "task_store_unavailable",
        "durable task storage is unavailable",
    )
}

pub(super) fn task_response(result: StoreResult<TaskRecord>) -> Response {
    match result {
        Ok(task) => Json(task.resource()).into_response(),
        Err(StoreError::NotFound(_)) => {
            api_error(StatusCode::NOT_FOUND, "not_found", "task not found")
        }
        Err(_) => unavailable_response(),
    }
}

pub(super) fn session_resource(
    session: TaskSessionDescriptor,
    active: bool,
    last_event: Option<&TaskControlEvent>,
) -> Value {
    let mut resource = json!({
        "id": session.id, "object": "session", "created_at": session.created_at,
        "updated_at": session.updated_at, "metadata": session.metadata,
        "workspace_id": session.workspace_id, "state": "IDLE", "mode_id": session.mode_id,
        "transcript": {"source": "harn.acp", "session_id": session.id},
        "persona_id": session.persona_id, "root_session_id": session.root_session_id,
        "parent_session_id": session.parent_session_id, "branch_id": session.branch_id,
        "last_event_id": null, "summary": session.summary, "expires_at": null,
    });
    if session.status != harn_session_store::SessionStatus::Open {
        resource["state"] = json!("CLOSED");
    }
    if let Some(policy) = session.model_policy {
        resource["model_policy"] = policy;
    }
    if active && session.status == harn_session_store::SessionStatus::Open {
        resource["state"] = json!("WORKING");
    }
    if let Some(event) = last_event {
        resource["last_event_id"] = json!(event.id);
        let descriptor_time = OffsetDateTime::parse(&session.updated_at, &Rfc3339).ok();
        let event_time = OffsetDateTime::parse(&event.created_at, &Rfc3339).ok();
        if descriptor_time.is_none() || event_time > descriptor_time {
            resource["updated_at"] = json!(event.created_at);
        }
    }
    resource
}

impl ApiState {
    pub(super) fn task_store(&self) -> StoreResult<&dyn TaskStore> {
        self.canonical_sessions
            .as_ref()
            .and_then(|store| store.task_store())
            .ok_or_else(|| StoreError::Backend("durable task storage is unavailable".into()))
    }

    /// Recover abandoned executions before interpreting persisted task state.
    /// Recovery itself uses the raw store so this boundary cannot recurse.
    pub(super) fn ready_task_store(&self) -> StoreResult<&dyn TaskStore> {
        self.recover_interrupted_tasks()?;
        self.task_store()
    }

    pub(super) fn session_resource(&self, session_id: &str) -> StoreResult<Value> {
        if let Some(session) = self
            .inner
            .lock()
            .expect("api state poisoned")
            .sessions
            .get(session_id)
            .cloned()
        {
            return Ok(session);
        }
        let store = self.ready_task_store()?;
        let active = store
            .list_tasks(Some(session_id), None)?
            .iter()
            .any(|task| task.status.is_active());
        let events = store.task_events(None, Some(session_id))?;
        Ok(session_resource(
            store.get_task_session(session_id)?,
            active,
            events.last(),
        ))
    }

    pub(super) fn session_resources(&self) -> StoreResult<Vec<Value>> {
        let store = self.ready_task_store()?;
        let descriptors = store.list_task_sessions()?;
        let active: BTreeSet<_> = store
            .list_tasks(None, None)?
            .into_iter()
            .filter(|task| task.status.is_active())
            .map(|task| task.session_id)
            .collect();
        let events = store.task_events(None, None)?;
        let last_events: BTreeMap<_, _> = events
            .iter()
            .map(|event| (event.session_id.as_str(), event))
            .collect();
        let mut resources: BTreeMap<_, _> = descriptors
            .into_iter()
            .map(|descriptor| {
                let id = descriptor.id.clone();
                let resource = session_resource(
                    descriptor,
                    active.contains(&id),
                    last_events.get(id.as_str()).copied(),
                );
                (id, resource)
            })
            .collect();
        let inner = self.inner.lock().expect("api state poisoned");
        resources.extend(
            inner
                .sessions
                .iter()
                .map(|(id, resource)| (id.clone(), resource.clone())),
        );
        Ok(resources.into_values().collect())
    }

    pub(super) fn recover_interrupted_tasks(&self) -> StoreResult<()> {
        let store = self.task_store()?;
        for task in store
            .list_tasks(None, None)?
            .into_iter()
            .filter(|task| task.status.is_active())
        {
            let _claim = match store.claim_task_execution(&task.id) {
                Ok(claim) => claim,
                Err(StoreError::Contention {
                    kind: StoreContention::TaskExecutionHeld,
                    ..
                }) => continue,
                Err(error) => return Err(error),
            };
            let change = store.update_task(
                &task.id,
                TaskUpdate {
                    status: TaskStatus::Failed,
                    at: now_rfc3339(),
                    failure: Some(TaskFailure {
                        code: "task_interrupted".into(),
                        message: "execution interrupted before completion".into(),
                    }),
                },
            )?;
            if let Some(event) = change.event {
                self.publish_task_event(event);
            }
        }
        Ok(())
    }

    pub(super) fn change_task(
        &self,
        task_id: &str,
        status: TaskStatus,
        failure: Option<TaskFailure>,
    ) -> StoreResult<TaskRecord> {
        self.change_task_with_previous(task_id, status, failure)
            .map(|change| change.task)
    }

    pub(super) fn change_task_with_previous(
        &self,
        task_id: &str,
        status: TaskStatus,
        failure: Option<TaskFailure>,
    ) -> StoreResult<TaskChange> {
        let change = self.task_store()?.update_task(
            task_id,
            TaskUpdate {
                status,
                failure,
                at: now_rfc3339(),
            },
        )?;
        if let Some(event) = change.event.clone() {
            self.publish_task_event(event);
        }
        Ok(change)
    }

    pub(super) fn session_descriptor(
        &self,
        session_id: &str,
    ) -> StoreResult<TaskSessionDescriptor> {
        let inner = self.inner.lock().expect("api state poisoned");
        if let Some(session) = inner.sessions.get(session_id) {
            let workspace_id = session["workspace_id"]
                .as_str()
                .unwrap_or(&inner.root_workspace_id);
            let root = workspace_root_locked(&inner, workspace_id)
                .ok_or_else(|| StoreError::NotFound("workspace".into()))?;
            return Ok(TaskSessionDescriptor {
                id: session_id.into(),
                workspace_id: workspace_id.into(),
                workspace_root: root.to_string_lossy().into_owned(),
                mode_id: session["mode_id"].as_str().unwrap_or("ask").into(),
                model_policy: session
                    .get("model_policy")
                    .filter(|value| !value.is_null())
                    .cloned(),
                metadata: session["metadata"].clone(),
                persona_id: session["persona_id"].as_str().map(str::to_owned),
                root_session_id: session["root_session_id"].as_str().map(str::to_owned),
                parent_session_id: session["parent_session_id"].as_str().map(str::to_owned),
                branch_id: session["branch_id"].as_str().map(str::to_owned),
                created_at: session["created_at"].as_str().unwrap_or_default().into(),
                updated_at: session["updated_at"].as_str().unwrap_or_default().into(),
                summary: session["summary"].as_str().map(str::to_owned),
                status: if session["state"] == "CLOSED" {
                    harn_session_store::SessionStatus::Closed
                } else {
                    harn_session_store::SessionStatus::Open
                },
            });
        }
        drop(inner);
        self.task_store()?.get_task_session(session_id)
    }

    pub(super) fn publish_task_event(&self, event: TaskControlEvent) -> ApiEvent {
        publish_task_event(&self.inner, &self.events_tx, event)
    }
}

pub(super) fn publish_task_event(
    inner: &Mutex<ApiStateInner>,
    events_tx: &broadcast::Sender<ApiEvent>,
    event: TaskControlEvent,
) -> ApiEvent {
    let event = ApiEvent::from(event);
    let mut inner = inner.lock().expect("api state poisoned");
    if let Some(session) = event
        .session_id
        .as_ref()
        .and_then(|id| inner.sessions.get_mut(id))
    {
        session["last_event_id"] = json!(event.id);
        session["updated_at"] = json!(event.created_at);
    }
    drop(inner);
    let _ = events_tx.send(event.clone());
    event
}

impl From<TaskControlEvent> for ApiEvent {
    fn from(event: TaskControlEvent) -> Self {
        let fallback = ResourcePointer {
            object: "task".into(),
            id: event.task_id.clone(),
        };
        Self::new(
            event.id,
            event.created_at,
            event.event,
            Some(event.session_id),
            Some(event.task_id),
            event.payload,
            fallback,
        )
    }
}
