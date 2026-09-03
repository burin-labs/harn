//! Frame-isolated construction of the scoped tool-dispatch future.

use crate::value::VmValue;

use super::agent_tools;

/// The borrowed inputs one tool dispatch reads, bundled so
/// [`pin_scoped_tool_dispatch`] stays inside clippy's argument limit.
pub(super) struct ToolDispatchRequest<'a> {
    pub(super) ctx: &'a crate::vm::AsyncBuiltinCtx,
    pub(super) tool_name: &'a str,
    pub(super) tool_args: &'a serde_json::Value,
    pub(super) tools: Option<&'a VmValue>,
    pub(super) mcp_clients:
        Option<&'a std::collections::BTreeMap<String, crate::mcp::VmMcpClientHandle>>,
    pub(super) bridge: Option<&'a std::sync::Arc<crate::bridge::HostBridge>>,
    pub(super) tool_retries: usize,
    pub(super) tool_backoff_ms: u64,
}

/// Build the session- and tool-call-scoped dispatch future on its own frame.
///
/// `Box::pin(expr)` materializes `expr` in the caller's frame before moving it
/// to the heap, so constructing this future inline left its whole state machine
/// resident in `host_agent_dispatch_tool_call` for the entire call, which
/// measured 518097 bytes against clippy's 512000-byte `large_stack_frames`
/// ceiling. That frame is re-entered once per nested tool call, so it is also
/// what a deep sub-agent descent spends its tokio worker stack on: before this
/// split the conformance suite aborted with an overflowed worker stack, and
/// after it the same suite passes. `#[inline(never)]` keeps the temporary on a
/// frame that is popped as soon as the box exists, so neither the lint nor the
/// descent pays for it.
#[inline(never)]
pub(super) fn pin_scoped_tool_dispatch<'a>(
    session_id: String,
    tool_id: String,
    request: ToolDispatchRequest<'a>,
) -> std::pin::Pin<Box<impl std::future::Future<Output = agent_tools::ToolDispatchOutcome> + 'a>> {
    Box::pin(crate::orchestration::scope_agent_session(
        session_id,
        crate::agent_sessions::scope_current_tool_call(tool_id, async move {
            agent_tools::dispatch_tool_execution_with_mcp(
                Some(request.ctx),
                request.tool_name,
                request.tool_args,
                request.tools,
                request.mcp_clients,
                request.bridge,
                request.tool_retries,
                request.tool_backoff_ms,
            )
            .await
        }),
    ))
}
