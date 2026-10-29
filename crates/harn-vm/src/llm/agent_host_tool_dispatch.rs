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
    Box::pin(crate::orchestration::scope_agent_session(
        session_id,
        crate::agent_sessions::scope_current_tool_call(tool_id.clone(), async move {
            let serve =
                crate::run_tape::serve_tool_call(&tool_id, request.tool_name, request.tool_args);
            let reexecuted = match serve {
                crate::run_tape::ToolServe::Served(recorded) => {
                    return outcome_from_recorded(recorded);
                }
                crate::run_tape::ToolServe::Diverged(error) => {
                    return agent_tools::ToolDispatchOutcome {
                        result: Err(error),
                        executor: None,
                        handler_outcome: None,
                    };
                }
                crate::run_tape::ToolServe::Reexecute => true,
                crate::run_tape::ToolServe::Live => false,
            };
            let recording = crate::run_tape::begin_tool_call(&tool_id);
            let outcome = super::agent_tool_preparation::scope(
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
            .await;
            if recording.is_some() || reexecuted {
                let recorded = recorded_from_outcome(&outcome);
                if reexecuted {
                    crate::run_tape::check_reexecuted_tool_call(&tool_id, &recorded);
                }
                if let Some(recording) = recording {
                    crate::run_tape::finish_tool_call(
                        recording,
                        request.tool_name,
                        request.tool_args,
                        recorded,
                    );
                }
            }
            outcome
        }),
    ))
}

/// Project a dispatch outcome onto the run tape's recorded shape. Error
/// variants without a faithful recorded form are refused, so a branch
/// re-executes those calls instead of serving an approximation.
fn recorded_from_outcome(
    outcome: &agent_tools::ToolDispatchOutcome,
) -> Result<crate::run_tape::RecordedToolOutcome, String> {
    use crate::run_tape::RecordedToolResult;
    let result = match &outcome.result {
        Ok(value) => RecordedToolResult::Ok {
            value: value.clone(),
        },
        Err(VmError::CategorizedError { message, category }) => RecordedToolResult::Categorized {
            message: message.clone(),
            category: category.as_str().to_string(),
        },
        Err(VmError::Thrown(VmValue::String(message))) => RecordedToolResult::Thrown {
            message: message.to_string(),
        },
        Err(other) => return Err(format!("tool error has no recorded shape: {other}")),
    };
    let executor = match &outcome.executor {
        Some(executor) => Some(
            serde_json::to_value(executor)
                .map_err(|err| format!("tool executor does not serialize: {err}"))?,
        ),
        None => None,
    };
    let handler_outcome = outcome.handler_outcome.map(|outcome| {
        match outcome {
            crate::tool_registry::handler_result::HandlerOutcome::Ok => "ok",
            crate::tool_registry::handler_result::HandlerOutcome::Error => "error",
            crate::tool_registry::handler_result::HandlerOutcome::Rejected => "rejected",
        }
        .to_string()
    });
    Ok(crate::run_tape::RecordedToolOutcome {
        result,
        executor,
        handler_outcome,
    })
}

fn outcome_from_recorded(
    recorded: crate::run_tape::RecordedToolOutcome,
) -> agent_tools::ToolDispatchOutcome {
    use crate::run_tape::RecordedToolResult;
    use crate::tool_registry::handler_result::HandlerOutcome;
    let result = match recorded.result {
        RecordedToolResult::Ok { value } => Ok(value),
        RecordedToolResult::Categorized { message, category } => Err(VmError::CategorizedError {
            message,
            category: crate::value::ErrorCategory::parse(&category),
        }),
        RecordedToolResult::Thrown { message } => {
            Err(VmError::Thrown(VmValue::String(message.into())))
        }
    };
    agent_tools::ToolDispatchOutcome {
        result,
        executor: recorded
            .executor
            .and_then(|value| serde_json::from_value(value).ok()),
        handler_outcome: recorded
            .handler_outcome
            .as_deref()
            .and_then(|outcome| match outcome {
                "ok" => Some(HandlerOutcome::Ok),
                "error" => Some(HandlerOutcome::Error),
                "rejected" => Some(HandlerOutcome::Rejected),
                _ => None,
            }),
    }
}
