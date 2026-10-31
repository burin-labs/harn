//! Host observation of admitted tool executions, so a host can attribute the
//! workspace changes each call makes.

use std::sync::Arc;

use crate::tool_annotations::SideEffectLevel;

use super::HostBridge;

/// Stages of executing one admitted tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToolExecutionStage {
    /// The handler is about to run.
    Begin,
    /// The handler finished, failed, or was cancelled.
    End,
}

/// Host callback around each admitted tool execution: tool call id, the
/// tool's declared side-effect class, and the stage. It runs synchronously on
/// the agent's execution path. For [`ToolExecutionStage::Begin`] it returns
/// whether the host began tracking the call; `End` is sent only for calls it
/// began, so a refused begin never closes another call's window.
pub type ToolExecutionBoundary =
    Arc<dyn Fn(&str, SideEffectLevel, ToolExecutionStage) -> bool + Send + Sync>;

/// Sends [`ToolExecutionStage::End`] when dropped, so an early return or a
/// cancelled dispatch still closes the window it opened.
pub struct ToolExecutionGuard {
    boundary: ToolExecutionBoundary,
    tool_call_id: String,
    side_effect: SideEffectLevel,
}

impl Drop for ToolExecutionGuard {
    fn drop(&mut self) {
        (self.boundary)(
            &self.tool_call_id,
            self.side_effect,
            ToolExecutionStage::End,
        );
    }
}

impl HostBridge {
    /// Observe each admitted tool execution before and after its handler runs.
    pub fn set_tool_execution_boundary(&self, boundary: ToolExecutionBoundary) {
        *self
            .tool_execution_boundary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(boundary);
    }

    /// Open a tool execution window. Read-only tools and calls without an id
    /// cannot change the workspace attributably, so they open none.
    pub(crate) fn begin_tool_execution(
        &self,
        tool_call_id: &str,
        side_effect: SideEffectLevel,
    ) -> Option<ToolExecutionGuard> {
        if tool_call_id.is_empty() || side_effect == SideEffectLevel::ReadOnly {
            return None;
        }
        let boundary = self
            .tool_execution_boundary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        boundary(tool_call_id, side_effect, ToolExecutionStage::Begin).then(|| ToolExecutionGuard {
            boundary,
            tool_call_id: tool_call_id.to_string(),
            side_effect,
        })
    }
}
