use super::*;
use crate::task::{
    retry_decision, status_event, AdmitTask, NewTaskControlEvent, RetryDecision, TaskAdmission,
    TaskBinding, TaskChange, TaskControlEvent, TaskMessageProjection, TaskMessageView, TaskRecord,
    TaskSessionChange, TaskSessionDescriptor, TaskSessionUpdate, TaskStatus, TaskStore, TaskUpdate,
    TASK_RETRY_RETENTION_MS,
};
use serde_json::Value;

fn decode<T: serde::de::DeserializeOwned>(encoded: &str) -> StoreResult<T> {
    serde_json::from_str(encoded)
        .map_err(|_| StoreError::Backend("stored task data is invalid".into()))
}

fn encode<T: serde::Serialize>(value: &T) -> StoreResult<String> {
    serde_json::to_string(value)
        .map_err(|_| StoreError::Backend("task data could not be encoded".into()))
}

fn read_task(conn: &Connection, task_id: &str) -> StoreResult<TaskRecord> {
    let encoded: String = conn
        .query_row(
            "SELECT record_json FROM agent_tasks WHERE id = ?1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sql)?
        .ok_or_else(|| StoreError::NotFound(task_id.into()))?;
    decode(&encoded)
}

fn read_binding(conn: &Connection, digest: &str) -> StoreResult<Option<TaskBinding>> {
    conn.query_row(
        "SELECT task_id, request_fingerprint, expires_at_ms FROM task_retry_bindings WHERE binding_digest = ?1",
        params![digest], |row| Ok(TaskBinding {
            task_id: row.get(0)?, request_fingerprint: row.get(1)?, expires_at_ms: row.get(2)?,
        }),
    ).optional().map_err(map_sql)
}

fn write_task(conn: &Connection, task: &TaskRecord) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO agent_tasks (id, session_id, workspace_id, status, record_json)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET status = excluded.status, record_json = excluded.record_json",
        params![task.id, task.session_id, task.workspace_id, task.status.as_str(), encode(task)?],
    ).map_err(map_sql)?;
    Ok(())
}

fn write_event(conn: &Connection, event: &TaskControlEvent) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO agent_task_events (id, task_id, session_id, record_json) VALUES (?1, ?2, ?3, ?4)",
        params![event.id, event.task_id, event.session_id, encode(event)?],
    ).map_err(map_sql)?;
    Ok(())
}

fn read_message_events(conn: &Connection, session_id: &str) -> StoreResult<Vec<TaskControlEvent>> {
    let mut statement = conn
        .prepare("SELECT record_json FROM agent_task_events WHERE session_id = ?1 ORDER BY id")
        .map_err(map_sql)?;
    let rows = statement
        .query_map(params![session_id], |row| row.get::<_, String>(0))
        .map_err(map_sql)?;
    let events: Vec<TaskControlEvent> = rows
        .map(|row| decode(&row.map_err(map_sql)?))
        .collect::<StoreResult<_>>()?;
    Ok(events
        .into_iter()
        .filter(|event| event.event == "message.created")
        .collect())
}

