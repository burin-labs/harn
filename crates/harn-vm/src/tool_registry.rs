//! Canonical, transport-neutral projection of executable Harn tool registries.
//!
//! A `tool_registry` is the semantic owner. MCP, generated command trees, and
//! static schema consumers all read this normalized projection so naming,
//! schemas, presentation metadata, and validation cannot drift by adapter.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as JsonValue;

use crate::tool_annotations::{SideEffectLevel, ToolKind};
use crate::value::{VmClosure, VmDictExt, VmError, VmValue};

mod cli_projection;
mod contract;
mod invocation;
pub use contract::*;
pub use invocation::{
    application_error_cli_envelope, application_error_mcp_result, classify_tool_failure,
    classify_tool_result, tool_runtime_error_summary, ToolFailureClassification,
    ToolInvocationError, ToolInvocationOutcome,
};

use cli_projection::cli_spec;

/// Retain the original executable entries and registry metadata while
/// projecting one adapter audience. This is the closure-preserving companion
/// to [`tool_registry_catalog_for_audience`]: runtime consumers can establish
/// one trusted exposure boundary without rebuilding handlers from the static
/// catalog.
pub fn project_tools_for_audience(
    tools: &VmValue,
    audience: ToolAudience,
) -> Result<VmValue, VmError> {
    let (entries, wrapper) = match tools {
        VmValue::List(entries) => ((**entries).clone(), None),
        VmValue::Dict(wrapper) => match wrapper.get("tools") {
            Some(VmValue::List(entries)) => ((**entries).clone(), Some(wrapper)),
            _ => {
                return Err(VmError::Runtime(
                    "tool projection requires a tool registry or list of tool definitions".into(),
                ))
            }
        },
        _ => {
            return Err(VmError::Runtime(
                "tool projection requires a tool registry or list of tool definitions".into(),
            ))
        }
    };

    // Normalize even legacy `{tools: [...]}` wrappers and bare lists through
    // the canonical registry parser. The projected value preserves the
    // caller's original outer shape and the executable closure objects.
    let mut validation_registry = wrapper
        .map(|wrapper| (**wrapper).clone())
        .unwrap_or_default();
    validation_registry.put_str("_type", "tool_registry");
    validation_registry.insert(
        crate::value::intern_key("tools"),
        VmValue::List(std::sync::Arc::new(entries.clone())),
    );
    let catalog = tool_registry_catalog(&VmValue::dict(validation_registry))?;
    let projected = entries
        .into_iter()
        .zip(catalog.tools)
        .filter_map(|(entry, catalog)| catalog.governance.allows(audience).then_some(entry))
        .collect::<Vec<_>>();

    if let Some(wrapper) = wrapper {
        let mut projected_registry = (**wrapper).clone();
        projected_registry.insert(
            crate::value::intern_key("tools"),
            VmValue::List(std::sync::Arc::new(projected)),
        );
        Ok(VmValue::dict(projected_registry))
    } else {
        Ok(VmValue::List(std::sync::Arc::new(projected)))
    }
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
    for entry in entries {
        let catalog = catalog_entry(entry)?;
        if !names.insert(catalog.name.clone()) {
            return Err(VmError::Runtime(format!(
                "tool registry contains duplicate tool name {:?}",
                catalog.name
            )));
        }
        tools.push(catalog);
    }
    let catalog = ToolCatalog {
        schema_version: ToolCatalogSchemaVersion::V2,
        info: registry_info(registry)?,
        cli: registry_cli(registry)?,
        tools,
        components: registry_components(registry)?,
    };
    catalog
        .validate()
        .map_err(|error| VmError::Runtime(format!("invalid tool catalog: {error}")))?;
    Ok(catalog)
}

/// Normalize and retain only tools exposed to one adapter audience.
pub fn tool_registry_catalog_for_audience(
    registry: &VmValue,
    audience: ToolAudience,
) -> Result<ToolCatalog, VmError> {
    let mut catalog = tool_registry_catalog(registry)?;
    catalog
        .tools
        .retain(|tool| tool.governance.allows(audience));
    Ok(catalog)
}

