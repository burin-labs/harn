//! Canonical, transport-neutral projection of executable Harn tool registries.
//!
//! A `tool_registry` is the semantic owner. MCP, generated command trees, and
//! static schema consumers all read this normalized projection so naming,
//! schemas, presentation metadata, and validation cannot drift by adapter.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::mcp_server::convert::annotations_to_json;
use crate::value::{VmClosure, VmError, VmValue};

pub const TOOL_CATALOG_SCHEMA_VERSION: &str = "harn-tools/1.0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolRegistryInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Deterministic command-line presentation for one tool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolCliSpec {
    /// Non-empty command path below `harn tool run <script>`.
    pub command: Vec<String>,
    /// Hide the command from help while retaining explicit invocation.
    pub hidden: bool,
}

/// Origin binding retained for diagnostics and generated projections.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolSource {
    /// Stable source vocabulary such as `openapi` or `harn`.
    pub kind: String,
    /// Source-local operation identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Protocol-specific binding data. The typed outer record prevents an
    /// unlabelled metadata bag while allowing integration protocols to retain
    /// their native coordinates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<JsonValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSideEffectLevel {
    None,
    ReadOnly,
    WorkspaceWrite,
    ProcessExec,
    Network,
    DesktopControl,
}

/// Harn-owned execution classification, separate from advisory MCP hints.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ToolPolicy {
    pub kind: ToolPolicyKind,
    pub side_effect_level: ToolSideEffectLevel,
}

/// One normalized tool entry shared by every presentation adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCatalogEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<JsonValue>,
    pub cli: ToolCliSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub defer_loading: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ToolSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<ToolPolicy>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<JsonValue>,
}

/// Versioned, serializable projection of a tool registry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolCatalog {
    pub schema_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<ToolRegistryInfo>,
    pub tools: Vec<ToolCatalogEntry>,
}

/// Normalized catalog entry paired with its one executable Harn handler.
pub struct ExecutableTool {
    pub catalog: ToolCatalogEntry,
    pub handler: VmClosure,
}

/// Normalize a registry into the versioned transport-neutral catalog.
pub fn tool_registry_catalog(registry: &VmValue) -> Result<ToolCatalog, VmError> {
    let registry = registry_dict(registry)?;
    let entries = registry_entries(registry)?;
    let mut tools = Vec::with_capacity(entries.len());
    let mut names = BTreeSet::new();
    let mut command_owners = BTreeMap::<Vec<String>, String>::new();
    for entry in entries {
        let catalog = catalog_entry(entry)?;
        if !names.insert(catalog.name.clone()) {
            return Err(VmError::Runtime(format!(
                "tool registry contains duplicate tool name {:?}",
                catalog.name
            )));
        }
        if let Some(previous) =
            command_owners.insert(catalog.cli.command.clone(), catalog.name.clone())
        {
            return Err(VmError::Runtime(format!(
                "tool registry CLI command '{}' is ambiguous: tools {previous:?} and {:?} claim it",
                catalog.cli.command.join(" "),
                catalog.name,
            )));
        }
        tools.push(catalog);
    }
    Ok(ToolCatalog {
        schema_version: TOOL_CATALOG_SCHEMA_VERSION,
        info: registry_info(registry)?,
        tools,
    })
}

/// Normalize a registry and require one local Harn closure per tool.
pub fn executable_tools(registry: &VmValue) -> Result<Vec<ExecutableTool>, VmError> {
    let registry_dict = registry_dict(registry)?;
    let entries = registry_entries(registry_dict)?;
    let catalog = tool_registry_catalog(registry)?;
    entries
        .iter()
        .zip(catalog.tools)
        .map(|(entry, catalog)| {
            let entry = match entry {
                VmValue::Dict(entry) => entry,
                _ => {
                    return Err(VmError::Runtime(
                        "tool registry entries must be objects".into(),
                    ));
                }
            };
            let handler = match entry.get("handler") {
                Some(VmValue::Closure(handler)) => handler.as_ref().clone(),
                _ => {
                    return Err(VmError::Runtime(format!(
                        "tool registry entry {:?} has no local Harn handler closure",
                        catalog.name
                    )));
                }
            };
            Ok(ExecutableTool { catalog, handler })
        })
        .collect()
}

