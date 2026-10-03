//! Project runtime and authored errors onto the connector client contract.

use crate::llm::vm_value_to_json;
use crate::value::{ErrorCategory, VmError, VmValue};
use crate::{ClientError, ConnectorError};

pub(super) fn vm_error_to_connector(error: VmError) -> ConnectorError {
    ConnectorError::HarnRuntime(vm_error_message(error))
}

pub(super) fn vm_error_to_connector_for_export(export: &str, error: VmError) -> ConnectorError {
    match &error {
        VmError::CategorizedError {
            category: ErrorCategory::ToolRejected,
            message,
        } => ConnectorError::HarnRuntime(format!(
            "connector export '{export}' violated effect policy: {message}"
        )),
        _ => vm_error_to_connector(error),
    }
}

pub(super) fn connector_error_to_client(error: ConnectorError) -> ClientError {
    match error {
        ConnectorError::HarnRuntime(message) => client_error_from_message(message),
        other => ClientError::Other(other.to_string()),
    }
}

fn client_error_from_message(message: String) -> ClientError {
    if let Some(detail) = message.strip_prefix("method_not_found:") {
        return ClientError::MethodNotFound(detail.trim().to_string());
    }
    if let Some(detail) = message.strip_prefix("invalid_args:") {
        return ClientError::InvalidArgs(detail.trim().to_string());
    }
    if let Some(detail) = message.strip_prefix("rate_limited:") {
        return ClientError::RateLimited(detail.trim().to_string());
    }
    ClientError::Other(message)
}

fn vm_error_message(error: VmError) -> String {
    match error {
        VmError::Thrown(VmValue::String(message))
        | VmError::DeclaredThrown(VmValue::String(message)) => message.to_string(),
        VmError::Thrown(value) | VmError::DeclaredThrown(value) => {
            vm_value_to_json(&value).to_string()
        }
        other => other.to_string(),
    }
}
