use crate::bridge::json_result_to_vm_value;
use crate::external_agent::{delegate_external_agent, ExternalAgentDelegationRequest};
use crate::llm::vm_value_to_json;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

pub(crate) fn register_external_agent_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&EXTERNAL_AGENT_DELEGATE_IMPL_DEF];

#[harn_builtin(
    sig = "__external_agent_delegate(task: string, options: dict) -> dict",
    kind = "async",
    category = "agents"
)]
async fn external_agent_delegate_impl(
    _ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let task = required_string_arg(&args, 0, "__external_agent_delegate", "task")?;
    let options = match args.get(1) {
        Some(VmValue::Dict(_)) => vm_value_to_json(&args[1]),
        Some(value) if !matches!(value, VmValue::Nil) => {
            return Err(thrown("__external_agent_delegate: options must be a dict"));
        }
        _ => serde_json::Value::Object(Default::default()),
    };
    let mut request: ExternalAgentDelegationRequest =
        serde_json::from_value(options).map_err(|error| {
            thrown(format!(
                "__external_agent_delegate: invalid options: {error}"
            ))
        })?;
    request.task = task;
    let (_cancel_tx, mut cancel_rx) = tokio::sync::broadcast::channel(1);
    let envelope = delegate_external_agent(request, &mut cancel_rx)
        .await
        .map_err(|error| thrown(format!("__external_agent_delegate: {error}")))?;
    let value = serde_json::to_value(envelope)
        .map_err(|error| VmError::Runtime(format!("external agent encode error: {error}")))?;
    Ok(json_result_to_vm_value(&value))
}

fn required_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    label: &str,
) -> Result<String, VmError> {
    let value = args.get(index).map(VmValue::display).unwrap_or_default();
    if value.trim().is_empty() {
        return Err(thrown(format!("{builtin}: {label} is required")));
    }
    Ok(value)
}

fn thrown(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message.into())))
}
