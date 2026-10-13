//! Pure path argument shape shared by dispatch and approval capture.

use super::ToolAnnotations;
use serde_json::Value;

pub(crate) const CONVENTIONAL_PATH_PARAMETERS: &[&str] = &[
    "path",
    "file",
    "target",
    "source_path",
    "new_path",
    "target_path",
    "paths",
];

pub(crate) fn parameters(annotations: Option<&ToolAnnotations>) -> Vec<String> {
    annotations
        .map(|annotations| annotations.arg_schema.path_params.clone())
        .unwrap_or_else(|| {
            CONVENTIONAL_PATH_PARAMETERS
                .iter()
                .map(|name| (*name).into())
                .collect()
        })
}

pub(crate) fn validate(arguments: &Value, parameters: &[String]) -> Result<(), String> {
    for parameter in parameters {
        if parameter.trim().is_empty() {
            return Err(
                "tool_annotations.arg_schema.path_params must contain nonempty field names".into(),
            );
        }
        match arguments.get(parameter) {
            None => {}
            Some(Value::String(value)) if !value.trim().is_empty() => {}
            Some(Value::Array(values))
                if values
                    .iter()
                    .all(|value| value.as_str().is_some_and(|value| !value.trim().is_empty())) => {}
            _ => {
                return Err(format!(
                    "workspace path argument '{parameter}' must be a string or list of strings"
                ))
            }
        }
    }
    Ok(())
}

/// Callers supply resolved annotations. This layer owns input shape, not
/// execution policy or permission state.
pub(crate) fn validate_arguments(
    arguments: &Value,
    annotations: Option<&ToolAnnotations>,
) -> Result<(), String> {
    validate(arguments, &parameters(annotations))
}
