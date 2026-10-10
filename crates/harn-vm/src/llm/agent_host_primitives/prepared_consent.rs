//! Resolve consent observations and present preparation refusals as tool results.

use std::sync::Arc;

use crate::tool_registry::preparation_scope::PreparedInvocation;
use crate::value::VmValue;

// Keep preparation state out of the dispatcher's already large async frame.
#[inline(never)]
pub(super) fn prepare<'a>(
    ctx: &'a crate::vm::AsyncBuiltinCtx,
    tools: Option<&'a VmValue>,
    tool_name: &'a str,
    tool_id: &'a str,
    tool_args: &'a serde_json::Value,
    session_id: &'a str,
) -> std::pin::Pin<
    Box<impl std::future::Future<Output = Result<Option<Arc<PreparedInvocation>>, VmValue>> + 'a>,
> {
    Box::pin(async move {
        crate::agent_sessions::scope_current_tool_call(
            tool_id.to_string(),
            crate::llm::agent_tool_preparation::prepare(
                ctx, tools, tool_name, tool_args, session_id,
            ),
        )
        .await
        .map_err(|error| {
            let category = if crate::value::error_to_category(&error)
                == crate::value::ErrorCategory::SchemaValidation
            {
                crate::agent_events::ToolCallErrorCategory::SchemaValidation
            } else {
                crate::agent_events::ToolCallErrorCategory::PermissionDenied
            };
            crate::stdlib::json_to_vm_value(&super::agent_primitive_denied_tool(
                tool_name,
                tool_id,
                tool_args,
                error.to_string(),
                category,
                None,
                None,
            ))
        })
    })
}