/// Convert a handler result to portable JSON without stringifying unsupported
/// runtime-only values such as closures or capability handles.
pub fn result_to_json(value: &VmValue) -> Result<JsonValue, String> {
    crate::llm::helpers::vm_value_to_json_strict(value, "result")
}

/// Validate one definition as it enters a registry. Cross-entry invariants are
/// checked once when the registry is published or projected.
pub(crate) fn validate_tool_entry(entry: &VmValue) -> Result<(), VmError> {
    catalog_entry(entry).map(|_| ())
}

fn registry_dict(registry: &VmValue) -> Result<&crate::value::DictMap, VmError> {
    let dict = match registry {
        VmValue::Dict(dict) => dict,
        _ => return Err(VmError::Runtime("expected a tool registry".into())),
    };
    match dict.get("_type") {
        Some(VmValue::String(kind)) if kind.as_str() == "tool_registry" => {}
        _ => {
            return Err(VmError::Runtime(
                "expected a tool registry created with tool_registry()".into(),
            ));
        }
    }
    Ok(dict)
}

fn registry_entries(registry: &crate::value::DictMap) -> Result<&[VmValue], VmError> {
    match registry.get("tools") {
        Some(VmValue::List(tools)) => Ok(tools),
        _ => Err(VmError::Runtime(
            "tool registry field 'tools' must be a list".into(),
        )),
    }
}

fn registry_info(registry: &crate::value::DictMap) -> Result<Option<ToolRegistryInfo>, VmError> {
    let Some(value) = registry.get("info") else {
        return Ok(None);
    };
    let fields = match value {
        VmValue::Dict(fields) => fields,
        _ => {
            return Err(VmError::Runtime(
                "tool registry info must be an object".into(),
            ))
        }
    };
    for key in fields.keys() {
        if !matches!(key.as_str(), "name" | "version" | "description") {
            return Err(VmError::Runtime(format!(
                "tool registry info contains unknown key {key:?}"
            )));
        }
    }
    let name = required_string(fields, "name", "tool registry info")?;
    let version = optional_string(fields, "version", "tool registry info")?;
    let description = optional_string(fields, "description", "tool registry info")?;
    Ok(Some(ToolRegistryInfo {
        name,
        version,
        description,
    }))
}

fn catalog_entry(entry: &VmValue) -> Result<ToolCatalogEntry, VmError> {
    let entry = match entry {
        VmValue::Dict(entry) => entry,
        _ => {
            return Err(VmError::Runtime(
                "tool registry entries must be objects".into(),
            ))
        }
    };
    let name = required_string(entry, "name", "tool registry entry")?;
    let description = optional_description(entry, &format!("tool {name:?}"))?;
    let title = optional_string(entry, "title", &format!("tool {name:?}"))?;
    let namespace = optional_string(entry, "namespace", &format!("tool {name:?}"))?;
    let defer_loading =
        optional_bool(entry, "defer_loading", &format!("tool {name:?}"))?.unwrap_or(false);
    let input_schema = params_to_json_schema(entry.get("parameters"))?;
    let output_schema = optional_object(entry, "outputSchema", &format!("tool {name:?}"))?;
    validate_json_schema(&input_schema, &format!("tool {name:?} input schema"))?;
    if let Some(output_schema) = output_schema.as_ref() {
        validate_json_schema(output_schema, &format!("tool {name:?} output schema"))?;
    }
    let annotations = entry.get("annotations").and_then(annotations_to_json);
    let icons = optional_array(entry, "icons", &format!("tool {name:?}"))?;
    let execution = optional_object(entry, "execution", &format!("tool {name:?}"))?;
    let meta = optional_object(entry, "meta", &format!("tool {name:?}"))?;
    let cli = cli_spec(entry.get("cli"), &name, namespace.as_deref())?;
    let source = source_spec(entry.get("source"), &name)?;
    let policy = policy_spec(
        entry.get("execution_policy"),
        entry.get("annotations"),
        &name,
    )?;
    Ok(ToolCatalogEntry {
        name,
        title,
        description,
        input_schema,
        output_schema,
        annotations,
        icons,
        execution,
        cli,
        namespace,
        defer_loading,
        source,
        policy,
        meta,
    })
}