/// Serialize the catalog adapter projection with optional shared schemas.
pub fn tool_registry_schema(registry: &VmValue) -> Result<JsonValue, VmError> {
    let catalog = tool_registry_catalog_for_audience(registry, ToolAudience::Catalog)?;
    catalog
        .validate()
        .map_err(|error| VmError::Runtime(format!("tool_schema: {error}")))?;
    serde_json::to_value(catalog).map_err(|error| VmError::Runtime(format!("tool_schema: {error}")))
}

/// Normalize a registry and require one local Harn closure per tool.
pub fn executable_tools(registry: &VmValue) -> Result<Vec<ExecutableTool>, VmError> {
    executable_tools_matching(registry, None)
}

/// Normalize executable tools, then require handlers only for entries exposed
/// to the requested adapter. An excluded alternate-executor entry must not
/// prevent an adapter from loading the tools it can actually invoke.
pub fn executable_tools_for_audience(
    registry: &VmValue,
    audience: ToolAudience,
) -> Result<Vec<ExecutableTool>, VmError> {
    executable_tools_matching(registry, Some(audience))
}

fn executable_tools_matching(
    registry: &VmValue,
    audience: Option<ToolAudience>,
) -> Result<Vec<ExecutableTool>, VmError> {
    let registry_dict = registry_dict(registry)?;
    let entries = registry_entries(registry_dict)?;
    let catalog = tool_registry_catalog(registry)?;
    let mut executable = Vec::with_capacity(entries.len());
    for (entry, catalog) in entries.iter().zip(catalog.tools) {
        if audience.is_some_and(|audience| !catalog.governance.allows(audience)) {
            continue;
        }
        let VmValue::Dict(entry) = entry else {
            return Err(VmError::Runtime(
                "tool registry entries must be objects".into(),
            ));
        };
        let Some(VmValue::Closure(handler)) = entry.get("handler") else {
            return Err(VmError::Runtime(format!(
                "tool registry entry {:?} has no local Harn handler closure",
                catalog.name
            )));
        };
        executable.push(ExecutableTool {
            catalog,
            handler: handler.as_ref().clone(),
        });
    }
    Ok(executable)
}

/// Convert a handler result to portable JSON without stringifying unsupported
/// runtime-only values such as closures or capability handles.
pub fn result_to_json(value: &VmValue) -> Result<JsonValue, String> {
    crate::llm::helpers::vm_value_to_export_json_strict(value, "result")
}

/// Validate one definition as it enters a registry. Cross-entry invariants are
/// checked once when the registry is published or projected.
pub(crate) fn validate_tool_entry(entry: &VmValue) -> Result<(), VmError> {
    catalog_entry(entry).map(|_| ())
}

