use super::*;
use crate::task::{
    retry_decision, status_event, AdmitTask, NewTaskControlEvent, RetryDecision, StoredTaskRetry,
    TaskAdmission, TaskBinding, TaskChange, TaskControlEvent, TaskMessageView, TaskRecord,
    TaskSessionDescriptor, TaskStore, TaskUpdate, TASK_RETRY_RETENTION_MS,
};
use serde_json::Value;

impl TaskStore for MemorySessionStore {
    fn next_control_event_id(&self) -> StoreResult<String> {
        next_id(&mut lock(&self.inner))
    }

    fn admit_task(&self, request: AdmitTask) -> StoreResult<TaskAdmission<TaskRecord>> {
        request.validate()?;
        let mut guard = lock(&self.inner);
        if let Some(retry) = &request.retry {
            if let Some(binding) = guard.task_bindings.get(&retry.binding_digest) {
                let task = guard
                    .tasks
                    .get(&binding.task_id)
                    .ok_or_else(|| StoreError::Backend("task retry binding has no task".into()))?;
                match retry_decision(binding, task, &retry.request_fingerprint, request.now_ms) {
                    RetryDecision::Reuse => {
                        return Ok(TaskAdmission::Reused(task.clone().redacted(&self.hooks)?))
                    }
                    RetryDecision::Conflict => return Ok(TaskAdmission::Conflict),
                    RetryDecision::Create => {}
                }
            }
        }
        if guard.tasks.contains_key(&request.task.id) {
            return Err(StoreError::AlreadyExists(request.task.id));
        }
        let task = request.task.redacted(&self.hooks)?;
        let session = request.session.redacted(&self.hooks);
        let mut events = vec![TaskControlEvent::new(
            next_id(&mut guard)?,
            &task,
            status_event(&task, None),
            &self.hooks,
        )];
        for event in request.initial_events {
            events.push(TaskControlEvent::new(
                next_id(&mut guard)?,
                &task,
                event,
                &self.hooks,
            ));
        }
        guard.task_sessions.insert(session.id.clone(), session);
        guard.tasks.insert(task.id.clone(), task.clone());
        guard.task_events.extend(events);
        if let Some(StoredTaskRetry {
            binding_digest,
            request_fingerprint,
        }) = request.retry
        {
            guard.task_bindings.insert(
                binding_digest,
                TaskBinding {
                    task_id: task.id.clone(),
                    request_fingerprint,
                    expires_at_ms: request.now_ms.saturating_add(TASK_RETRY_RETENTION_MS),
                },
            );
        }
        Ok(TaskAdmission::Created(task))
    }

