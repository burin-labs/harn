//! Private invocation facts resolved before consent, retained across retries.

use std::future::Future;
use std::sync::Arc;

use harn_builtin_meta::{BuiltinContract, BuiltinExposure, EffectAccess, EffectKind};
use serde_json::Value;

use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};

tokio::task_local! {
    static PREPARING: ();
    static INVOCATION: Option<Arc<PreparedInvocation>>;
}

pub(super) struct PreparedInvocation {
    name: String,
    arguments: Value,
    facts: Value,
    prepare: Arc<VmClosure>,
}

fn rejected(message: impl Into<String>) -> VmError {
    VmError::CategorizedError {
        message: message.into(),
        category: ErrorCategory::ToolRejected,
    }
}

/// Apply before ordinary authority and reviewer exemptions. Preparation is a
/// read of execution facts, never an opportunity to request extra authority.
pub(crate) fn enforce_contract(
    name: &str,
    contract: Option<&BuiltinContract>,
) -> Result<(), VmError> {
    if PREPARING.try_with(|()| true).unwrap_or(false) {
        let permitted = contract.is_some_and(|contract| {
            let pure = matches!(
                contract.exposure,
                BuiltinExposure::PureGlobal | BuiltinExposure::StdlibInternal
            );
            (pure || !contract.effects.is_empty())
                && contract.effects.iter().all(|effect| {
                    effect.access == EffectAccess::Read
                        && matches!(
                            effect.kind,
                            EffectKind::Fs | EffectKind::Env | EffectKind::State | EffectKind::Host
                        )
                })
        });
        if !permitted {
            return Err(rejected(format!(
                "{name} is not a read-only invocation preparation operation"
            )));
        }
    }
    Ok(())
}

async fn resolve(
    ctx: &crate::vm::AsyncBuiltinCtx,
    prepare: &VmClosure,
    arguments: &Value,
) -> Result<Value, VmError> {
    let mut vm = ctx.child_vm();
    let args = crate::stdlib::json_to_vm_value(arguments);
    let overlay: crate::orchestration::CapabilityPolicy =
        serde_json::from_value(serde_json::json!({
        "capabilities": {"workspace": ["read_text"], "environment": ["read"], "state": ["read"], "connector": ["call"]},
            "side_effect_level": "read_only",
            "sandbox_profile": "unrestricted",
        }))
        .expect("literal read-only preparation policy");
    let ceiling = crate::orchestration::current_execution_policy()
        .unwrap_or_default()
        .intersect(&overlay)
        .map_err(rejected)?;
    let value = INVOCATION
        .scope(
            None,
            PREPARING.scope(
                (),
                crate::orchestration::scope_execution_policy(
                    ceiling,
                    vm.call_closure_pub(prepare, &[args]),
                ),
            ),
        )
        .await?;
    let facts = crate::tool_registry::result_to_json(&value).map_err(rejected)?;
    if !facts.is_object() || !facts.get("operation").is_some_and(Value::is_object) {
        return Err(rejected(
            "tool preparation must return an object with an operation object",
        ));
    }
    Ok(facts)
}

pub(super) async fn prepare(
    ctx: &crate::vm::AsyncBuiltinCtx,
    registry: Option<&VmValue>,
    name: &str,
    arguments: &Value,
) -> Result<Option<Arc<PreparedInvocation>>, VmError> {
    let Some(prepare) = super::agent_tools::find_tool_closure(registry, name, "prepare") else {
        return Ok(None);
    };
    super::agent_tool_governance::require_registry_membership(registry, name).map_err(rejected)?;
    let schemas = super::tools::collect_tool_schemas(registry, None);
    super::tools::validate_tool_args(name, arguments, &schemas).map_err(rejected)?;
    let facts = resolve(ctx, &prepare, arguments).await?;
    Ok(Some(Arc::new(PreparedInvocation {
        name: name.into(),
        arguments: arguments.clone(),
        facts,
        prepare,
    })))
}

impl PreparedInvocation {
    pub(super) fn operation(&self) -> &Value {
        &self.facts["operation"]
    }

    async fn validate(
        &self,
        ctx: &crate::vm::AsyncBuiltinCtx,
        name: &str,
        arguments: &Value,
    ) -> Result<(), VmError> {
        if name != self.name
            || arguments != &self.arguments
            || resolve(ctx, &self.prepare, arguments).await? != self.facts
        {
            return Err(rejected(
                "tool execution facts changed after preparation; request new approval",
            ));
        }
        Ok(())
    }
}

pub(super) async fn validate_handler(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    registry: Option<&VmValue>,
    name: &str,
    arguments: &Value,
) -> Result<(), VmError> {
    let bound = INVOCATION.try_with(Clone::clone).ok().flatten();
    match bound {
        Some(bound) => {
            bound
                .validate(
                    ctx.ok_or_else(|| rejected("prepared tool requires a VM context"))?,
                    name,
                    arguments,
                )
                .await
        }
        None if super::agent_tools::find_tool_closure(registry, name, "prepare").is_some() => {
            Err(rejected("tool requires preparation before consent"))
        }
        None => Ok(()),
    }
}

pub(super) async fn scope<F: Future>(
    invocation: Option<Arc<PreparedInvocation>>,
    future: F,
) -> F::Output {
    INVOCATION.scope(invocation, future).await
}

pub(crate) fn current_binding() -> VmValue {
    INVOCATION
        .try_with(|invocation| {
            invocation
                .as_ref()
                .map(|bound| crate::stdlib::json_to_vm_value(&bound.facts))
        })
        .ok()
        .flatten()
        .unwrap_or(VmValue::Nil)
}

#[cfg(all(test, unix))]
#[path = "agent_tool_preparation_tests.rs"]
mod tests;
