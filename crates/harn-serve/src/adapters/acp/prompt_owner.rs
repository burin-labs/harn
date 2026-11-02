//! One admitted execution's task identity, cancellation, and lazy HITL log.
use super::sessions::mark_cancelled_session;
use super::*;

#[derive(Clone)]
pub(super) struct PromptExecutionOwner {
    task: Option<TaskPromptTarget>,
    cancellation: Arc<PromptCancellation>,
    event_log: Arc<std::sync::Mutex<Option<Arc<harn_vm::event_log::AnyEventLog>>>>,
}

impl ConcurrentSessionControl {
    pub(super) fn admit_and_bind_prompt(
        &self,
        task: Option<TaskPromptTarget>,
        cancellation: Arc<PromptCancellation>,
        admit: impl FnOnce() -> Result<(), TaskAdmissionError>,
    ) -> Result<PromptExecutionOwner, TaskAdmissionError> {
        let mut current = self
            .prompt_owner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        admit()?;
        let owner = PromptExecutionOwner::new(task, cancellation);
        *current = Some(owner.clone());
        self.set_prompt_active(true);
        Ok(owner)
    }

    pub(super) fn prompt_event_log(&self) -> Option<Arc<harn_vm::event_log::AnyEventLog>> {
        self.prompt_owner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .and_then(PromptExecutionOwner::event_log)
    }

    pub(super) fn close_prompt_owner(&self) -> Option<PromptExecutionOwner> {
        self.prompt_owner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    // Keep the binding locked through the targeted control's effect. Cleanup
    // closes it before taking the input queue, so a late input cannot escape.
    pub(super) fn with_current_task<T>(
        &self,
        task_id: &str,
        effect: impl FnOnce(&PromptExecutionOwner) -> Result<T, TaskAdmissionError>,
    ) -> Result<T, TaskAdmissionError> {
        let owner = self
            .prompt_owner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if owner
            .as_ref()
            .and_then(PromptExecutionOwner::task)
            .map(|task| task.task_id.as_str())
            != Some(task_id)
        {
            return Err(TaskAdmissionError::TaskNotCurrent);
        }
        effect(owner.as_ref().expect("matched current prompt"))
    }
}

impl PromptExecutionOwner {
    pub(super) fn new(
        task: Option<TaskPromptTarget>,
        cancellation: Arc<PromptCancellation>,
    ) -> Self {
        Self {
            task,
            cancellation,
            event_log: Arc::default(),
        }
    }

    pub(super) fn task(&self) -> Option<&TaskPromptTarget> {
        self.task.as_ref()
    }

    pub(super) fn capture_current(&self) {
        *self
            .event_log
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = harn_vm::event_log::active_event_log();
    }