fn cli_spec(
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
        return checked_cli_spec(default_command, false, name);
    };
    if matches!(value, VmValue::Nil) {
        return checked_cli_spec(default_command, false, name);
    }
    let fields = match value {
        VmValue::Dict(fields) => fields,
        _ => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'cli' must be an object"
            )))
        }
    };
    let allowed = BTreeSet::from(["command", "hidden"]);
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
                VmValue::String(part) if valid_command_part(part) => Ok(part.to_string()),
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
    let hidden =
        optional_bool(fields, "hidden", &format!("tool {name:?} field 'cli'"))?.unwrap_or(false);
    checked_cli_spec(command, hidden, name)
}

fn checked_cli_spec(
    command: Vec<String>,
    hidden: bool,
    name: &str,
) -> Result<ToolCliSpec, VmError> {
    if command.is_empty() || command.iter().any(|part| !valid_command_part(part)) {
        return Err(VmError::Runtime(format!(
            "tool {name:?} CLI command must contain only non-empty portable command names"
        )));
    }
    Ok(ToolCliSpec { command, hidden })
}

fn source_spec(value: Option<&VmValue>, name: &str) -> Result<Option<ToolSource>, VmError> {
    let Some(value) = value else { return Ok(None) };
    if matches!(value, VmValue::Nil) {
        return Ok(None);
    }
    let fields = match value {
        VmValue::Dict(fields) => fields,
        _ => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'source' must be an object"
            )))
        }
    };
    let allowed = BTreeSet::from(["kind", "id", "binding"]);
    for key in fields.keys() {
        if !allowed.contains(key.as_str()) {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'source' contains unknown key {key:?}"
            )));
        }
    }
    let kind = required_string(fields, "kind", &format!("tool {name:?} field 'source'"))?;
    let id = optional_string(fields, "id", &format!("tool {name:?} field 'source'"))?;
    let binding = optional_object(fields, "binding", &format!("tool {name:?} field 'source'"))?;
    Ok(Some(ToolSource { kind, id, binding }))
}

fn policy_spec(
    value: Option<&VmValue>,
    legacy_annotations: Option<&VmValue>,
    name: &str,
) -> Result<Option<ToolPolicy>, VmError> {
    if let Some(value) = value.filter(|value| !matches!(value, VmValue::Nil)) {
        let fields = match value {
            VmValue::Dict(fields) => fields,
            _ => {
                return Err(VmError::Runtime(format!(
                    "tool {name:?} field 'execution_policy' must be an object"
                )));
            }
        };
        let allowed = BTreeSet::from(["kind", "side_effect_level"]);
        for key in fields.keys() {
            if !allowed.contains(key.as_str()) {
                return Err(VmError::Runtime(format!(
                    "tool {name:?} field 'execution_policy' contains unknown key {key:?}"
                )));
            }
        }
        let kind = required_string(
            fields,
            "kind",
            &format!("tool {name:?} field 'execution_policy'"),
        )?;
        let side_effect_level = required_string(
            fields,
            "side_effect_level",
            &format!("tool {name:?} field 'execution_policy'"),
        )?;
        let Some(kind) = policy_kind(&kind) else {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'execution_policy.kind' has unknown value {kind:?}"
            )));
        };
        let Some(side_effect_level) = parse_side_effect_level(&side_effect_level) else {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'execution_policy.side_effect_level' has unknown value {side_effect_level:?}"
            )));
        };
        return Ok(Some(ToolPolicy {
            kind,
            side_effect_level,
        }));
    }

    // Compatibility projection for registries authored before `policy` became
    // a separate typed field. MCP filtering still excludes these Harn-only
    // keys, while the static catalog retains them for policy-aware consumers.
    let Some(VmValue::Dict(annotations)) = legacy_annotations else {
        return Ok(None);
    };
    let Some(VmValue::String(kind)) = annotations.get("kind") else {
        return Ok(None);
    };
    let Some(VmValue::String(side_effect_level)) = annotations.get("side_effect_level") else {
        return Ok(None);
    };
    Ok(policy_kind(kind)
        .zip(parse_side_effect_level(side_effect_level))
        .map(|(kind, side_effect_level)| ToolPolicy {
            kind,
            side_effect_level,
        }))
}

