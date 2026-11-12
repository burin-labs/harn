//! Pure path argument shape shared by dispatch and approval capture.

use super::ToolAnnotations;
use harn_vm_core::workspace_path::expand_home_path;
use serde_json::Value;
use std::path::Path;

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

/// Spell every home-relative path argument (`~`, `~/...`) out in full, by
/// [`expand_home_path`]. Dispatch runs this before consent, policy and the
/// handler, so all three see one absolute path, which policy then admits or
/// refuses like any other.
pub(crate) fn expand_home(arguments: &mut Value, parameters: &[String], home: Option<&Path>) {
    let Value::Object(map) = arguments else {
        return;
    };
    let expand = |value: &mut String| {
        if let Some(expanded) = expand_home_path(value, home) {
            *value = expanded;
        }
    };
    for parameter in parameters {
        match map.get_mut(parameter) {
            Some(Value::String(value)) => expand(value),
            Some(Value::Array(values)) => {
                for value in values {
                    if let Value::String(value) = value {
                        expand(value);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn home_spellings_expand_and_others_stay() {
        let home = Path::new("/home/someone");
        let mut args = json!({
            "path": "~/projects/app/README.md",
            "paths": ["~", "src/lib.rs", "~other/x", "/abs/~/y"],
            "content": "~/not-a-path-param",
        });
        expand_home(&mut args, &["path".into(), "paths".into()], Some(home));
        assert_eq!(args["path"], "/home/someone/projects/app/README.md");
        assert_eq!(
            args["paths"],
            json!(["/home/someone", "src/lib.rs", "~other/x", "/abs/~/y"])
        );
        assert_eq!(args["content"], "~/not-a-path-param");

        let mut unknown_home = json!({"path": "~/x"});
        expand_home(&mut unknown_home, &["path".into()], None);
        assert_eq!(unknown_home["path"], "~/x");
    }
}