    pub(super) fn event_log(&self) -> Option<Arc<harn_vm::event_log::AnyEventLog>> {
        self.event_log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    #[cfg(test)]
    pub(super) fn set_event_log(&self, log: Arc<harn_vm::event_log::AnyEventLog>) {
        *self.event_log.lock().unwrap() = Some(log);
    }
}

impl PromptCancellation {
    pub(super) async fn wait_cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.cancelled.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn cancel(&self) -> bool {
        let newly_cancelled = !self.cancelled.swap(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        newly_cancelled
    }
}

/// Untargeted stops retain session semantics; task stops select one admitted token.
pub(super) fn cancel_session_prompt(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    controls: &ConcurrentSessionControls,
    params: &serde_json::Value,
) -> Option<Result<bool, TaskAdmissionError>> {
    let session_id = session_id_param(params)?;
    lookup_session_cancellation(cancellations, &session_id)?;
    let target = match TaskPromptTarget::from_params(params) {
        Ok(None) => return mark_cancelled_session(cancellations, params).map(Ok),
        Ok(Some(target)) => target,
        Err(error) => return Some(Err(error)),
    };
    Some(
        controls
            .registered_control(&session_id)
            .ok_or(TaskAdmissionError::TaskNotCurrent)
            .and_then(|control| {
                control.with_current_task(&target.task_id, |owner| {
                    let newly_cancelled = owner.cancellation.cancel();
                    if newly_cancelled {
                        cancel_session_command_handles(&session_id);
                    }
                    Ok(newly_cancelled)
                })
            }),
    )
}

impl ConcurrentSessionControls {
    pub(super) fn registered_control(&self, session_id: &str) -> Option<ConcurrentSessionControl> {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(session_id)
            .cloned()
    }
}

pub(super) fn record_cancel_refusal(
    controls: &ConcurrentSessionControls,
    session_id: &str,
    params: &serde_json::Value,
    error: TaskAdmissionError,
) {
    controls.emit_control_event(&harn_vm::agent_events::AgentEvent::ControlOutcome {
        session_id: session_id.to_owned(),
        control_id: control_id(),
        method: "session/cancel".into(),
        outcome: "rejected".into(),
        status: "rejected".into(),
        actor: control_actor_from_params(params),
        target: serde_json::json!({"sessionId":session_id}),
        reason: Some(error.message().into()),
        metadata: serde_json::Value::Null,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn normal_task_cancel_is_scoped_and_untargeted_session_cancel_stays_global() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
        server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":".","environmentPolicy":{"kind":"isolated","grants":[]}}})).await;
        let created: serde_json::Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let session = server.sessions.get(&session_id).unwrap();
        let current = session.cancellation.prepare_prompt();
        let queued = session.cancellation.prepare_prompt();
        session
            .concurrent_control
            .admit_and_bind_prompt(
                Some(TaskPromptTarget {
                    task_id: "current".into(),
                }),
                current.clone(),
                || Ok(()),
            )
            .unwrap();
        session.concurrent_control.set_prompt_active(true);
        for (id, target, code) in [
            (2, serde_json::json!({"taskId":"queued"}), -32000),
            (3, serde_json::json!({"taskId":17}), -32602),
        ] {
            server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0","id":id,"method":"session/cancel","params":{"sessionId":session_id,"_meta":{"harn":{"task":target}}}})).await;
            let response: serde_json::Value =
                serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
            assert_eq!(response["error"]["code"], code);
            assert!(!current.cancelled.load(Ordering::SeqCst));
            assert!(!queued.cancelled.load(Ordering::SeqCst));
        }
        let stop = serde_json::json!({"jsonrpc":"2.0","id":4,"method":"session/cancel","params":{"sessionId":session_id,"_meta":{"harn":{"task":TaskPromptTarget {task_id:"current".into()}}}}});
        let prepared = prepare_session_request(
            &server.session_cancellations,
            &server.concurrent_controls,
            &stop,
        );
        assert!(current.cancelled.load(Ordering::SeqCst));
        assert!(!queued.cancelled.load(Ordering::SeqCst));
        server
            .sessions
            .get(&session_id)
            .unwrap()
            .concurrent_control
            .admit_and_bind_prompt(
                Some(TaskPromptTarget {
                    task_id: "queued".into(),
                }),
                queued.clone(),
                || Ok(()),
            )
            .unwrap();
        server.handle_prepared_message(stop, prepared).await;
        let response: serde_json::Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert_eq!(response["result"]["status"], "cancelled");
        assert!(current.cancelled.load(Ordering::SeqCst));
        assert!(!queued.cancelled.load(Ordering::SeqCst));
        server.handle_incoming_message(serde_json::json!({"jsonrpc":"2.0","id":5,"method":"session/cancel","params":{"sessionId":session_id}})).await;
        let response: serde_json::Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert_eq!(response["result"]["status"], "cancelled");
        assert!(queued.cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prompt_handoff_keeps_old_task_and_hitl_log_isolated() {
        let control = ConcurrentSessionControl::new();
        let old_log = harn_vm::event_log::install_memory_for_current_thread(32);
        let old = control
            .admit_and_bind_prompt(
                Some(TaskPromptTarget {
                    task_id: "old".into(),
                }),
                Arc::default(),
                || Ok(()),
            )
            .unwrap();
        old.capture_current();
        assert!(control.with_current_task("old", |_| Ok(())).is_ok());
        assert!(matches!(
            control.with_current_task::<()>("new", |_| panic!("wrong task admitted")),
            Err(TaskAdmissionError::TaskNotCurrent)
        ));
        assert_eq!(control.close_prompt_owner().unwrap().task(), old.task());
        assert!(matches!(
            control.with_current_task::<()>("old", |_| panic!("closed task admitted")),
            Err(TaskAdmissionError::TaskNotCurrent)
        ));
        let next = control
            .admit_and_bind_prompt(
                Some(TaskPromptTarget {
                    task_id: "new".into(),
                }),
                Arc::default(),
                || Ok(()),
            )
            .unwrap();
        let next_log = harn_vm::event_log::install_memory_for_current_thread(32);
        next.capture_current();
        old.set_event_log(old_log.clone());
        assert!(Arc::ptr_eq(&old.event_log().unwrap(), &old_log));
        assert!(Arc::ptr_eq(&control.prompt_event_log().unwrap(), &next_log));
        assert_eq!(old.task().unwrap().task_id, "old");
        assert!(control.with_current_task("new", |_| Ok(())).is_ok());
        assert!(matches!(
            control.with_current_task::<()>("old", |_| panic!("previous task admitted")),
            Err(TaskAdmissionError::TaskNotCurrent)
        ));
    }
}
