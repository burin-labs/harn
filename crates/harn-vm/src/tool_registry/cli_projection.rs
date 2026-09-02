//! Live `ToolRegistry` CLI metadata normalization.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    is_valid_cli_command_component, optional_bool, result_to_json, ToolCliArgumentSpec, ToolCliSpec,
};
use crate::value::{VmError, VmValue};

pub(super) fn cli_spec(
    value: Option<&VmValue>,
    name: &str,
    namespace: Option<&str>,
) -> Result<ToolCliSpec, VmError> {
    let default_command = namespace
        .into_iter()
        .chain(std::iter::once(name))
        .flat_map(|part| part.split('.'))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let Some(value) = value else {
        return checked_cli_spec(default_command, Vec::new(), false, BTreeMap::new(), name);
    };
    if matches!(value, VmValue::Nil) {
        return checked_cli_spec(default_command, Vec::new(), false, BTreeMap::new(), name);
    }
    let fields = match value {
        VmValue::Dict(fields) => fields,
        _ => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'cli' must be an object"
            )))
        }
    };
    let allowed = BTreeSet::from(["aliases", "arguments", "command", "hidden"]);
    for key in fields.keys() {
        if !allowed.contains(key.as_str()) {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'cli' contains unknown key {key:?}"
            )));
        }
    }
    let command = match fields.get("command") {
        None | Some(VmValue::Nil) => default_command,
        Some(VmValue::List(parts)) if !parts.is_empty() => parts
            .iter()
            .map(|part| match part {
                VmValue::String(part) if is_valid_cli_command_component(part) => {
                    Ok(part.to_string())
                }
                _ => Err(VmError::Runtime(format!(
                    "tool {name:?} field 'cli.command' must contain only non-empty command names"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'cli.command' must be a non-empty list of command names"
            )));
        }
    };
    let aliases = match fields.get("aliases") {
        None | Some(VmValue::Nil) => Vec::new(),
        Some(VmValue::List(aliases)) => aliases
            .iter()
            .map(|alias| match alias {
                VmValue::String(alias) if is_valid_cli_command_component(alias) => {
                    Ok(alias.to_string())
                }
                _ => Err(VmError::Runtime(format!(
                    "tool {name:?} field 'cli.aliases' must contain only portable command names"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'cli.aliases' must be a list of portable command names"
            )));
        }
    };
    let hidden =
        optional_bool(fields, "hidden", &format!("tool {name:?} field 'cli'"))?.unwrap_or(false);
    let arguments = match fields.get("arguments") {
        None | Some(VmValue::Nil) => BTreeMap::new(),
        Some(arguments @ VmValue::Dict(_)) => {
            let json = result_to_json(arguments).map_err(|error| {
                VmError::Runtime(format!(
                    "tool {name:?} field 'cli.arguments' is not portable JSON: {error}"
                ))
            })?;
            serde_json::from_value(json).map_err(|error| {
                VmError::Runtime(format!(
                    "tool {name:?} field 'cli.arguments' is invalid: {error}"
                ))
            })?
        }
        _ => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'cli.arguments' must be an object keyed by input property"
            )));
        }
    };
    checked_cli_spec(command, aliases, hidden, arguments, name)
}

fn checked_cli_spec(
    command: Vec<String>,
    aliases: Vec<String>,
    hidden: bool,
    arguments: BTreeMap<String, ToolCliArgumentSpec>,
    name: &str,
) -> Result<ToolCliSpec, VmError> {
    if command.is_empty()
        || command
            .iter()
            .any(|part| !is_valid_cli_command_component(part))
    {
        return Err(VmError::Runtime(format!(
            "tool {name:?} CLI command must contain only non-empty portable command names"
        )));
    }
    Ok(ToolCliSpec {
        command,
        aliases,
        hidden,
        arguments,
    })
}
