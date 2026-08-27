use crate::bridge::json_result_to_vm_value;
use crate::external_agent::{
    delegate_external_agent, ExternalAgentDelegationRequest, ExternalAgentError,
};
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
    exposure = "harness.agent.external_agent_delegate",
    effects = ["worker.mutate@dynamic"],
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
            return Err(invalid_request(
                "__external_agent_delegate: options must be a dict",
            ));
        }
        _ => serde_json::Value::Object(Default::default()),
    };
    let mut request: ExternalAgentDelegationRequest =
        serde_json::from_value(options).map_err(|error| {
            invalid_request(format!(
                "__external_agent_delegate: invalid options: {error}"
            ))
        })?;
    request.task = task;
    let (_cancel_tx, mut cancel_rx) = tokio::sync::broadcast::channel(1);
    let envelope = delegate_external_agent(request, &mut cancel_rx)
        .await
        .map_err(external_agent_error_to_vm)?;
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
        return Err(invalid_request(format!("{builtin}: {label} is required")));
    }
    Ok(value)
}

fn invalid_request(message: impl Into<String>) -> VmError {
    external_agent_error_to_vm(ExternalAgentError::InvalidRequest(message.into()))
}

fn external_agent_error_to_vm(error: ExternalAgentError) -> VmError {
    use crate::value::ErrorCategory;

    let (kind, category, message) = match error {
        ExternalAgentError::InvalidRequest(message) => {
            ("invalid_request", ErrorCategory::InvalidRequest, message)
        }
        ExternalAgentError::Discovery(message) => {
            let category = crate::value::classify_error_message(&message);
            let category = if category == ErrorCategory::Generic {
                ErrorCategory::ToolError
            } else {
                category
            };
            ("discovery", category, message)
        }
        ExternalAgentError::Denied(message) => ("denied", ErrorCategory::ToolRejected, message),
        ExternalAgentError::Timeout(message) => ("timeout", ErrorCategory::Timeout, message),
        ExternalAgentError::Cancelled(message) => ("cancelled", ErrorCategory::Cancelled, message),
        ExternalAgentError::Transport(message) => {
            let category = crate::value::classify_error_message(&message);
            let category = if category == ErrorCategory::Generic {
                ErrorCategory::TransientNetwork
            } else {
                category
            };
            ("transport", category, message)
        }
        ExternalAgentError::Protocol(message) => ("protocol", ErrorCategory::ToolError, message),
    };
    VmError::Thrown(json_result_to_vm_value(&serde_json::json!({
        "error": "external_agent_error",
        "kind": kind,
        "category": category.as_str(),
        "message": message,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::ErrorCategory;

    #[test]
    fn external_agent_errors_preserve_kind_and_runtime_category() {
        let cases = [
            (
                ExternalAgentError::InvalidRequest("invalid".into()),
                "invalid_request",
                ErrorCategory::InvalidRequest,
                "invalid",
            ),
            (
                ExternalAgentError::Discovery("A2A agent card missing protocolVersion".into()),
                "discovery",
                ErrorCategory::ToolError,
                "A2A agent card missing protocolVersion",
            ),
            (
                ExternalAgentError::Denied("remote agent rejected task".into()),
                "denied",
                ErrorCategory::ToolRejected,
                "remote agent rejected task",
            ),
            (
                ExternalAgentError::Timeout("request timed out".into()),
                "timeout",
                ErrorCategory::Timeout,
                "request timed out",
            ),
            (
                ExternalAgentError::Cancelled("request cancelled".into()),
                "cancelled",
                ErrorCategory::Cancelled,
                "request cancelled",
            ),
            (
                ExternalAgentError::Transport("connection dropped".into()),
                "transport",
                ErrorCategory::TransientNetwork,
                "connection dropped",
            ),
            (
                ExternalAgentError::Protocol("invalid response".into()),
                "protocol",
                ErrorCategory::ToolError,
                "invalid response",
            ),
        ];

        for (error, expected_kind, expected_category, expected_message) in cases {
            let vm_error = external_agent_error_to_vm(error);
            let VmError::Thrown(VmValue::Dict(fields)) = &vm_error else {
                panic!("expected structured external-agent error, got {vm_error:?}");
            };
            assert_eq!(
                fields.get("error").map(VmValue::display).as_deref(),
                Some("external_agent_error")
            );
            assert_eq!(
                fields.get("kind").map(VmValue::display).as_deref(),
                Some(expected_kind)
            );
            assert_eq!(
                fields.get("message").map(VmValue::display).as_deref(),
                Some(expected_message)
            );
            assert_eq!(
                crate::value::error_to_category(&vm_error),
                expected_category
            );
        }
    }
}