fn policy_kind(value: &str) -> Option<ToolPolicyKind> {
    Some(match value {
        "read" => ToolPolicyKind::Read,
        "edit" => ToolPolicyKind::Edit,
        "delete" => ToolPolicyKind::Delete,
        "move" => ToolPolicyKind::Move,
        "search" => ToolPolicyKind::Search,
        "execute" => ToolPolicyKind::Execute,
        "think" => ToolPolicyKind::Think,
        "fetch" => ToolPolicyKind::Fetch,
        "other" => ToolPolicyKind::Other,
        _ => return None,
    })
}

fn parse_side_effect_level(value: &str) -> Option<ToolSideEffectLevel> {
    Some(match value {
        "none" => ToolSideEffectLevel::None,
        "read_only" => ToolSideEffectLevel::ReadOnly,
        "workspace_write" => ToolSideEffectLevel::WorkspaceWrite,
        "process_exec" => ToolSideEffectLevel::ProcessExec,
        "network" => ToolSideEffectLevel::Network,
        "desktop_control" => ToolSideEffectLevel::DesktopControl,
        _ => return None,
    })
}

fn valid_command_part(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn required_string(
    fields: &crate::value::DictMap,
    key: &str,
    owner: &str,
) -> Result<String, VmError> {
    optional_string(fields, key, owner)?.ok_or_else(|| {
        VmError::Runtime(format!("{owner} field {key:?} must be a non-empty string"))
    })
}

fn optional_string(
    fields: &crate::value::DictMap,
    key: &str,
    owner: &str,
) -> Result<Option<String>, VmError> {
    match fields.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) if !value.is_empty() => Ok(Some(value.to_string())),
        _ => Err(VmError::Runtime(format!(
            "{owner} field {key:?} must be a non-empty string"
        ))),
    }
}

fn optional_description(
    fields: &crate::value::DictMap,
    owner: &str,
) -> Result<Option<String>, VmError> {
    match fields.get("description") {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) if value.is_empty() => Ok(None),
        Some(VmValue::String(value)) => Ok(Some(value.to_string())),
        _ => Err(VmError::Runtime(format!(
            "{owner} field \"description\" must be a string"
        ))),
    }
}

fn optional_bool(
    fields: &crate::value::DictMap,
    key: &str,
    owner: &str,
) -> Result<Option<bool>, VmError> {
    match fields.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        _ => Err(VmError::Runtime(format!(
            "{owner} field {key:?} must be a bool"
        ))),
    }
}

fn optional_object(
    fields: &crate::value::DictMap,
    key: &str,
    owner: &str,
) -> Result<Option<JsonValue>, VmError> {
    match fields.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value @ VmValue::Dict(_)) => Ok(Some(portable_json(value, owner)?)),
        _ => Err(VmError::Runtime(format!(
            "{owner} field {key:?} must be an object"
        ))),
    }
}

fn optional_array(
    fields: &crate::value::DictMap,
    key: &str,
    owner: &str,
) -> Result<Option<JsonValue>, VmError> {
    match fields.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value @ VmValue::List(_)) => Ok(Some(portable_json(value, owner)?)),
        _ => Err(VmError::Runtime(format!(
            "{owner} field {key:?} must be a list"
        ))),
    }
}

