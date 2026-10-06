//! Resolve private invocation facts before consent and validate every attempt.

use std::sync::Arc;

use serde_json::Value;

use crate::tool_registry::preparation_scope::{current_invocation, preparation};
pub(super) use crate::tool_registry::preparation_scope::{scope, PreparedInvocation};
use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};

fn rejected(message: impl Into<String>) -> VmError {
    VmError::CategorizedError {
        message: message.into(),
        category: ErrorCategory::ToolRejected,
    }
}

// Keep the child interpreter and its closure future off every consent and retry
// caller's async state machine. Construct the boxed future on this owner frame.
#[inline(never)]
fn resolve<'a>(
    ctx: &'a crate::vm::AsyncBuiltinCtx,
    prepare: &'a VmClosure,
    arguments: &'a Value,
    session_id: &'a str,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<Value, VmError>> + 'a>> {
    Box::pin(async move {
        let mut vm = Box::new(ctx.child_vm());
        let args = crate::stdlib::json_to_vm_value(arguments);
        let value = preparation(crate::orchestration::scope_read_only_invocation(
            session_id,
            vm.call_closure_pub(prepare, &[args]),
        ))
        .await?;
        let facts = crate::tool_registry::result_to_json(&value).map_err(rejected)?;
        if !facts.is_object() || !facts.get("operation").is_some_and(Value::is_object) {
            return Err(rejected(
                "tool preparation must return an object with an operation object",
            ));
        }
        Ok(facts)
    })
}

pub(super) async fn prepare(
    ctx: &crate::vm::AsyncBuiltinCtx,
    registry: Option<&VmValue>,
    name: &str,
    arguments: &Value,
    session_id: &str,
) -> Result<Option<Arc<PreparedInvocation>>, VmError> {
    let Some(prepare) = super::agent_tools::find_tool_closure(registry, name, "prepare") else {
        return Ok(None);
    };
    super::agent_tool_governance::require_registry_membership(registry, name).map_err(rejected)?;
    super::agent_tool_governance::prepared_handler_catalog(
        registry.ok_or_else(|| rejected("prepared tool requires its registry"))?,
        name,
    )?
    .validate_input(name, arguments)
    .map_err(|error| VmError::CategorizedError {
        message: error.to_string(),
        category: ErrorCategory::SchemaValidation,
    })?;
    let facts = resolve(ctx, &prepare, arguments, session_id).await?;
    Ok(Some(Arc::new(PreparedInvocation {
        name: name.into(),
        session_id: session_id.into(),
        arguments: arguments.clone(),
        facts,
        prepare,
    })))
}

pub(super) async fn validate_handler(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    registry: Option<&VmValue>,
    name: &str,
    arguments: &Value,
) -> Result<(), VmError> {
    match current_invocation() {
        Some(bound) => {
            let ctx = ctx.ok_or_else(|| rejected("prepared tool requires a VM context"))?;
            if name != bound.name
                || arguments != &bound.arguments
                || resolve(ctx, &bound.prepare, arguments, &bound.session_id).await? != bound.facts
            {
                return Err(rejected(
                    "tool execution facts changed after preparation; request new approval",
                ));
            }
            Ok(())
        }
        None if super::agent_tools::find_tool_closure(registry, name, "prepare").is_some() => {
            Err(rejected("tool requires preparation before consent"))
        }
        None => Ok(()),
    }
}

#[cfg(all(test, unix))]
#[path = "agent_tool_preparation_tests.rs"]
mod tests;