/// Read one entry's normalized audience decision. Registry construction and
/// adapter publication perform the full structural validation; model-facing
/// discovery and dispatch also use this parser so malformed raw registries
/// fail closed instead of bypassing governance.
pub(crate) fn tool_entry_allows_audience(
    entry: &crate::value::DictMap,
    audience: ToolAudience,
) -> Result<bool, VmError> {
    let name = entry
        .get("name")
        .map(VmValue::display)
        .unwrap_or_else(|| "<unnamed>".to_string());
    governance_spec(entry.get("governance"), &name).map(|policy| policy.allows(audience))
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

fn registry_components(
    registry: &crate::value::DictMap,
) -> Result<Option<ToolCatalogComponents>, VmError> {
    let Some(value) = registry.get("components") else {
        return Ok(None);
    };
    let VmValue::Dict(components) = value else {
        return Err(VmError::Runtime(
            "tool registry components must be an object with a schemas field".into(),
        ));
    };
    let Some(VmValue::Dict(schemas)) = components.get("schemas") else {
        return Err(VmError::Runtime(
            "tool registry components.schemas must be an object of named JSON Schemas".into(),
        ));
    };
    let schemas = result_to_json(&VmValue::Dict(schemas.clone())).map_err(|error| {
        VmError::Runtime(format!(
            "tool registry components.schemas are not portable JSON: {error}"
        ))
    })?;
    Ok(Some(ToolCatalogComponents {
        schemas: schemas
            .as_object()
            .expect("dict converts to object")
            .iter()
            .map(|(name, schema)| (name.clone(), schema.clone()))
            .collect(),
    }))
}

fn registry_cli(
    registry: &crate::value::DictMap,
) -> Result<Option<crate::tool_registry::ToolCliTreeSpec>, VmError> {
    let Some(value) = registry.get("cli") else {
        return Ok(None);
    };
    let json = result_to_json(value).map_err(|error| {
        VmError::Runtime(format!(
            "tool registry CLI metadata is not portable JSON: {error}"
        ))
    })?;
    serde_json::from_value(json).map(Some).map_err(|error| {
        VmError::Runtime(format!("tool registry CLI metadata is invalid: {error}"))
    })
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
    let input_schema = match entry.get("inputSchema") {
        Some(schema @ VmValue::Dict(_)) => portable_json(schema, &format!("tool {name:?}"))?,
        Some(_) => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field \"inputSchema\" must be an object-root JSON Schema"
            )))
        }
        None => params_to_json_schema(entry.get("parameters"))?,
    };
    let output_schema = optional_schema(entry, "outputSchema", &format!("tool {name:?}"))?;
    let error_schema = optional_schema(entry, "errorSchema", &format!("tool {name:?}"))?;
    validate_json_schema(&input_schema, &format!("tool {name:?} input schema"))?;
    if let Some(output_schema) = output_schema.as_ref() {
        validate_json_schema(output_schema, &format!("tool {name:?} output schema"))?;
    }
    if let Some(error_schema) = error_schema.as_ref() {
        validate_json_schema(error_schema, &format!("tool {name:?} error schema"))?;
    }
    let annotations = presentation_annotations(entry.get("annotations"), &name)?;
    let icons = icons_spec(entry.get("icons"), &name)?;
    let execution = execution_spec(entry.get("execution"), &name)?;
    let meta = open_record(entry.get("meta"), &format!("tool {name:?} field 'meta'"))?;
    let governance = governance_spec(entry.get("governance"), &name)?;
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
        error_schema,
        annotations,
        icons,
        execution,
        governance,
        cli,
        namespace,
        defer_loading,
        source,
        policy,
        meta,
    })
}

fn governance_spec(value: Option<&VmValue>, name: &str) -> Result<ToolGovernance, VmError> {
    let Some(value) = value.filter(|value| !matches!(value, VmValue::Nil)) else {
        return Ok(ToolGovernance::default());
    };
    let fields = match value {
        VmValue::Dict(fields) => fields,
        _ => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'governance' must be an object"
            )));
        }
    };
    for key in fields.keys() {
        if key.as_str() != "audiences" {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'governance' contains unknown key {key:?}"
            )));
        }
    }
    let values = match fields.get("audiences") {
        Some(VmValue::List(values)) if !values.is_empty() => values,
        _ => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'governance.audiences' must be a non-empty list"
            )));
        }
    };
    let mut audiences = BTreeSet::new();
    for value in values.iter() {
        let VmValue::String(value) = value else {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'governance.audiences' must contain only audience names"
            )));
        };
        let Some(audience) = ToolAudience::parse(value) else {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'governance.audiences' has unknown audience {value:?}"
            )));
        };
        if !audiences.insert(audience) {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'governance.audiences' contains duplicate audience {value:?}"
            )));
        }
    }
    Ok(ToolGovernance {
        audiences: audiences.into_iter().collect(),
    })
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
    let binding = open_record(
        fields.get("binding"),
        &format!("tool {name:?} field 'source.binding'"),
    )?;
    Ok(Some(ToolSource { kind, id, binding }))
}

