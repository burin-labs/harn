//! Host callbacks that run synchronously on the agent's execution path: one
//! around each queued user message's delivery, one around each admitted tool
//! execution so a host can attribute the workspace changes each call makes.

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

/// Sends [`ToolExecutionStage::End`] when dropped, so an early return or a
/// cancelled dispatch still closes the window it opened.
pub struct ToolExecutionGuard {
    boundary: ToolExecutionBoundary,
    tool_call_id: String,
    side_effect: SideEffectLevel,
}

impl Drop for ToolExecutionGuard {
    fn drop(&mut self) {
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

    pub(crate) fn user_injection_stage(
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
            boundary(session_id, message_id, stage);
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
    pub(crate) fn begin_tool_execution(
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
        match boundary(tool_call_id, side_effect, ToolExecutionStage::Begin) {
            ToolExecutionAdmission::Untracked => ToolExecutionStart::Untracked,
            ToolExecutionAdmission::Tracked => ToolExecutionStart::Tracked(ToolExecutionGuard {
                boundary,
                tool_call_id: tool_call_id.to_string(),
                side_effect,
            }),
            ToolExecutionAdmission::Refused(detail) => ToolExecutionStart::Refused(detail),
        }
    }
}