/// Convert Harn parameter definitions into the canonical JSON object schema.
pub fn params_to_json_schema(params: Option<&VmValue>) -> Result<JsonValue, VmError> {
    let params = match params {
        Some(VmValue::Dict(params)) => params,
        _ => return Ok(serde_json::json!({"type": "object", "properties": {}})),
    };
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, definition) in params.iter() {
        let property = match definition {
            VmValue::Dict(definition) => {
                let mut property = match definition.get("schema") {
                    Some(schema @ VmValue::Dict(_)) => {
                        portable_json(schema, "tool parameter schema")?
                            .as_object()
                            .cloned()
                            .unwrap_or_default()
                    }
                    _ => {
                        let mut fields = serde_json::Map::new();
                        for (key, value) in definition.iter() {
                            if key.as_str() != "required" {
                                if key.as_str() == "type" {
                                    if let VmValue::String(kind) = value {
                                        if let Some(kind) = json_schema_type(kind.as_str()) {
                                            fields.insert(
                                                key.to_string(),
                                                JsonValue::String(kind.to_string()),
                                            );
                                        }
                                        continue;
                                    }
                                }
                                fields.insert(
                                    key.to_string(),
                                    portable_json(value, "tool parameter definition")?,
                                );
                            }
                        }
                        fields
                    }
                };
                if matches!(definition.get("required"), Some(VmValue::Bool(true))) {
                    required.push(JsonValue::String(name.to_string()));
                }
                if let Some(VmValue::String(description)) = definition.get("description") {
                    property
                        .entry("description")
                        .or_insert_with(|| JsonValue::String(description.to_string()));
                }
                JsonValue::Object(property)
            }
            VmValue::String(kind) => json_schema_type(kind.as_str())
                .map(|kind| serde_json::json!({"type": kind}))
                .unwrap_or_else(|| JsonValue::Object(serde_json::Map::new())),
            _ => JsonValue::Object(serde_json::Map::new()),
        };
        properties.insert(name.to_string(), property);
    }
    required.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    let mut schema = serde_json::json!({"type": "object", "properties": properties});
    if !required.is_empty() {
        schema["required"] = JsonValue::Array(required);
    }
    Ok(schema)
}

fn portable_json(value: &VmValue, owner: &str) -> Result<JsonValue, VmError> {
    crate::llm::helpers::vm_value_to_json_strict(value, owner)
        .map_err(|error| VmError::Runtime(format!("{owner} is not portable JSON: {error}")))
}

fn validate_json_schema(schema: &JsonValue, owner: &str) -> Result<(), VmError> {
    jsonschema::draft202012::new(schema)
        .map(|_| ())
        .map_err(|error| VmError::Runtime(format!("{owner} is invalid: {error}")))
}