fn presentation_annotations(
    value: Option<&VmValue>,
    name: &str,
) -> Result<Option<ToolPresentationAnnotations>, VmError> {
    let Some(value) = value.filter(|value| !matches!(value, VmValue::Nil)) else {
        return Ok(None);
    };
    let VmValue::Dict(fields) = value else {
        return Err(VmError::Runtime(format!(
            "tool {name:?} field 'annotations' must be an object"
        )));
    };
    // Registry annotations are a broader Harn runtime record. Select only the
    // portable presentation fields here; lifecycle, rendering, artifact, and
    // other runtime annotations remain available to their owning subsystems
    // without leaking into the transport-neutral DTO. `policy_spec` separately
    // consumes the legacy `kind` and `side_effect_level` keys.
    let annotations = ToolPresentationAnnotations {
        title: optional_string(fields, "title", &format!("tool {name:?} annotations"))?,
        read_only_hint: optional_bool(
            fields,
            "readOnlyHint",
            &format!("tool {name:?} annotations"),
        )?,
        destructive_hint: optional_bool(
            fields,
            "destructiveHint",
            &format!("tool {name:?} annotations"),
        )?,
        idempotent_hint: optional_bool(
            fields,
            "idempotentHint",
            &format!("tool {name:?} annotations"),
        )?,
        open_world_hint: optional_bool(
            fields,
            "openWorldHint",
            &format!("tool {name:?} annotations"),
        )?,
    };
    Ok((!annotations.is_empty()).then_some(annotations))
}