fn read_message_projection(
    conn: &Connection,
    session_id: &str,
) -> StoreResult<TaskMessageProjection> {
    let encoded: Option<String> = conn
        .query_row(
            "SELECT record_json FROM agent_task_message_projections WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sql)?;
    encoded
        .map(|encoded| decode(&encoded))
        .transpose()
        .map(Option::unwrap_or_default)
}

impl TaskStore for SqliteSessionStore {
    fn next_control_event_id(&self) -> StoreResult<String> {
        let mut conn = self.lock_for_mutation()?;
        let tx = write_transaction(&mut conn)?;
        let id = next_id(&tx)?;
        tx.commit().map_err(map_sql)?;
        Ok(id)
    }

    fn admit_task(&self, request: AdmitTask) -> StoreResult<TaskAdmission<TaskRecord>> {
        request.validate()?;
        let mut conn = self.lock_for_mutation()?;
        // Own the writer before lookup, so a waiting submitter sees the first
        // committed binding rather than a snapshot predating that admission.
        let tx = write_transaction(&mut conn)?;
        if let Some(retry) = &request.retry {
            if let Some(binding) = read_binding(&tx, &retry.binding_digest)? {
                let task = read_task(&tx, &binding.task_id)?;
                match retry_decision(&binding, &task, &retry.request_fingerprint, request.now_ms) {
                    RetryDecision::Reuse => {
                        return Ok(TaskAdmission::Reused(task.redacted(self.hooks())?))
                    }
                    RetryDecision::Conflict => return Ok(TaskAdmission::Conflict),
                    RetryDecision::Create => {}
                }
            }
        }
        if tx
            .query_row(
                "SELECT 1 FROM agent_tasks WHERE id = ?1",
                params![request.task.id],
                |_| Ok(()),
            )
            .optional()
            .map_err(map_sql)?
            .is_some()
        {
            return Err(StoreError::AlreadyExists(request.task.id));
        }
        let stored_session: Option<String> = tx
            .query_row(
                "SELECT record_json FROM agent_task_sessions WHERE id = ?1",
                params![request.session.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sql)?;
        let session = stored_session
            .map(|encoded| decode::<TaskSessionDescriptor>(&encoded))
            .transpose()?
            .unwrap_or(request.session)
            .redacted(self.hooks());
        if session.status != SessionStatus::Open {
            return Err(StoreError::Conflict("session is closed".into()));
        }
        let task = request.task.redacted(self.hooks())?;
        tx.execute(
            "INSERT INTO agent_task_sessions (id, record_json) VALUES (?1, ?2)
             ON CONFLICT(id) DO NOTHING",
            params![session.id, encode(&session)?],
        )
        .map_err(map_sql)?;
        write_task(&tx, &task)?;
        write_event(
            &tx,
            &TaskControlEvent::new(
                next_id(&tx)?,
                &task,
                status_event(&task, None),
                self.hooks(),
            ),
        )?;
        for event in request.initial_events {
            write_event(
                &tx,
                &TaskControlEvent::new(next_id(&tx)?, &task, event, self.hooks()),
            )?;
        }
        if let Some(retry) = request.retry {
            tx.execute(
                "INSERT INTO task_retry_bindings (binding_digest, request_fingerprint, task_id, expires_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(binding_digest) DO UPDATE SET
                    request_fingerprint = excluded.request_fingerprint,
                    task_id = excluded.task_id, expires_at_ms = excluded.expires_at_ms",
                params![retry.binding_digest, retry.request_fingerprint, task.id,
                    request.now_ms.saturating_add(TASK_RETRY_RETENTION_MS)],
            ).map_err(map_sql)?;
        }
        tx.commit().map_err(map_sql)?;
        Ok(TaskAdmission::Created(task))
    }

    fn get_task(&self, task_id: &str) -> StoreResult<TaskRecord> {
        read_task(&self.lock(), task_id)?.redacted(self.hooks())
    }

    fn list_tasks(
        &self,
        session_id: Option<&str>,
        workspace_id: Option<&str>,
    ) -> StoreResult<Vec<TaskRecord>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT record_json FROM agent_tasks
             WHERE (?1 IS NULL OR session_id = ?1) AND (?2 IS NULL OR workspace_id = ?2) ORDER BY id",
        ).map_err(map_sql)?;
        let encoded = statement
            .query_map(params![session_id, workspace_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(map_sql)?;
        encoded
            .map(|row| decode::<TaskRecord>(&row.map_err(map_sql)?)?.redacted(self.hooks()))
            .collect()
    }

    fn get_task_session(&self, session_id: &str) -> StoreResult<TaskSessionDescriptor> {
        let conn = self.lock();
        let encoded: String = conn
            .query_row(
                "SELECT record_json FROM agent_task_sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sql)?
            .ok_or_else(|| StoreError::NotFound(session_id.into()))?;
        Ok(decode::<TaskSessionDescriptor>(&encoded)?.redacted(self.hooks()))
    }

    fn list_task_sessions(&self) -> StoreResult<Vec<TaskSessionDescriptor>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare("SELECT record_json FROM agent_task_sessions ORDER BY id")
            .map_err(map_sql)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_sql)?;
        rows.map(|row| {
            Ok(decode::<TaskSessionDescriptor>(&row.map_err(map_sql)?)?.redacted(self.hooks()))
        })
        .collect()
    }

    fn update_task_session(
        &self,
        session_id: &str,
        update: TaskSessionUpdate,
    ) -> StoreResult<TaskSessionChange> {
        let mut conn = self.lock_for_mutation()?;
        let tx = write_transaction(&mut conn)?;
        let encoded: String = tx
            .query_row(
                "SELECT record_json FROM agent_task_sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sql)?
            .ok_or_else(|| StoreError::NotFound(session_id.into()))?;
        let mut session: TaskSessionDescriptor = decode(&encoded)?;
        update.apply(&mut session)?;
        let session = session.redacted(self.hooks());
        tx.execute(
            "UPDATE agent_task_sessions SET record_json = ?2 WHERE id = ?1",
            params![session_id, encode(&session)?],
        )
        .map_err(map_sql)?;
        let mut events = Vec::new();
        if session.status == SessionStatus::Closed {
            let active = {
                let mut statement = tx
                    .prepare(
                        "SELECT record_json FROM agent_tasks WHERE session_id = ?1 ORDER BY id",
                    )
                    .map_err(map_sql)?;
                let rows = statement
                    .query_map(params![session_id], |row| row.get::<_, String>(0))
                    .map_err(map_sql)?;
                rows.map(|row| decode::<TaskRecord>(&row.map_err(map_sql)?))
                    .collect::<StoreResult<Vec<_>>>()?
            };
            for mut task in active.into_iter().filter(|task| task.status.is_active()) {
                let previous = task.status;
                task.transition(TaskUpdate {
                    status: TaskStatus::Canceled,
                    at: session.updated_at.clone(),
                    failure: None,
                });
                let task = task.redacted(self.hooks())?;
                write_task(&tx, &task)?;
                let event = TaskControlEvent::new(
                    next_id(&tx)?,
                    &task,
                    status_event(&task, Some(previous)),
                    self.hooks(),
                );
                write_event(&tx, &event)?;
                events.push(event);
            }
        }
        tx.commit().map_err(map_sql)?;
        Ok(TaskSessionChange { session, events })
    }

    fn update_task(&self, task_id: &str, update: TaskUpdate) -> StoreResult<TaskChange> {
        let mut conn = self.lock_for_mutation()?;
        let tx = write_transaction(&mut conn)?;
        let mut task = read_task(&tx, task_id)?;
        let previous_status = task.status;
        if !task.transition(update) {
            return Ok(TaskChange {
                task: task.redacted(self.hooks())?,
                previous_status,
                event: None,
            });
        }
        let task = task.redacted(self.hooks())?;
        write_task(&tx, &task)?;
        let event = TaskControlEvent::new(
            next_id(&tx)?,
            &task,
            status_event(&task, Some(previous_status)),
            self.hooks(),
        );
        write_event(&tx, &event)?;
        tx.commit().map_err(map_sql)?;
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
        let mut conn = self.lock_for_mutation()?;
        let tx = write_transaction(&mut conn)?;
        let task = read_task(&tx, task_id)?;
        let event = TaskControlEvent::new(next_id(&tx)?, &task, event, self.hooks());
        write_event(&tx, &event)?;
        tx.commit().map_err(map_sql)?;
        Ok(event)
    }

    fn task_events(
        &self,
        task_id: Option<&str>,
        session_id: Option<&str>,
    ) -> StoreResult<Vec<TaskControlEvent>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT record_json FROM agent_task_events
             WHERE (?1 IS NULL OR task_id = ?1) AND (?2 IS NULL OR session_id = ?2) ORDER BY id",
            )
            .map_err(map_sql)?;
        let encoded = statement
            .query_map(params![task_id, session_id], |row| row.get::<_, String>(0))
            .map_err(map_sql)?;
        encoded
            .map(|row| {
                Ok(decode::<TaskControlEvent>(&row.map_err(map_sql)?)?.redacted(self.hooks()))
            })
            .collect()
    }

    fn append_task_input(
        &self,
        task_id: &str,
        session_id: &str,
        message: Value,
    ) -> StoreResult<TaskControlEvent> {
        let mut conn = self.lock_for_mutation()?;
        let tx = write_transaction(&mut conn)?;
        let task = read_task(&tx, task_id)?;
        if task.session_id != session_id || !task.status.is_active() {
            return Err(StoreError::InvalidInput(
                "task cannot accept this session input".into(),
            ));
        }
        let event = TaskControlEvent::new(
            next_id(&tx)?,
            &task,
            NewTaskControlEvent {
                event: "message.created".into(),
                payload: message,
            },
            self.hooks(),
        );
        write_event(&tx, &event)?;
        tx.commit().map_err(map_sql)?;
        Ok(event)
    }

    fn task_message_view(&self, session_id: &str) -> StoreResult<TaskMessageView> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction().map_err(map_sql)?;
        let projection = read_message_projection(&tx, session_id)?;
        let events = read_message_events(&tx, session_id)?
            .into_iter()
            .map(|event| event.redacted(self.hooks()))
            .collect();
        Ok(projection.view(events))
    }

    fn retain_task_messages(
        &self,
        session_id: &str,
        through_event_id: Option<&str>,
        retained_event_ids: &[String],
    ) -> StoreResult<()> {
        let mut conn = self.lock_for_mutation()?;
        let tx = write_transaction(&mut conn)?;
        let mut projection = read_message_projection(&tx, session_id)?;
        projection.retain(
            &read_message_events(&tx, session_id)?,
            through_event_id,
            retained_event_ids,
        )?;
        tx.execute(
            "INSERT INTO agent_task_message_projections (session_id, record_json) VALUES (?1, ?2)
             ON CONFLICT(session_id) DO UPDATE SET record_json = excluded.record_json",
            params![session_id, encode(&projection)?],
        )
        .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        Ok(())
    }

    fn claim_task_execution(&self, task_id: &str) -> StoreResult<crate::TaskExecutionLease> {
        match &self.inner.mutation_admission {
            MutationAdmission::InMemory => {
                crate::TaskExecutionLease::try_acquire_memory(&self.inner.task_claims, task_id)
            }
            MutationAdmission::Shared { store_dir } => {
                crate::TaskExecutionLease::try_acquire_file(store_dir, task_id).map_err(|error| {
                    let kind = match &error {
                        SessionLeaseError::Contended { path }
                            if path == &super::super::lease::project_lease_path(store_dir) =>
                        {
                            StoreContention::MaintenanceActive
                        }
                        _ => StoreContention::TaskExecutionHeld,
                    };
                    map_lease(error, kind)
                })
            }
            MutationAdmission::ReadOnly | MutationAdmission::Maintenance { .. } => {
                Err(StoreError::InvalidInput(
                    "task execution requires an ordinary writable store".into(),
                ))
            }
        }
    }
}

fn next_id(conn: &Connection) -> StoreResult<String> {
    let id: i64 = conn
        .query_row(
            "SELECT next_id FROM agent_control_event_sequence WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(map_sql)?;
    let next = id
        .checked_add(1)
        .filter(|_| id > 0)
        .ok_or_else(|| StoreError::Backend("control event sequence exhausted".into()))?;
    conn.execute(
        "UPDATE agent_control_event_sequence SET next_id = ?1 WHERE singleton = 1",
        params![next],
    )
    .map_err(map_sql)?;
    Ok(format!("event_{id:016x}"))
}