fn json_schema_type(kind: &str) -> Option<&str> {
    Some(match kind {
        "any" | "unknown" => return None,
        "int" => "integer",
        "float" => "number",
        "bool" => "boolean",
        "list" => "array",
        "dict" => "object",
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::DictMap;

    fn string(value: &str) -> VmValue {
        VmValue::String(value.into())
    }

    fn registry(entries: Vec<VmValue>) -> VmValue {
        let mut registry = DictMap::new();
        registry.insert("_type".into(), string("tool_registry"));
        registry.insert("tools".into(), VmValue::List(entries.into()));
        VmValue::dict(registry)
    }

    fn entry(name: &str, cli: Option<VmValue>) -> VmValue {
        let mut entry = DictMap::new();
        entry.insert("name".into(), string(name));
        entry.insert("description".into(), string("Test tool"));
        entry.insert("parameters".into(), VmValue::dict(DictMap::new()));
        if let Some(cli) = cli {
            entry.insert("cli".into(), cli);
        }
        VmValue::dict(entry)
    }

    #[test]
    fn defaults_command_to_namespace_and_name() {
        let mut tool = match entry("get_widget", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        tool.insert("namespace".into(), string("widgets"));
        let catalog = tool_registry_catalog(&registry(vec![VmValue::dict(tool)])).unwrap();
        assert_eq!(catalog.tools[0].cli.command, ["widgets", "get_widget"]);
    }

    #[test]
    fn dotted_identity_defaults_to_a_nested_command_path() {
        let catalog = tool_registry_catalog(&registry(vec![entry(
            "harn.code.search_examples",
            None,
        )]))
        .unwrap();
        assert_eq!(
            catalog.tools[0].cli.command,
            ["harn", "code", "search_examples"]
        );
    }

    #[test]
    fn treats_an_empty_description_as_absent() {
        let mut tool = match entry("undocumented", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        tool.insert("description".into(), string(""));
        let catalog = tool_registry_catalog(&registry(vec![VmValue::dict(tool)])).unwrap();
        assert_eq!(catalog.tools[0].description, None);
    }

    #[test]
    fn lowers_unconstrained_harn_parameter_types_to_valid_json_schema() {
        let mut definition = DictMap::new();
        definition.insert("type".into(), string("any"));
        definition.insert("description".into(), string("Optional input"));
        let mut parameters = DictMap::new();
        parameters.insert("input".into(), VmValue::dict(definition));
        parameters.insert("context".into(), string("unknown"));
        let schema = params_to_json_schema(Some(&VmValue::dict(parameters))).unwrap();
        assert_eq!(
            schema["properties"]["input"],
            serde_json::json!({"description": "Optional input"})
        );
        assert_eq!(schema["properties"]["context"], serde_json::json!({}));
        validate_json_schema(&schema, "test schema").unwrap();
    }

    #[test]
    fn rejects_duplicate_command_paths() {
        let command = || {
            let mut cli = DictMap::new();
            cli.insert(
                "command".into(),
                VmValue::List(vec![string("widgets"), string("get")].into()),
            );
            VmValue::dict(cli)
        };
        let error = tool_registry_catalog(&registry(vec![
            entry("first", Some(command())),
            entry("second", Some(command())),
        ]))
        .unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn rejects_duplicate_tool_names_even_with_distinct_commands() {
        let command = |part: &str| {
            let mut cli = DictMap::new();
            cli.insert("command".into(), VmValue::List(vec![string(part)].into()));
            VmValue::dict(cli)
        };
        let error = tool_registry_catalog(&registry(vec![
            entry("duplicate", Some(command("first"))),
            entry("duplicate", Some(command("second"))),
        ]))
        .unwrap_err();
        assert!(error.to_string().contains("duplicate tool name"));
    }

    #[test]
    fn rejects_unknown_cli_keys() {
        let mut cli = DictMap::new();
        cli.insert("commnad".into(), string("typo"));
        let error =
            tool_registry_catalog(&registry(vec![entry("broken", Some(VmValue::dict(cli)))]))
                .unwrap_err();
        assert!(error.to_string().contains("unknown key"));
    }

    #[test]
    fn preserves_typed_policy_outside_mcp_annotations() {
        let mut tool = match entry("fetch_widget", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        let mut policy = DictMap::new();
        policy.insert("kind".into(), string("fetch"));
        policy.insert("side_effect_level".into(), string("network"));
        tool.insert("execution_policy".into(), VmValue::dict(policy));
        let catalog = tool_registry_catalog(&registry(vec![VmValue::dict(tool)])).unwrap();
        assert_eq!(
            catalog.tools[0].policy,
            Some(ToolPolicy {
                kind: ToolPolicyKind::Fetch,
                side_effect_level: ToolSideEffectLevel::Network,
            })
        );
        assert!(catalog.tools[0].annotations.is_none());
    }

    #[test]
    fn rejects_runtime_only_values_in_static_metadata() {
        let mut tool = match entry("inspect", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        let mut meta = DictMap::new();
        meta.insert("opaque".into(), VmValue::BuiltinRef("println".into()));
        tool.insert("meta".into(), VmValue::dict(meta));
        let error = tool_registry_catalog(&registry(vec![VmValue::dict(tool)])).unwrap_err();
        assert!(error.to_string().contains("not portable JSON"));
    }

    #[test]
    fn leaves_existing_runtime_policy_objects_out_of_execution_classification() {
        let mut tool = match entry("search", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        let mut runtime_policy = DictMap::new();
        runtime_policy.insert("kind".into(), string("search"));
        runtime_policy.insert(
            "path_params".into(),
            VmValue::List(vec![string("path")].into()),
        );
        tool.insert("policy".into(), VmValue::dict(runtime_policy));
        let catalog = tool_registry_catalog(&registry(vec![VmValue::dict(tool)])).unwrap();
        assert_eq!(catalog.tools[0].policy, None);
    }

    #[test]
    fn rejects_invalid_json_schema_at_the_registry_boundary() {
        let mut tool = match entry("inspect", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        let mut schema = DictMap::new();
        schema.insert("type".into(), string("not-a-json-schema-type"));
        let mut parameter = DictMap::new();
        parameter.insert("schema".into(), VmValue::dict(schema));
        let mut parameters = DictMap::new();
        parameters.insert("value".into(), VmValue::dict(parameter));
        tool.insert("parameters".into(), VmValue::dict(parameters));
        let error = tool_registry_catalog(&registry(vec![VmValue::dict(tool)])).unwrap_err();
        assert!(error.to_string().contains("input schema is invalid"));
    }
}
