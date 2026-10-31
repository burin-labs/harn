//! Frame-isolated construction of the scoped tool-dispatch future.

use crate::value::{VmError, VmValue};

use super::agent_tools;

type HookDispatchResult<T> = (Result<T, VmError>, Vec<serde_json::Value>);

/// Run and apply pre-tool interception with their separate report scopes.
/// Construct the scoped future on this short-lived frame, as with execution
/// below, rather than materializing it in the retained dispatch frame.
#[inline(never)]
pub(super) fn pin_pre_tool_interception<'a>(
    ctx: &'a crate::vm::AsyncBuiltinCtx,
    tool_id: String,
    tool_name: &'a str,
    tool_args: &'a mut serde_json::Value,
) -> std::pin::Pin<Box<impl std::future::Future<Output = HookDispatchResult<Option<String>>> + 'a>>
{
    Box::pin(async move {
        let (action, mut reports) = crate::orchestration::scope_hook_reminder_reports(
            crate::agent_sessions::scope_current_tool_call(tool_id.clone(), async {
                crate::orchestration::run_pre_tool_hooks_with_ctx(Some(ctx), tool_name, tool_args)
                    .await
            }),
        )
        .await;
        let action = match action {
            Ok(action) => action,
            Err(error) => return (Err(error), reports),
        };
        let (result, applied_reports) = crate::orchestration::scope_hook_reminder_reports(
            crate::agent_sessions::scope_current_tool_call(tool_id, async {
                crate::orchestration::apply_pre_tool_action(action, tool_args)
            }),
        )
        .await;
        reports.extend(applied_reports);
        (result, reports)
    })
}

/// Preserve post-tool output, denial/truncation metadata and ordered reminder
/// reports without retaining hook-future construction in the dispatch frame.
#[inline(never)]
pub(super) fn pin_post_tool_interception<'a>(
    ctx: &'a crate::vm::AsyncBuiltinCtx,
    tool_id: String,
    tool_name: &'a str,
    tool_args: &'a serde_json::Value,
    rendered: &'a str,
) -> std::pin::Pin<
    Box<
        impl std::future::Future<
                Output = HookDispatchResult<crate::orchestration::PostToolHookResult>,
            > + 'a,
    >,
> {
    Box::pin(crate::orchestration::scope_hook_reminder_reports(
        crate::agent_sessions::scope_current_tool_call(tool_id, async move {
            crate::orchestration::run_post_tool_hooks_with_ctx(
                Some(ctx),
                tool_name,
                tool_args,
                rendered,
            )
            .await
        }),
    ))
}

/// The borrowed inputs one tool dispatch reads, bundled so
/// [`pin_scoped_tool_dispatch`] stays inside clippy's argument limit.
pub(super) struct ToolDispatchRequest<'a> {
    pub(super) ctx: &'a crate::vm::AsyncBuiltinCtx,
    pub(super) tool_name: &'a str,
    pub(super) tool_args: &'a serde_json::Value,
    pub(super) tools: Option<&'a VmValue>,
    pub(super) audience: crate::tool_registry::ToolAudience,
    pub(super) mcp_clients:
        Option<&'a std::collections::BTreeMap<String, crate::mcp::VmMcpClientHandle>>,
    pub(super) bridge: Option<&'a std::sync::Arc<crate::bridge::HostBridge>>,
    pub(super) tool_retries: usize,
    pub(super) tool_backoff_ms: u64,
    pub(super) prepared_invocation:
        Option<std::sync::Arc<super::agent_tool_preparation::PreparedInvocation>>,
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
    let window_id = tool_id.clone();
    Box::pin(crate::orchestration::scope_agent_session(
        session_id,
        crate::agent_sessions::scope_current_tool_call(tool_id, async move {
            // Lives as long as this future, which the caller keeps until the
            // outcome (a cancelled one included), so the host's attribution
            // window covers every write the handler committed.
            let _execution_window = match request.bridge {
                None => None,
                Some(bridge) => {
                    let annotations = super::agent_host_primitives::tool_catalog::annotations_for(
                        request.tools,
                        request.tool_name,
                    );
                    match bridge.begin_tool_execution(
                        &window_id,
                        annotations.map_or_else(Default::default, |a| a.side_effect_level),
                    ) {
                        crate::bridge::ToolExecutionStart::Untracked => None,
                        crate::bridge::ToolExecutionStart::Tracked(guard) => Some(guard),
                        crate::bridge::ToolExecutionStart::Refused(detail) => {
                            return agent_tools::ToolDispatchOutcome {
                                result: Err(crate::value::VmError::CategorizedError {
                                    message: format!(
                                    "workspace checkpoint tracking refused this tool call: {detail}"
                                ),
                                    category: crate::value::ErrorCategory::ToolRejected,
                                }),
                                executor: None,
                                handler_outcome: None,
                            };
                        }
                    }
                }
            };
            super::agent_tool_preparation::scope(
                request.prepared_invocation,
                agent_tools::dispatch_tool_execution_with_mcp(
                    (Some(request.ctx), request.audience),
                    request.tool_name,
                    request.tool_args,
                    request.tools,
                    request.mcp_clients,
                    request.bridge,
                    request.tool_retries,
                    request.tool_backoff_ms,
                ),
            )
            .await
        }),
    ))
}