    fn get_task(&self, task_id: &str) -> StoreResult<TaskRecord> {
        lock(&self.inner)
            .tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(task_id.into()))?
            .redacted(&self.hooks)
    }

    fn list_tasks(
        &self,
        session_id: Option<&str>,
        workspace_id: Option<&str>,
    ) -> StoreResult<Vec<TaskRecord>> {
        lock(&self.inner)
            .tasks
            .values()
            .filter(|task| {
                session_id.is_none_or(|id| task.session_id == id)
                    && workspace_id.is_none_or(|id| task.workspace_id == id)
            })
            .cloned()
            .map(|task| task.redacted(&self.hooks))
            .collect()
    }

    fn get_task_session(&self, session_id: &str) -> StoreResult<TaskSessionDescriptor> {
        Ok(lock(&self.inner)
            .task_sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(session_id.into()))?
            .redacted(&self.hooks))
    }

    fn list_task_sessions(&self) -> StoreResult<Vec<TaskSessionDescriptor>> {
        Ok(lock(&self.inner)
            .task_sessions
            .values()
            .cloned()
            .map(|session| session.redacted(&self.hooks))
            .collect())
    }

    fn update_task(&self, task_id: &str, update: TaskUpdate) -> StoreResult<TaskChange> {
        let mut guard = lock(&self.inner);
        let mut task = guard
            .tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(task_id.into()))?;
        let previous_status = task.status;
        if !task.transition(update) {
            return Ok(TaskChange {
                task: task.redacted(&self.hooks)?,
                previous_status,
                event: None,
            });
        }
        let task = task.redacted(&self.hooks)?;
        let event = TaskControlEvent::new(
            next_id(&mut guard)?,
            &task,
            status_event(&task, Some(previous_status)),
            &self.hooks,
        );
        guard.task_events.push(event.clone());
        guard.tasks.insert(task.id.clone(), task.clone());
        Ok(TaskChange {
            task,
            previous_status,
            event: Some(event),
        })
    }

    fn append_task_event(
        &self,
        task_id: &str,
        event: NewTaskControlEvent,
    ) -> StoreResult<TaskControlEvent> {
        let mut guard = lock(&self.inner);
        let task = guard
            .tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(task_id.into()))?;
        let event = TaskControlEvent::new(next_id(&mut guard)?, &task, event, &self.hooks);
        guard.task_events.push(event.clone());
        Ok(event)
    }

    fn task_events(
        &self,
        task_id: Option<&str>,
        session_id: Option<&str>,
    ) -> StoreResult<Vec<TaskControlEvent>> {
        Ok(lock(&self.inner)
            .task_events
            .iter()
            .filter(|event| {
                task_id.is_none_or(|id| event.task_id == id)
                    && session_id.is_none_or(|id| event.session_id == id)
            })
            .cloned()
            .map(|event| event.redacted(&self.hooks))
            .collect())
    }

    fn append_task_input(
        &self,
        task_id: &str,
        session_id: &str,
        message: Value,
    ) -> StoreResult<TaskControlEvent> {
        let mut guard = lock(&self.inner);
        let task = guard
            .tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(task_id.into()))?;
        if task.session_id != session_id || !task.status.is_active() {
            return Err(StoreError::InvalidInput(
                "task cannot accept this session input".into(),
            ));
        }
        let event = TaskControlEvent::new(
            next_id(&mut guard)?,
            &task,
            NewTaskControlEvent {
                event: "message.created".into(),
                payload: message,
            },
            &self.hooks,
        );
        guard.task_events.push(event.clone());
        Ok(event)
    }

    fn task_message_view(&self, session_id: &str) -> StoreResult<TaskMessageView> {
        let guard = lock(&self.inner);
        let events = guard
            .task_events
            .iter()
            .filter(|event| event.session_id == session_id && event.event == "message.created")
            .cloned()
            .map(|event| event.redacted(&self.hooks))
            .collect();
        Ok(guard
            .task_message_projections
            .get(session_id)
            .cloned()
            .unwrap_or_default()
            .view(events))
    }

    fn retain_task_messages(
        &self,
        session_id: &str,
        through_event_id: Option<&str>,
        retained_event_ids: &[String],
    ) -> StoreResult<()> {
        let mut guard = lock(&self.inner);
        let events: Vec<_> = guard
            .task_events
            .iter()
            .filter(|event| event.session_id == session_id && event.event == "message.created")
            .cloned()
            .collect();
        guard
            .task_message_projections
            .entry(session_id.into())
            .or_default()
            .retain(&events, through_event_id, retained_event_ids)
    }

    fn claim_task_execution(&self, task_id: &str) -> StoreResult<crate::TaskExecutionLease> {
        crate::TaskExecutionLease::try_acquire_memory(&self.task_claims, task_id)
    }
}

fn next_id(inner: &mut Inner) -> StoreResult<String> {
    let next = inner
        .control_event_seq
        .checked_add(1)
        .filter(|next| *next < i64::MAX as u64)
        .ok_or_else(|| StoreError::Backend("control event sequence exhausted".into()))?;
    inner.control_event_seq = next;
    Ok(format!("event_{next:016x}"))
}
