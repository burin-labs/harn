//! Host callbacks on the agent's execution path: one around each queued user
//! message's delivery, one around each admitted tool execution so a host can
//! attribute the workspace changes each call makes.
//!
//! A host callback may block for seconds (a workspace scan, a durable write).
//! The agent awaits each one on Tokio's blocking pool under its ambient scope,
//! so the thread that polls the agent keeps serving every other task on it;
//! only the delivery or tool call that needs the result waits.

use std::sync::{Arc, Mutex, PoisonError};

use crate::tool_annotations::SideEffectLevel;

use super::HostBridge;

/// Stages of delivering a queued user message to the agent transcript.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UserInjectionStage {
    /// The message is about to enter the transcript.
    Before,
    /// The message entered the transcript.
    Delivered,
    /// The transcript rejected the message.
    Failed,
}

/// Host callback observing transcript delivery of a queued user message.
pub type UserInjectionBoundary = Arc<dyn Fn(&str, &str, UserInjectionStage) + Send + Sync>;

/// Stages of executing one admitted tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToolExecutionStage {
    /// The handler is about to run.
    Begin,
    /// The handler finished, failed, or was cancelled.
    End,
}

/// Admission result for a tool execution tracking boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolExecutionAdmission {
    /// No tracking applies to this call, so it proceeds without a guard.
    Untracked,
    /// Tracking durably opened a call window.
    Tracked,
    /// Tracking could not safely open a call window, so the handler must not run.
    Refused(String),
}

/// Host callback around each admitted tool execution: tool call id, the
/// tool's declared side-effect class, and the stage. At `Begin`, `Untracked`
/// preserves calls outside checkpoint scope, `Tracked` opens a guard, and
/// `Refused` blocks the handler. `End` is sent only for calls that began as
/// `Tracked`.
pub type ToolExecutionBoundary =
    Arc<dyn Fn(&str, SideEffectLevel, ToolExecutionStage) -> ToolExecutionAdmission + Send + Sync>;

/// Result of asking the bridge's installed tracking boundary to admit a call.
pub(crate) enum ToolExecutionStart {
    Untracked,
    Tracked(ToolExecutionGuard),
    Refused(String),
}

/// The host callbacks installed on one bridge.
#[derive(Default)]
pub(super) struct HostBoundaries {
    user_injection: Mutex<Option<UserInjectionBoundary>>,
    tool_execution: Mutex<Option<ToolExecutionBoundary>>,
}

/// Run one host callback on the blocking pool and await it. `None` means the
/// runtime is shutting down; a panic in the callback resumes on the caller.
async fn await_host_callback<T: Send + 'static>(
    callback: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    match crate::orchestration::run_blocking_with_ambient(callback).await {
        Ok(value) => Some(value),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_) => None,
    }
}

/// An open tool execution window. [`ToolExecutionGuard::finish`] sends
/// [`ToolExecutionStage::End`] off the polling thread once the handler
/// returns; dropping an unfinished guard sends it inline.
pub struct ToolExecutionGuard {
    window: Option<ToolExecutionWindow>,
}

struct ToolExecutionWindow {
    boundary: ToolExecutionBoundary,
    tool_call_id: String,
    side_effect: SideEffectLevel,
}

impl ToolExecutionGuard {
    /// Close the window and wait for the host to finish attributing it, so the
    /// next window or boundary for this session starts after this one ended.
    pub(crate) async fn finish(mut self) {
        if let Some(window) = self.window.take() {
            // A cancelled wait leaves the blocking task to finish `End`.
            await_host_callback(move || window.end()).await;
        }
    }
}

/// Only a cancelled dispatch, or a `Begin` whose waiter was cancelled, reaches
/// this with the window open. `End` runs inline there rather than on a detached
/// task: a detached `End` could still be running when the session's next
/// boundary capture or `Begin` starts, and the host refuses those while a call
/// is pending. Cancellation is rare, so the inline cost is bounded to it.
impl Drop for ToolExecutionGuard {
    fn drop(&mut self) {
        if let Some(window) = self.window.take() {
            window.end();
        }
    }
}

impl ToolExecutionWindow {
    fn end(self) {
        if let ToolExecutionAdmission::Refused(detail) = (self.boundary)(
            &self.tool_call_id,
            self.side_effect,
            ToolExecutionStage::End,
        ) {
            // The handler has already finished; this cannot change its
            // outcome. Keep the tracking failure visible in the host log and
            // let the host's checkpoint owner refuse later tracked calls.
            tracing::warn!(
                tool_call_id = %self.tool_call_id,
                detail = %detail,
                "tool completed but checkpoint attribution could not be finalized"
            );
        }
    }
}