fn icons_spec(value: Option<&VmValue>, name: &str) -> Result<Option<Vec<ToolIcon>>, VmError> {
    let Some(value) = value.filter(|value| !matches!(value, VmValue::Nil)) else {
        return Ok(None);
    };
    let VmValue::List(values) = value else {
        return Err(VmError::Runtime(format!(
            "tool {name:?} field 'icons' must be a non-empty list"
        )));
    };
    if values.is_empty() {
        return Err(VmError::Runtime(format!(
            "tool {name:?} field 'icons' must be a non-empty list"
        )));
    }
    values
        .iter()
        .map(|value| {
            let VmValue::Dict(fields) = value else {
                return Err(VmError::Runtime(format!(
                    "tool {name:?} icon must be an object"
                )));
            };
            let allowed = BTreeSet::from(["src", "mimeType", "sizes", "theme"]);
            for key in fields.keys() {
                if !allowed.contains(key.as_str()) {
                    return Err(VmError::Runtime(format!(
                        "tool {name:?} icon contains unknown key {key:?}"
                    )));
                }
            }
            let sizes = match fields.get("sizes") {
                None | Some(VmValue::Nil) => None,
                Some(VmValue::List(values)) if !values.is_empty() => Some(
                    values
                        .iter()
                        .map(|value| match value {
                            VmValue::String(value) if !value.is_empty() => Ok(value.to_string()),
                            _ => Err(VmError::Runtime(format!(
                                "tool {name:?} icon field 'sizes' must contain non-empty strings"
                            ))),
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                _ => {
                    return Err(VmError::Runtime(format!(
                        "tool {name:?} icon field 'sizes' must be a non-empty list"
                    )))
                }
            };
            let theme = match optional_string(fields, "theme", &format!("tool {name:?} icon"))?
                .as_deref()
            {
                None => None,
                Some("light") => Some(ToolIconTheme::Light),
                Some("dark") => Some(ToolIconTheme::Dark),
                Some(value) => {
                    return Err(VmError::Runtime(format!(
                        "tool {name:?} icon field 'theme' has unknown value {value:?}"
                    )))
                }
            };
            Ok(ToolIcon {
                src: required_string(fields, "src", &format!("tool {name:?} icon"))?,
                mime_type: optional_string(fields, "mimeType", &format!("tool {name:?} icon"))?,
                sizes,
                theme,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn execution_spec(value: Option<&VmValue>, name: &str) -> Result<Option<ToolExecution>, VmError> {
    let Some(value) = value.filter(|value| !matches!(value, VmValue::Nil)) else {
        return Ok(None);
    };
    let VmValue::Dict(fields) = value else {
        return Err(VmError::Runtime(format!(
            "tool {name:?} field 'execution' must be an object"
        )));
    };
    for key in fields.keys() {
        if key.as_str() != "taskSupport" {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'execution' contains unknown key {key:?}"
            )));
        }
    }
    let task_support = match required_string(
        fields,
        "taskSupport",
        &format!("tool {name:?} field 'execution'"),
    )?
    .as_str()
    {
        "forbidden" => ToolTaskSupport::Forbidden,
        "optional" => ToolTaskSupport::Optional,
        "required" => ToolTaskSupport::Required,
        value => {
            return Err(VmError::Runtime(format!(
                "tool {name:?} field 'execution.taskSupport' has unknown value {value:?}"
            )))
        }
    };
    Ok(Some(ToolExecution { task_support }))
}

fn open_record(
    value: Option<&VmValue>,
    owner: &str,
) -> Result<Option<BTreeMap<String, JsonValue>>, VmError> {
    let Some(value) = value.filter(|value| !matches!(value, VmValue::Nil)) else {
        return Ok(None);
    };
    let VmValue::Dict(_) = value else {
        return Err(VmError::Runtime(format!("{owner} must be an object")));
    };
    let json = portable_json(value, owner)?;
    Ok(Some(
        json.as_object()
            .expect("dict converts to object")
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    ))
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

fn policy_kind(value: &str) -> Option<ToolKind> {
    Some(match value {
        "read" => ToolKind::Read,
        "edit" => ToolKind::Edit,
        "delete" => ToolKind::Delete,
        "move" => ToolKind::Move,
        "search" => ToolKind::Search,
        "execute" => ToolKind::Execute,
        "think" => ToolKind::Think,
        "fetch" => ToolKind::Fetch,
        "other" => ToolKind::Other,
        _ => return None,
    })
}

fn parse_side_effect_level(value: &str) -> Option<SideEffectLevel> {
    Some(match value {
        "none" => SideEffectLevel::None,
        "read_only" => SideEffectLevel::ReadOnly,
        "workspace_write" => SideEffectLevel::WorkspaceWrite,
        "process_exec" => SideEffectLevel::ProcessExec,
        "network" => SideEffectLevel::Network,
        "desktop_control" => SideEffectLevel::DesktopControl,
        _ => return None,
    })
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

fn optional_schema(
    fields: &crate::value::DictMap,
    key: &str,
    owner: &str,
) -> Result<Option<JsonValue>, VmError> {
    match fields.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value @ (VmValue::Bool(_) | VmValue::Dict(_))) => {
            Ok(Some(portable_json(value, owner)?))
        }
        _ => Err(VmError::Runtime(format!(
            "{owner} field {key:?} must be a JSON Schema boolean or object"
        ))),
    }
}

/// Convert Harn parameter definitions into the canonical JSON object schema.
pub fn params_to_json_schema(params: Option<&VmValue>) -> Result<JsonValue, VmError> {
    let params = match params {
        None | Some(VmValue::Nil) => {
            return Ok(serde_json::json!({"type": "object", "properties": {}}));
        }
        Some(VmValue::Dict(params)) => params,
        Some(_) => {
            return Err(VmError::Runtime(
                "tool parameters must be a per-parameter definition object; use input_schema for a complete JSON Schema"
                    .to_string(),
            ));
        }
    };
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, definition) in params.iter() {
        let property = match definition {
            VmValue::Dict(definition) => {
                let mut property = match definition.get("schema") {
                    Some(schema @ (VmValue::Bool(_) | VmValue::Dict(_))) => {
                        portable_json(schema, "tool parameter schema")?
                    }
                    Some(_) => {
                        return Err(VmError::Runtime(format!(
                            "tool parameter {name:?} field \"schema\" must be a JSON Schema boolean or object"
                        )))
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
                        JsonValue::Object(fields)
                    }
                };
                if matches!(definition.get("required"), Some(VmValue::Bool(true))) {
                    required.push(JsonValue::String(name.to_string()));
                }
                if let Some(VmValue::String(description)) = definition.get("description") {
                    if let Some(property) = property.as_object_mut() {
                        property
                            .entry("description")
                            .or_insert_with(|| JsonValue::String(description.to_string()));
                    } else {
                        property = serde_json::json!({
                            "allOf": [property],
                            "description": description,
                        });
                    }
                }
                property
            }
            VmValue::String(kind) => json_schema_type(kind.as_str())
                .map(|kind| serde_json::json!({"type": kind}))
                .ok_or_else(|| {
                    VmError::Runtime(format!(
                        "tool parameter {name:?} has unknown shorthand type {kind:?}"
                    ))
                })?,
            _ => {
                return Err(VmError::Runtime(format!(
                    "tool parameter {name:?} must be a type string or definition object"
                )))
            }
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

    fn governed_entry(name: &str, audiences: &[&str]) -> VmValue {
        let mut tool = match entry(name, None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        let mut governance = DictMap::new();
        governance.insert(
            "audiences".into(),
            VmValue::List(
                audiences
                    .iter()
                    .map(|audience| string(audience))
                    .collect::<Vec<_>>()
                    .into(),
            ),
        );
        tool.insert("governance".into(), VmValue::dict(governance));
        VmValue::dict(tool)
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
        let catalog =
            tool_registry_catalog(&registry(vec![entry("harn.code.search_examples", None)]))
                .unwrap();
        assert_eq!(
            catalog.tools[0].cli.command,
            ["harn", "code", "search_examples"]
        );
    }

    #[test]
    fn registry_cli_commands_share_the_portable_component_contract() {
        fn cli(parts: &[&str]) -> VmValue {
            let mut cli = DictMap::new();
            cli.insert(
                "command".into(),
                VmValue::List(
                    parts
                        .iter()
                        .map(|part| string(part))
                        .collect::<Vec<_>>()
                        .into(),
                ),
            );
            VmValue::dict(cli)
        }

        let repeated = tool_registry_catalog(&registry(vec![entry(
            "nested",
            Some(cli(&["inspect", "inspect"])),
        )]))
        .expect("repeated command components form a valid nested path");
        assert_eq!(repeated.tools[0].cli.command, ["inspect", "inspect"]);

        for invalid in [vec!["-inspect"], vec!["inspect me"], Vec::new()] {
            let error =
                tool_registry_catalog(&registry(vec![entry("invalid", Some(cli(&invalid)))]))
                    .expect_err(
                        "the live registry must reject the generated schema's invalid paths",
                    );
            assert!(error.to_string().contains("cli.command"));
        }
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
    fn registry_preserves_boolean_output_and_error_schemas() {
        let mut tool = match entry("always_fails", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        tool.insert("outputSchema".into(), VmValue::Bool(false));
        tool.insert("errorSchema".into(), VmValue::Bool(true));
        let mut parameter = DictMap::new();
        parameter.insert("schema".into(), VmValue::Bool(false));
        parameter.insert("description".into(), string("No value is accepted."));
        let mut parameters = DictMap::new();
        parameters.insert("impossible".into(), VmValue::dict(parameter));
        tool.insert("parameters".into(), VmValue::dict(parameters));

        let catalog = tool_registry_catalog(&registry(vec![VmValue::dict(tool)]))
            .expect("Draft 2020-12 boolean schemas are portable");
        assert_eq!(
            catalog.tools[0].input_schema["properties"]["impossible"],
            serde_json::json!({
                "allOf": [false],
                "description": "No value is accepted."
            })
        );
        assert_eq!(catalog.tools[0].output_schema, Some(JsonValue::Bool(false)));
        assert_eq!(catalog.tools[0].error_schema, Some(JsonValue::Bool(true)));
    }

    #[test]
    fn registry_preserves_a_complete_object_root_input_schema() {
        let mut tool = match entry("lookup", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        tool.insert(
            "inputSchema".into(),
            crate::schema::json_to_vm_value(&serde_json::json!({
                "type": "object",
                "properties": {"id": {"type": "integer"}},
                "required": ["id"],
                "additionalProperties": false
            })),
        );

        let catalog = tool_registry_catalog(&registry(vec![VmValue::dict(tool)]))
            .expect("complete input schema is normalized without reinterpretation");
        assert_eq!(
            catalog.tools[0].input_schema["required"],
            serde_json::json!(["id"])
        );
        assert_eq!(catalog.tools[0].input_schema["additionalProperties"], false);
    }

    #[test]
    fn legacy_parameter_shorthand_rejects_shapes_it_cannot_interpret() {
        let bool_error = params_to_json_schema(Some(&VmValue::Bool(true)))
            .expect_err("a complete schema must use input_schema");
        assert!(bool_error.to_string().contains("use input_schema"));

        let invalid = VmValue::dict(DictMap::from_iter([(
            arcstr::ArcStr::from("id"),
            VmValue::String("unrecognized".into()),
        )]));
        let shorthand_error = params_to_json_schema(Some(&invalid))
            .expect_err("unknown shorthand must not erase to an open schema");
        assert!(shorthand_error
            .to_string()
            .contains("unknown shorthand type"));
    }

    #[test]
    fn defaults_governance_to_every_adapter_for_compatible_registries() {
        let catalog = tool_registry_catalog(&registry(vec![entry("inspect", None)])).unwrap();
        assert_eq!(
            catalog.tools[0].governance.audiences,
            [
                ToolAudience::Cli,
                ToolAudience::Mcp,
                ToolAudience::Catalog,
                ToolAudience::Dashboard,
                ToolAudience::Agent,
            ]
        );
    }

    #[test]
    fn registry_components_are_the_catalog_components_source_of_truth() {
        let mut registry = match registry(vec![entry("inspect", None)]) {
            VmValue::Dict(registry) => (*registry).clone(),
            _ => unreachable!(),
        };
        let mut schemas = DictMap::new();
        schemas.insert(
            "Receipt".into(),
            crate::schema::json_to_vm_value(&serde_json::json!({
                "type": "object",
                "required": ["ok"]
            })),
        );
        let mut components = DictMap::new();
        components.insert("schemas".into(), VmValue::dict(schemas));
        registry.insert("components".into(), VmValue::dict(components));

        let catalog = tool_registry_catalog(&VmValue::dict(registry)).expect("project catalog");
        assert_eq!(
            catalog.components.unwrap().schemas["Receipt"]["required"],
            serde_json::json!(["ok"])
        );
    }

    #[test]
    fn filters_each_projection_and_serializes_normalized_governance() {
        let registry = registry(vec![governed_entry(
            "operator_inspect",
            &["catalog", "cli"],
        )]);
        for audience in [ToolAudience::Cli, ToolAudience::Catalog] {
            let catalog = tool_registry_catalog_for_audience(&registry, audience).unwrap();
            assert_eq!(catalog.tools.len(), 1);
        }
        for audience in [
            ToolAudience::Mcp,
            ToolAudience::Dashboard,
            ToolAudience::Agent,
        ] {
            let catalog = tool_registry_catalog_for_audience(&registry, audience).unwrap();
            assert!(catalog.tools.is_empty());
        }
        let catalog = tool_registry_catalog_for_audience(&registry, ToolAudience::Catalog).unwrap();
        let serialized = serde_json::to_value(catalog).unwrap();
        assert_eq!(
            serialized["tools"][0]["governance"]["audiences"],
            serde_json::json!(["cli", "catalog"])
        );
    }

    #[test]
    fn rejects_empty_unknown_and_open_governance() {
        let mut missing_audiences = match entry("missing_audiences", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        missing_audiences.insert("governance".into(), VmValue::dict(DictMap::new()));
        let missing_audiences = validate_tool_entry(&VmValue::dict(missing_audiences)).unwrap_err();
        assert!(missing_audiences.to_string().contains("non-empty list"));

        let empty = validate_tool_entry(&governed_entry("empty", &[])).unwrap_err();
        assert!(empty.to_string().contains("non-empty list"));

        let unknown = validate_tool_entry(&governed_entry("unknown", &["desktop"])).unwrap_err();
        assert!(unknown.to_string().contains("unknown audience"));

        let mut tool = match governed_entry("open", &["cli"]) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        let mut governance = match tool.get("governance").unwrap() {
            VmValue::Dict(governance) => (**governance).clone(),
            _ => unreachable!(),
        };
        governance.insert("authorization".into(), string("admin"));
        tool.insert("governance".into(), VmValue::dict(governance));
        let open = validate_tool_entry(&VmValue::dict(tool)).unwrap_err();
        assert!(open.to_string().contains("unknown key"));
    }

    #[test]
    fn excluded_handlerless_tool_does_not_block_an_adapter_projection() {
        let mut remote_only = match governed_entry("remote_only", &["agent"]) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        remote_only.insert("executor".into(), string("provider_native"));
        let registry = registry(vec![VmValue::dict(remote_only)]);
        let tools = executable_tools_for_audience(&registry, ToolAudience::Cli).unwrap();
        assert!(tools.is_empty());

        let error = match executable_tools_for_audience(&registry, ToolAudience::Agent) {
            Err(error) => error,
            Ok(_) => panic!("agent projection must require the included local handler"),
        };
        assert!(error.to_string().contains("no local Harn handler closure"));
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
        assert!(error.to_string().contains("duplicate CLI command path"));
    }

    #[test]
    fn cli_path_conflicts_are_scoped_to_the_cli_audience() {
        let command = |parts: &[&str]| {
            let mut cli = DictMap::new();
            cli.insert(
                "command".into(),
                VmValue::List(
                    parts
                        .iter()
                        .map(|part| string(part))
                        .collect::<Vec<_>>()
                        .into(),
                ),
            );
            VmValue::dict(cli)
        };
        let mut mcp_parent = match governed_entry("mcp_parent", &["mcp"]) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        mcp_parent.insert("cli".into(), command(&["widgets"]));
        let mut cli_leaf = match governed_entry("cli_leaf", &["cli"]) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        cli_leaf.insert("cli".into(), command(&["widgets", "get"]));
        tool_registry_catalog(&registry(vec![
            VmValue::dict(mcp_parent),
            VmValue::dict(cli_leaf.clone()),
        ]))
        .expect("non-CLI tools do not occupy the CLI command tree");

        let mut cli_parent = match governed_entry("cli_parent", &["cli"]) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        cli_parent.insert("cli".into(), command(&["widgets"]));
        let error = tool_registry_catalog(&registry(vec![
            VmValue::dict(cli_parent),
            VmValue::dict(cli_leaf),
        ]))
        .expect_err("a CLI leaf cannot also be a parent command");
        assert!(error.to_string().contains("both a tool and a parent"));
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
                kind: ToolKind::Fetch,
                side_effect_level: SideEffectLevel::Network,
            })
        );
        assert!(catalog.tools[0].annotations.is_none());
    }

    #[test]
    fn filters_runtime_annotations_from_portable_presentation_metadata() {
        let mut tool = match entry("runtime_annotated", None) {
            VmValue::Dict(tool) => (*tool).clone(),
            _ => unreachable!(),
        };
        let mut annotations = DictMap::new();
        annotations.insert("readOnlyHint".into(), VmValue::Bool(true));
        annotations.insert("agent_lifecycle".into(), VmValue::Bool(true));
        annotations.insert("inline_result".into(), VmValue::Bool(true));
        annotations.insert("emits_artifacts".into(), VmValue::Bool(true));
        annotations.insert("kind".into(), string("execute"));
        annotations.insert("side_effect_level".into(), string("process_exec"));
        tool.insert("annotations".into(), VmValue::dict(annotations));

        let catalog = tool_registry_catalog(&registry(vec![VmValue::dict(tool)])).unwrap();
        assert_eq!(
            catalog.tools[0].annotations,
            Some(ToolPresentationAnnotations {
                read_only_hint: Some(true),
                ..ToolPresentationAnnotations::default()
            })
        );
        assert_eq!(
            catalog.tools[0].policy,
            Some(ToolPolicy {
                kind: ToolKind::Execute,
                side_effect_level: SideEffectLevel::ProcessExec,
            })
        );
        assert_eq!(
            serde_json::to_value(&catalog).unwrap()["tools"][0]["annotations"],
            serde_json::json!({"readOnlyHint": true})
        );
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
