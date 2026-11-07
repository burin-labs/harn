use std::future::Future;
use std::sync::{Arc, Mutex};

use crate::agent_events::AgentEvent;
use crate::value::{ErrorCategory, VmError};

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStatus {
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

struct ActivityState {
    id: Option<String>,
    terminal: bool,
}

struct CompactionActivity {
    session_id: String,
    state: Mutex<ActivityState>,
    transport: crate::agent_events::AgentEventTransport,
}

tokio::task_local! {
    static ACTIVITY: Option<Arc<CompactionActivity>>;
}

impl CompactionActivity {
    fn emit(&self, id: String, status: CompactionStatus, error: Option<String>) {
        self.transport.with(|| {
            crate::llm::emit_live_agent_event_sync(&AgentEvent::CompactionUpdated {
                session_id: self.session_id.clone(),
                compaction_id: id,
                status,
                error,
            });
        });
    }

    fn begin(&self) -> Result<String, VmError> {
        let id = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.terminal {
                return Err(VmError::Runtime(
                    "the session compaction has already committed".into(),
                ));
            }
            if let Some(id) = &state.id {
                return Ok(id.clone());
            }
            let id = super::new_compaction_receipt_id();
            state.id = Some(id.clone());
            id
        };
        self.emit(id.clone(), CompactionStatus::InProgress, None);
        Ok(id)
    }

    fn terminate(&self, status: CompactionStatus, error: Option<String>) {
        let id = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.terminal {
                return;
            }
            state.terminal = true;
            state.id.clone()
        };
        if let Some(id) = id {
            self.emit(id, status, error);
        }
    }
}

impl Drop for CompactionActivity {
    fn drop(&mut self) {
        self.terminate(CompactionStatus::Cancelled, None);
    }
}

/// Keep one admitted identity alive through preparation and session commit.
pub(crate) async fn scope_compaction_activity<T>(
    session_id: &str,
    work: impl Future<Output = Result<T, VmError>>,
) -> Result<T, VmError> {
    let activity = Arc::new(CompactionActivity {
        session_id: session_id.to_string(),
        state: Mutex::new(ActivityState {
            id: None,
            terminal: false,
        }),
        transport: crate::agent_events::transport::current(),
    });
    let result = ACTIVITY.scope(Some(activity.clone()), work).await;
    match &result {
        Err(error) => activity.terminate(
            if error.is_uncatchable_control_flow()
                || crate::value::error_to_category(error) == ErrorCategory::Cancelled
            {
                CompactionStatus::Cancelled
            } else {
                CompactionStatus::Failed
            },
            Some(error.to_string()),
        ),
        Ok(_) => activity.terminate(
            CompactionStatus::Failed,
            Some("compaction ended before its session commit".into()),
        ),
    }
    result
}

pub(crate) fn begin_compaction_activity() -> Result<(), VmError> {
    ACTIVITY
        .try_with(|activity| {
            activity
                .as_ref()
                .map(|activity| activity.begin())
                .transpose()
        })
        .unwrap_or(Ok(None))
        .map(|_| ())
}

pub(crate) fn active_compaction_receipt_id() -> Option<String> {
    ACTIVITY
        .try_with(|activity| {
            let activity = activity.as_ref()?;
            activity
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .id
                .clone()
        })
        .ok()
        .flatten()
}

pub(crate) fn commit_compaction_activity(session_id: &str, receipt_id: &str) {
    let _ = ACTIVITY.try_with(|activity| {
        let Some(activity) = activity else { return };
        let mut state = activity
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if activity.session_id == session_id && state.id.as_deref() == Some(receipt_id) {
            state.terminal = true;
        }
    });
}

pub(crate) fn without_compaction_activity<T>(work: impl FnOnce() -> T) -> T {
    ACTIVITY.sync_scope(None, work)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_events::{AgentEventSink, AgentEventTransport};

    #[derive(Default)]
    struct Updates(Mutex<Vec<(String, CompactionStatus)>>);

    impl AgentEventSink for Updates {
        fn handle_event(&self, event: &AgentEvent) {
            if let AgentEvent::CompactionUpdated {
                compaction_id,
                status,
                ..
            } = event
            {
                self.0
                    .lock()
                    .unwrap()
                    .push((compaction_id.clone(), *status));
            }
        }
    }

    #[tokio::test]
    async fn only_admitted_compactions_publish_and_a_dropped_wait_keeps_its_transport() {
        let updates = Arc::new(Updates::default());
        let transport = AgentEventTransport::new(updates.clone());
        transport
            .scope(scope_compaction_activity("s", async { Ok(()) }))
            .await
            .unwrap();
        assert!(
            updates.0.lock().unwrap().is_empty(),
            "a no-op must not invent an activity"
        );
        transport
            .scope(scope_compaction_activity("s", async {
                begin_compaction_activity()?;
                let id = active_compaction_receipt_id().unwrap();
                begin_compaction_activity()?;
                assert_eq!(active_compaction_receipt_id().as_deref(), Some(id.as_str()));
                assert!(without_compaction_activity(active_compaction_receipt_id).is_none());
                commit_compaction_activity("s", &id);
                Ok(())
            }))
            .await
            .unwrap();
        assert_eq!(
            updates.0.lock().unwrap().len(),
            1,
            "commit is published by the session owner"
        );

        let mut dropped = Box::pin(transport.scope(scope_compaction_activity("s", async {
            begin_compaction_activity()?;
            std::future::pending::<()>().await;
            Ok(())
        })));
        assert!(futures::poll!(dropped.as_mut()).is_pending());
        drop(dropped);
        let events = updates.0.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert_ne!(events[0].0, events[1].0);
        assert_eq!(events[1].0, events[2].0);
        assert_eq!(events[2].1, CompactionStatus::Cancelled);
    }

    #[tokio::test]
    async fn admitted_failure_and_missing_commit_are_terminal_on_the_same_identity() {
        for fail in [false, true] {
            let updates = Arc::new(Updates::default());
            let transport = AgentEventTransport::new(updates.clone());
            let result = transport
                .scope(scope_compaction_activity("s", async {
                    begin_compaction_activity()?;
                    if fail {
                        Err(VmError::Runtime("strategy failed".into()))
                    } else {
                        Ok(())
                    }
                }))
                .await;
            assert_eq!(result.is_err(), fail);
            let events = updates.0.lock().unwrap();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].0, events[1].0);
            assert_eq!(events[0].1, CompactionStatus::InProgress);
            assert_eq!(events[1].1, CompactionStatus::Failed);
        }
    }
}