impl HostBridge {
    /// Observe a queued user message before insertion and after its outcome.
    pub fn set_user_injection_boundary(&self, boundary: UserInjectionBoundary) {
        *self
            .boundaries
            .user_injection
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(boundary);
    }

    pub(crate) async fn user_injection_stage(
        &self,
        session_id: &str,
        message_id: &str,
        stage: UserInjectionStage,
    ) {
        let boundary = self
            .boundaries
            .user_injection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(boundary) = boundary {
            let (session_id, message_id) = (session_id.to_string(), message_id.to_string());
            await_host_callback(move || boundary(&session_id, &message_id, stage)).await;
        }
    }

    /// Observe each admitted tool execution before and after its handler runs.
    pub fn set_tool_execution_boundary(&self, boundary: ToolExecutionBoundary) {
        *self
            .boundaries
            .tool_execution
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(boundary);
    }

    /// Open a tool execution window. Read-only tools and calls without an id
    /// cannot change the workspace attributably, so they open none.
    pub(crate) async fn begin_tool_execution(
        &self,
        tool_call_id: &str,
        side_effect: SideEffectLevel,
    ) -> ToolExecutionStart {
        if tool_call_id.is_empty() || side_effect == SideEffectLevel::ReadOnly {
            return ToolExecutionStart::Untracked;
        }
        let Some(boundary) = self
            .boundaries
            .tool_execution
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return ToolExecutionStart::Untracked;
        };
        let tool_call_id = tool_call_id.to_string();
        // The guard is built on the blocking thread, so a cancelled waiter
        // drops it there or with the join handle, and the window still closes.
        await_host_callback(move || {
            match boundary(&tool_call_id, side_effect, ToolExecutionStage::Begin) {
                ToolExecutionAdmission::Untracked => ToolExecutionStart::Untracked,
                ToolExecutionAdmission::Tracked => {
                    ToolExecutionStart::Tracked(ToolExecutionGuard {
                        window: Some(ToolExecutionWindow {
                            boundary,
                            tool_call_id,
                            side_effect,
                        }),
                    })
                }
                ToolExecutionAdmission::Refused(detail) => ToolExecutionStart::Refused(detail),
            }
        })
        .await
        .unwrap_or_else(|| {
            ToolExecutionStart::Refused("the runtime stopped before tracking began".to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// A boundary callback that blocks must not stall the thread polling the
    /// agent: a sibling task on the same current-thread `LocalSet` has to run
    /// while `Begin` and `End` are in progress. Each stage waits for the
    /// sibling to acknowledge that it saw the stage start; on a thread the
    /// callback blocks, that acknowledgement can never arrive.
    #[test]
    fn a_blocking_tool_execution_boundary_leaves_the_polling_thread_live() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let _environment =
                crate::security::session_environment::declare_session_environment_if_absent(
                    crate::security::session_environment::SessionEnvironment::inherited(),
                );
            let started = Arc::new(AtomicBool::new(false));
            let (ack, acked) = mpsc::channel::<()>();
            let acked = Arc::new(Mutex::new(acked));
            let stages = Arc::new(Mutex::new(Vec::new()));
            let bridge = super::super::tests::test_bridge();
            bridge.set_tool_execution_boundary(Arc::new({
                let (started, stages) = (started.clone(), stages.clone());
                move |_, _, stage| {
                    let environment =
                        crate::security::session_environment::current_session_environment();
                    started.store(true, Ordering::SeqCst);
                    let live = acked
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .is_ok();
                    stages
                        .lock()
                        .unwrap()
                        .push((stage, live, environment.is_some()));
                    if live {
                        ToolExecutionAdmission::Tracked
                    } else {
                        ToolExecutionAdmission::Refused("the polling thread was blocked".into())
                    }
                }
            }));
            let sibling = tokio::task::spawn_local(async move {
                loop {
                    if started.swap(false, Ordering::SeqCst) && ack.send(()).is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });

            let start = bridge
                .begin_tool_execution("call-1", SideEffectLevel::WorkspaceWrite)
                .await;
            let ToolExecutionStart::Tracked(guard) = start else {
                panic!("Begin did not open a window: {:?}", stages.lock().unwrap());
            };
            guard.finish().await;
            sibling.abort();

            assert_eq!(
                *stages.lock().unwrap(),
                vec![
                    (ToolExecutionStage::Begin, true, true),
                    (ToolExecutionStage::End, true, true),
                ],
                "each stage must see the sibling run and keep the session environment"
            );
        });
    }
}
