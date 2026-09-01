//! Versioned, transport-neutral `harn-tools/1.0` data contract.
//!
//! Runtime closures and adapter configuration deliberately do not appear here.
//! OpenAPI and Harn exports normalize into this contract; CLI, MCP, docs, and
//! native clients project from it.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value as JsonValue};
use ts_rs::{Config, TS};

mod prepared;
mod schema;

use schema::{
    decode_json_pointer_segment, encode_json_pointer_segment, try_transform_schema_nodes,
    try_visit_schema_nodes,
};

pub use crate::mcp_tasks::McpTaskSupport as ToolTaskSupport;
use crate::tool_annotations::{SideEffectLevel, ToolKind};
pub use prepared::{
    PreparedToolCatalog, PreparedToolCatalogError, ToolContractPhase, ToolContractViolation,
};

pub const TOOL_CATALOG_SCHEMA_VERSION: &str = "harn-tools/1.0";
pub const TOOL_CATALOG_SCHEMA_ARTIFACT: &str = "schemas/harn-tools-v1.schema.json";
pub const TOOL_CATALOG_TYPESCRIPT_ARTIFACT: &str = "harn-tools.ts";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCatalogProjectionError {
    message: String,
}

impl std::fmt::Display for ToolCatalogProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ToolCatalogProjectionError {}

/// Closed version discriminator for the portable catalog.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub enum ToolCatalogSchemaVersion {
    #[default]
    #[serde(rename = "harn-tools/1.0")]
    V1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolRegistryInfo {
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub description: Option<String>,
}

/// Deterministic command-line presentation for one tool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCliSpec {
    /// Non-empty command path below `harn tool run <script>`.
    #[schemars(length(min = 1), inner(pattern(r"^[A-Za-z0-9_][A-Za-z0-9_-]*$")))]
    pub command: Vec<String>,
    /// Hide the command from help while retaining explicit invocation.
    pub hidden: bool,
}

/// Adapter projections that may discover and invoke a tool.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum ToolAudience {
    Cli,
    Mcp,
    Catalog,
    Dashboard,
    Agent,
}

impl ToolAudience {
    pub(crate) const ALL: [Self; 5] = [
        Self::Cli,
        Self::Mcp,
        Self::Catalog,
        Self::Dashboard,
        Self::Agent,
    ];

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "cli" => Self::Cli,
            "mcp" => Self::Mcp,
            "catalog" => Self::Catalog,
            "dashboard" => Self::Dashboard,
            "agent" => Self::Agent,
            _ => return None,
        })
    }
}

/// Closed adapter-exposure policy owned by one tool entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolGovernance {
    #[schemars(length(min = 1), extend("uniqueItems" = true))]
    pub audiences: Vec<ToolAudience>,
}

impl ToolGovernance {
    pub fn allows(&self, audience: ToolAudience) -> bool {
        self.audiences.contains(&audience)
    }
}

impl Default for ToolGovernance {
    fn default() -> Self {
        Self {
            audiences: ToolAudience::ALL.to_vec(),
        }
    }
}

/// Origin binding retained for diagnostics and generated projections.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolSource {
    /// Stable source vocabulary such as `openapi` or `harn`.
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub kind: String,
    /// Source-local operation identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub id: Option<String>,
    /// Protocol-specific coordinates. This and `_meta` are the only open
    /// metadata records in the contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable, type = "Readonly<Record<string, JsonValue>> | null")]
    pub binding: Option<BTreeMap<String, JsonValue>>,
}

/// MCP-compatible presentation hints. They are advisory, never policy.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolPresentationAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub idempotent_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub open_world_hint: Option<bool>,
}

impl ToolPresentationAnnotations {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// One icon advertised by a tool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolIcon {
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub src: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), inner(length(min = 1), pattern(r".*\S.*")), extend("uniqueItems" = true))]
    pub sizes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub theme: Option<ToolIconTheme>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ToolIconTheme {
    Light,
    Dark,
}

/// MCP task execution metadata. Runtime queues and retry policy stay outside
/// the portable tool contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolExecution {
    pub task_support: ToolTaskSupport,
}

/// Harn-owned execution classification, separate from advisory MCP hints.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolPolicy {
    pub kind: ToolKind,
    pub side_effect_level: SideEffectLevel,
}

/// Reusable named JSON Schemas carried by a catalog.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCatalogComponents {
    #[schemars(schema_with = "json_schema_document_map")]
    #[ts(type = "Readonly<Record<string, JsonSchema202012>>")]
    pub schemas: BTreeMap<String, JsonValue>,
}

/// One normalized tool entry shared by every presentation adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCatalogEntry {
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub description: Option<String>,
    #[schemars(schema_with = "object_root_schema_document")]
    #[ts(type = "JsonSchema202012")]
    pub input_schema: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable, type = "JsonSchema202012 | null")]
    #[schemars(schema_with = "json_schema_document")]
    pub output_schema: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub annotations: Option<ToolPresentationAnnotations>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), extend("uniqueItems" = true))]
    pub icons: Option<Vec<ToolIcon>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub execution: Option<ToolExecution>,
    pub governance: ToolGovernance,
    pub cli: ToolCliSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub namespace: Option<String>,
    pub defer_loading: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub source: Option<ToolSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub policy: Option<ToolPolicy>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable, type = "Readonly<Record<string, JsonValue>> | null")]
    pub meta: Option<BTreeMap<String, JsonValue>>,
}

/// Versioned, serializable projection of a tool registry or static exports.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCatalog {
    pub schema_version: ToolCatalogSchemaVersion,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub info: Option<ToolRegistryInfo>,
    pub tools: Vec<ToolCatalogEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub components: Option<ToolCatalogComponents>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToolCatalog {
    schema_version: ToolCatalogSchemaVersion,
    info: Option<ToolRegistryInfo>,
    tools: Vec<ToolCatalogEntry>,
    components: Option<ToolCatalogComponents>,
}

impl<'de> Deserialize<'de> for ToolCatalog {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawToolCatalog::deserialize(deserializer)?;
        let catalog = Self {
            schema_version: raw.schema_version,
            info: raw.info,
            tools: raw.tools,
            components: raw.components,
        };
        catalog.validate().map_err(serde::de::Error::custom)?;
        Ok(catalog)
    }
}

impl ToolCatalog {
    /// Validate semantic invariants that JSON Schema cannot express across
    /// entries. Every deserialized catalog passes through this one boundary.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(info) = self.info.as_ref() {
            require_non_empty(&info.name, "info.name")?;
            require_optional_non_empty(info.version.as_deref(), "info.version")?;
            require_optional_non_empty(info.description.as_deref(), "info.description")?;
        }

        let component_schemas = self
            .components
            .as_ref()
            .map(|components| &components.schemas);
        if let Some(components) = component_schemas {
            for (name, schema) in components {
                require_non_empty(name, "components.schemas key")?;
                validate_schema_document(schema, false, &format!("components.schemas.{name}"))?;
                validate_refs(
                    schema,
                    component_schemas,
                    &format!("components.schemas.{name}"),
                )?;
            }
        }

        let mut names = std::collections::BTreeSet::new();
        for (index, tool) in self.tools.iter().enumerate() {
            let context = format!("tools[{index}]");
            require_non_empty(&tool.name, &format!("{context}.name"))?;
            if !names.insert(tool.name.as_str()) {
                return Err(format!("duplicate tool name {:?}", tool.name));
            }
            require_optional_non_empty(tool.title.as_deref(), &format!("{context}.title"))?;
            require_optional_non_empty(
                tool.description.as_deref(),
                &format!("{context}.description"),
            )?;
            require_optional_non_empty(tool.namespace.as_deref(), &format!("{context}.namespace"))?;
            if tool.cli.command.is_empty() {
                return Err(format!("{context}.cli.command must not be empty"));
            }
            for (part_index, part) in tool.cli.command.iter().enumerate() {
                if !is_valid_cli_command_component(part) {
                    return Err(format!(
                        "{context}.cli.command[{part_index}] must match ^[A-Za-z0-9_][A-Za-z0-9_-]*$"
                    ));
                }
            }
            require_unique(
                &tool.governance.audiences,
                &format!("{context}.governance.audiences"),
            )?;
            if tool.governance.audiences.is_empty() {
                return Err(format!("{context}.governance.audiences must not be empty"));
            }
            if let Some(icons) = tool.icons.as_ref() {
                if icons.is_empty() {
                    return Err(format!("{context}.icons must not be empty"));
                }
                let mut seen_icons = std::collections::BTreeSet::new();
                for (icon_index, icon) in icons.iter().enumerate() {
                    let icon_context = format!("{context}.icons[{icon_index}]");
                    require_non_empty(&icon.src, &format!("{icon_context}.src"))?;
                    require_optional_non_empty(
                        icon.mime_type.as_deref(),
                        &format!("{icon_context}.mimeType"),
                    )?;
                    if let Some(sizes) = icon.sizes.as_ref() {
                        if sizes.is_empty() {
                            return Err(format!("{icon_context}.sizes must not be empty"));
                        }
                        for (size_index, size) in sizes.iter().enumerate() {
                            require_non_empty(
                                size,
                                &format!("{icon_context}.sizes[{size_index}]"),
                            )?;
                        }
                        require_unique(sizes, &format!("{icon_context}.sizes"))?;
                    }
                    let encoded = serde_json::to_string(icon)
                        .expect("tool icon contract is always serializable");
                    if !seen_icons.insert(encoded) {
                        return Err(format!("{context}.icons contains a duplicate icon"));
                    }
                }
            }
            if let Some(source) = tool.source.as_ref() {
                require_non_empty(&source.kind, &format!("{context}.source.kind"))?;
                require_optional_non_empty(source.id.as_deref(), &format!("{context}.source.id"))?;
            }
            validate_schema_document(&tool.input_schema, true, &format!("{context}.inputSchema"))?;
            validate_defs_do_not_shadow_components(
                &tool.input_schema,
                component_schemas,
                &format!("{context}.inputSchema"),
            )?;
            validate_refs(
                &tool.input_schema,
                component_schemas,
                &format!("{context}.inputSchema"),
            )?;
            if let Some(output_schema) = tool.output_schema.as_ref() {
                validate_schema_document(output_schema, false, &format!("{context}.outputSchema"))?;
                validate_defs_do_not_shadow_components(
                    output_schema,
                    component_schemas,
                    &format!("{context}.outputSchema"),
                )?;
                validate_refs(
                    output_schema,
                    component_schemas,
                    &format!("{context}.outputSchema"),
                )?;
            }
        }
        validate_cli_command_tree(&self.tools)?;
        Ok(())
    }

    /// Project a catalog entry onto MCP while carrying every reachable schema
    /// dependency into the standalone tool document.
    pub fn mcp_tool(
        &self,
        entry: &ToolCatalogEntry,
    ) -> Result<JsonValue, ToolCatalogProjectionError> {
        tool_catalog_entry_to_mcp_with_components(
            entry,
            self.components
                .as_ref()
                .map(|components| &components.schemas),
        )
    }

    pub fn mcp_tools(&self) -> Result<Vec<JsonValue>, ToolCatalogProjectionError> {
        self.tools
            .iter()
            .map(|entry| self.mcp_tool(entry))
            .collect()
    }

    /// Shape a tool result by the same standalone schema projection used by
    /// [`Self::mcp_tool`], including catalog component resolution.
    pub fn mcp_structured_content(
        &self,
        entry: &ToolCatalogEntry,
        result: JsonValue,
    ) -> Result<Option<JsonValue>, ToolCatalogProjectionError> {
        let Some(output_schema) = entry.output_schema.as_ref() else {
            return Ok(None);
        };
        let schema = standalone_schema(
            output_schema,
            self.components
                .as_ref()
                .map(|components| &components.schemas),
        )?;
        Ok(tool_output_to_mcp_structured_content(Some(&schema), result))
    }
}

fn validate_cli_command_tree(tools: &[ToolCatalogEntry]) -> Result<(), String> {
    let mut commands = BTreeMap::<&[String], &str>::new();
    for tool in tools
        .iter()
        .filter(|tool| tool.governance.allows(ToolAudience::Cli))
    {
        for (path, owner) in &commands {
            if *path == tool.cli.command {
                return Err(format!(
                    "duplicate CLI command path {:?}: tools {owner:?} and {:?} claim it",
                    tool.cli.command.join(" "),
                    tool.name
                ));
            }
            if path.starts_with(&tool.cli.command) || tool.cli.command.starts_with(path) {
                return Err(format!(
                    "CLI command path conflict: tools {owner:?} and {:?} make '{}' both a tool and a parent command",
                    tool.name,
                    if path.len() < tool.cli.command.len() {
                        path.join(" ")
                    } else {
                        tool.cli.command.join(" ")
                    }
                ));
            }
        }
        commands.insert(&tool.cli.command, &tool.name);
    }
    Ok(())
}

pub fn is_valid_cli_command_component(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn require_non_empty(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(())
    }
}

fn require_optional_non_empty(value: Option<&str>, field: &str) -> Result<(), String> {
    value.map_or(Ok(()), |value| require_non_empty(value, field))
}

fn require_unique<T: Ord>(values: &[T], field: &str) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(format!("{field} must not contain duplicates"));
        }
    }
    Ok(())
}

fn validate_schema_document(
    schema: &JsonValue,
    require_object: bool,
    field: &str,
) -> Result<(), String> {
    if !schema.is_boolean() && !schema.is_object() {
        return Err(format!("{field} must be a boolean or object JSON Schema"));
    }
    jsonschema::meta::validate(schema)
        .map_err(|error| format!("{field} is not a valid Draft 2020-12 JSON Schema: {error}"))?;
    if require_object && schema.get("type").and_then(JsonValue::as_str) != Some("object") {
        return Err(format!(
            "{field} must explicitly declare an object root with type: object"
        ));
    }
    Ok(())
}

const OFFLINE_SCHEMA_ROOT_URI: &str = "https://harn.invalid/tool-schema";
const OFFLINE_EXTERNAL_SCHEMA_URI: &str = "https://harn.invalid/external-schema";

fn validate_refs(
    value: &JsonValue,
    components: Option<&BTreeMap<String, JsonValue>>,
    field: &str,
) -> Result<(), String> {
    let component_uris = components
        .into_iter()
        .flat_map(|schemas| schemas.keys())
        .enumerate()
        .map(|(index, name)| {
            (
                name.clone(),
                format!("https://harn.invalid/tool-components/{index}"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let validation_schema = refs_for_offline_validation(value.clone(), &component_uris)?;
    let mut resources = components
        .into_iter()
        .flat_map(|schemas| schemas.iter())
        .map(|(name, schema)| {
            let uri = component_uris
                .get(name)
                .expect("every component has a synthetic validation URI")
                .clone();
            refs_for_offline_validation(schema.clone(), &component_uris).map(|schema| (uri, schema))
        })
        .collect::<Result<Vec<_>, _>>()?;
    resources.push((
        OFFLINE_EXTERNAL_SCHEMA_URI.to_string(),
        JsonValue::Bool(true),
    ));

    let registry = jsonschema::Registry::new()
        .draft(jsonschema::Draft::Draft202012)
        .extend(resources)
        .and_then(|builder| builder.prepare())
        .map_err(|error| format!("{field} contains invalid schema resources: {error}"))?;
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_registry(&registry)
        .with_base_uri(OFFLINE_SCHEMA_ROOT_URI)
        .build(&validation_schema)
        .map(|_| ())
        .map_err(|error| format!("{field} contains an unresolved schema reference: {error}"))
}

fn validate_defs_do_not_shadow_components(
    schema: &JsonValue,
    components: Option<&BTreeMap<String, JsonValue>>,
    field: &str,
) -> Result<(), String> {
    let Some(definitions) = schema.get("$defs").and_then(JsonValue::as_object) else {
        return Ok(());
    };
    let Some(components) = components else {
        return Ok(());
    };
    if let Some(name) = definitions
        .keys()
        .find(|name| components.contains_key(*name))
    {
        return Err(format!(
            "{field} $defs entry {name:?} conflicts with components.schemas"
        ));
    }
    Ok(())
}

fn refs_for_offline_validation(
    mut value: JsonValue,
    component_uris: &BTreeMap<String, String>,
) -> Result<JsonValue, String> {
    try_transform_schema_nodes(&mut value, &mut |object| {
        for keyword in ["$ref", "$dynamicRef"] {
            let Some(reference) = object.get(keyword).and_then(JsonValue::as_str) else {
                continue;
            };
            let resolved = reference_for_offline_validation(reference, component_uris)?;
            object.insert(keyword.to_string(), JsonValue::String(resolved));
        }
        Ok::<(), String>(())
    })?;
    Ok(value)
}

fn reference_for_offline_validation(
    reference: &str,
    component_uris: &BTreeMap<String, String>,
) -> Result<String, String> {
    if let Some(path) = reference.strip_prefix("#/components/schemas/") {
        let (encoded_name, nested_path) = path
            .split_once('/')
            .map_or((path, None), |(name, path)| (name, Some(path)));
        let name = decode_json_pointer_segment(encoded_name)
            .ok_or_else(|| format!("invalid schema component reference {reference:?}"))?;
        let uri = component_uris
            .get(&name)
            .ok_or_else(|| format!("dangling schema component reference {reference:?}"))?;
        return Ok(match nested_path {
            Some(path) => format!("{uri}#/{path}"),
            None => uri.clone(),
        });
    }
    if reference.starts_with('#') {
        Ok(reference.to_string())
    } else {
        // A portable catalog can name a resource supplied by its eventual host.
        // This offline pass proves only self-contained and catalog-component
        // references, so an unavailable external resource becomes `true`.
        Ok(OFFLINE_EXTERNAL_SCHEMA_URI.to_string())
    }
}

fn json_schema_document(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"oneOf": [{"type": "boolean"}, {"type": "object"}, {"type": "null"}]})
}

fn object_root_schema_document(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "properties": {"type": {"const": "object"}},
        "required": ["type"]
    })
}

fn json_schema_document_map(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "propertyNames": {"pattern": ".*\\S.*"},
        "additionalProperties": {"oneOf": [{"type": "boolean"}, {"type": "object"}]}
    })
}

/// Project one canonical catalog entry onto the MCP `Tool` wire shape.
pub fn tool_catalog_entry_to_mcp(entry: &ToolCatalogEntry) -> JsonValue {
    tool_catalog_entry_to_mcp_with_components(entry, None)
        .expect("component-free MCP projection is infallible")
}

fn tool_catalog_entry_to_mcp_with_components(
    entry: &ToolCatalogEntry,
    components: Option<&BTreeMap<String, JsonValue>>,
) -> Result<JsonValue, ToolCatalogProjectionError> {
    let title = entry
        .title
        .clone()
        .or_else(|| {
            entry
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.title.clone())
        })
        .unwrap_or_else(|| entry.name.clone());
    let description = entry
        .description
        .clone()
        .unwrap_or_else(|| format!("Exported Harn function '{}'.", entry.name));
    let mut tool = json!({
        "name": entry.name,
        "title": title,
        "description": description,
        "inputSchema": standalone_schema(&entry.input_schema, components)?,
    });
    if let Some(output_schema) = entry.output_schema.as_ref() {
        tool["outputSchema"] = mcp_output_schema(&standalone_schema(output_schema, components)?);
    }
    if let Some(annotations) = entry.annotations.as_ref() {
        let mut annotations = annotations.clone();
        annotations.title.get_or_insert(title);
        tool["annotations"] = serde_json::to_value(annotations)
            .expect("tool presentation annotations are serializable");
    } else {
        tool["annotations"] = json!({"title": title});
    }
    if let Some(icons) = entry.icons.as_ref() {
        tool["icons"] = serde_json::to_value(icons).expect("tool icons are serializable");
    }
    if let Some(execution) = entry.execution.as_ref() {
        tool["execution"] =
            serde_json::to_value(execution).expect("tool execution metadata is serializable");
    }
    if let Some(meta) = entry.meta.as_ref() {
        tool["_meta"] = serde_json::to_value(meta).expect("tool metadata is serializable");
    }
    Ok(tool)
}

fn standalone_schema(
    schema: &JsonValue,
    components: Option<&BTreeMap<String, JsonValue>>,
) -> Result<JsonValue, ToolCatalogProjectionError> {
    let Some(components) = components.filter(|components| !components.is_empty()) else {
        return Ok(schema.clone());
    };
    // Once a catalog carries components, MCP must emit a standalone schema.
    // Resource-scoped and external references cannot be moved beneath `$defs`
    // without changing their meaning, even when the entry has no ordinary
    // `#/components/schemas/*` reference for the closure walk to discover.
    reject_unsafe_bundled_schema_keywords(schema, "tool schema")?;
    let mut reachable = std::collections::BTreeSet::new();
    collect_component_refs(schema, &mut reachable);
    let mut pending = reachable.iter().cloned().collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        let Some(component) = components.get(&name) else {
            continue;
        };
        let mut dependencies = std::collections::BTreeSet::new();
        collect_component_refs(component, &mut dependencies);
        for dependency in dependencies {
            if reachable.insert(dependency.clone()) {
                pending.push(dependency);
            }
        }
    }
    if reachable.is_empty() {
        return Ok(schema.clone());
    }
    for name in &reachable {
        if let Some(component) = components.get(name) {
            reject_unsafe_bundled_schema_keywords(
                component,
                &format!("components.schemas.{name}"),
            )?;
        }
    }
    let mut schema = rewrite_schema_refs(schema.clone(), None);
    let JsonValue::Object(root) = &mut schema else {
        return Ok(schema);
    };
    let defs = reachable
        .iter()
        .filter_map(|name| {
            components.get(name).map(|schema| {
                let base = format!("#/$defs/{}", encode_json_pointer_segment(name));
                (
                    name.clone(),
                    rewrite_schema_refs(schema.clone(), Some(&base)),
                )
            })
        })
        .collect::<serde_json::Map<_, _>>();
    let root_defs = root
        .entry("$defs")
        .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
    let root_defs = root_defs
        .as_object_mut()
        .expect("validated schema $defs is an object");
    root_defs.extend(defs);
    Ok(schema)
}

fn collect_component_refs(value: &JsonValue, names: &mut std::collections::BTreeSet<String>) {
    try_visit_schema_nodes(value, &mut |object| {
        if let Some(path) = object
            .get("$ref")
            .and_then(JsonValue::as_str)
            .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
        {
            let encoded_name = path.split('/').next().unwrap_or(path);
            if let Some(name) = decode_json_pointer_segment(encoded_name) {
                names.insert(name);
            }
        }
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("infallible schema traversal");
}

fn reject_unsafe_bundled_schema_keywords(
    value: &JsonValue,
    field: &str,
) -> Result<(), ToolCatalogProjectionError> {
    try_visit_schema_nodes(value, &mut |object| {
        for keyword in ["$id", "$anchor", "$dynamicAnchor", "$dynamicRef"] {
            if object.contains_key(keyword) {
                return Err(ToolCatalogProjectionError {
                    message: format!(
                        "cannot project {field} through MCP components: {keyword} changes JSON Schema resource scope"
                    ),
                });
            }
        }
        if object
            .get("$ref")
            .and_then(JsonValue::as_str)
            .is_some_and(|reference| !reference.starts_with('#'))
        {
            return Err(ToolCatalogProjectionError {
                message: format!(
                    "cannot project {field} through MCP components: external $ref is not self-contained"
                ),
            });
        }
        Ok(())
    })
}

fn rewrite_schema_refs(mut value: JsonValue, component_base: Option<&str>) -> JsonValue {
    try_transform_schema_nodes(&mut value, &mut |object| {
        if let Some(JsonValue::String(reference)) = object.get_mut("$ref") {
            if let Some(path) = reference.strip_prefix("#/components/schemas/") {
                let (encoded_name, nested_path) = path
                    .split_once('/')
                    .map_or((path, None), |(name, path)| (name, Some(path)));
                if let Some(name) = decode_json_pointer_segment(encoded_name) {
                    let encoded_name = encode_json_pointer_segment(&name);
                    *reference = match nested_path {
                        Some(path) => format!("#/$defs/{encoded_name}/{path}"),
                        None => format!("#/$defs/{encoded_name}"),
                    };
                }
            } else if let Some(base) = component_base {
                if reference == "#" {
                    *reference = base.to_string();
                } else if let Some(path) = reference.strip_prefix("#/") {
                    *reference = format!("{base}/{path}");
                }
            }
        }
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("infallible schema traversal");
    value
}

pub fn tool_result_to_mcp_structured_content(
    entry: &ToolCatalogEntry,
    result: JsonValue,
) -> Option<JsonValue> {
    tool_output_to_mcp_structured_content(entry.output_schema.as_ref(), result)
}

pub fn tool_output_to_mcp_structured_content(
    output_schema: Option<&JsonValue>,
    result: JsonValue,
) -> Option<JsonValue> {
    output_schema.map(|output_schema| {
        if schema_guarantees_object(output_schema) {
            result
        } else {
            json!({"result": result})
        }
    })
}

fn mcp_output_schema(output_schema: &JsonValue) -> JsonValue {
    if schema_guarantees_object(output_schema) {
        return output_schema.clone();
    }
    json!({
        "type": "object",
        "properties": {"result": output_schema},
        "required": ["result"],
        "additionalProperties": false,
    })
}

fn schema_guarantees_object(schema: &JsonValue) -> bool {
    schema_node_guarantees_object(schema, schema, &mut std::collections::BTreeSet::new())
}

fn schema_node_guarantees_object<'a>(
    root: &'a JsonValue,
    schema: &'a JsonValue,
    visited_refs: &mut std::collections::BTreeSet<&'a str>,
) -> bool {
    if schema.get("type").and_then(JsonValue::as_str) == Some("object") {
        return true;
    }
    if let Some(reference) = schema.get("$ref").and_then(JsonValue::as_str) {
        if let Some(pointer) = reference.strip_prefix('#') {
            return visited_refs.insert(reference)
                && root.pointer(pointer).is_some_and(|target| {
                    schema_node_guarantees_object(root, target, visited_refs)
                });
        }
    }
    ["oneOf", "anyOf"].iter().any(|keyword| {
        schema
            .get(keyword)
            .and_then(JsonValue::as_array)
            .is_some_and(|branches| {
                !branches.is_empty()
                    && branches.iter().all(|branch| {
                        schema_node_guarantees_object(root, branch, &mut visited_refs.clone())
                    })
            })
    }) || schema
        .get("allOf")
        .and_then(JsonValue::as_array)
        .is_some_and(|branches| {
            branches.iter().any(|branch| {
                schema_node_guarantees_object(root, branch, &mut visited_refs.clone())
            })
        })
}

/// Draft 2020-12 schema generated by the owning Rust contract module.
pub fn tool_catalog_json_schema() -> JsonValue {
    let generator = schemars::generate::SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator();
    let mut schema = serde_json::to_value(generator.into_root_schema_for::<ToolCatalog>())
        .expect("tool catalog schema is serializable");
    schema["$id"] = json!("https://harnlang.com/schemas/harn-tools-v1.schema.json");
    schema["title"] = json!("Harn Tool Catalog");
    schema
}

/// Strict TypeScript projection generated beside the JSON Schema.
pub fn tool_catalog_typescript() -> String {
    let config = Config::default();
    let mut output = String::from(
        "// GENERATED by `harn dump-protocol-artifacts` from harn-vm::tool_registry::contract.\n\
         // Do not edit by hand.\n\n\
         export type JsonValue = null | boolean | number | string | readonly JsonValue[] | { readonly [key: string]: JsonValue };\n\
         export type JsonSchema202012 = boolean | { readonly [keyword: string]: JsonValue };\n\n",
    );
    macro_rules! declaration {
        ($type:ty) => {{
            output.push_str("export ");
            output.push_str(&<$type as TS>::decl(&config));
            output.push_str("\n\n");
        }};
    }
    declaration!(ToolCatalogSchemaVersion);
    declaration!(ToolAudience);
    declaration!(ToolKind);
    declaration!(SideEffectLevel);
    declaration!(ToolTaskSupport);
    declaration!(ToolIconTheme);
    declaration!(ToolRegistryInfo);
    declaration!(ToolCliSpec);
    declaration!(ToolGovernance);
    declaration!(ToolSource);
    declaration!(ToolPresentationAnnotations);
    declaration!(ToolIcon);
    declaration!(ToolExecution);
    declaration!(ToolPolicy);
    declaration!(ToolCatalogComponents);
    declaration!(ToolCatalogEntry);
    declaration!(ToolCatalog);
    let mut output = output
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    while output.ends_with('\n') {
        output.pop();
    }
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_catalog_json() -> JsonValue {
        json!({
          "schema_version": "harn-tools/1.0",
          "info": {
            "name": "inspection-suite",
            "version": "1.2.3",
            "description": "Typed inspection tools."
          },
          "tools": [{
            "name": "inspect",
            "title": "Inspect",
            "description": "Inspect one hypothesis.",
            "inputSchema": {"type": "object"},
            "outputSchema": {"$ref": "#/components/schemas/Result"},
            "governance": {"audiences": ["cli", "mcp", "catalog", "dashboard", "agent"]},
            "cli": {"command": ["inspect"], "hidden": false}, "deferLoading": false,
            "annotations": {
              "title": "Inspect a hypothesis",
              "readOnlyHint": true,
              "destructiveHint": false,
              "idempotentHint": true,
              "openWorldHint": false
            },
            "icons": [{"src": "data:image/svg+xml;base64,AA==", "mimeType": "image/svg+xml", "sizes": ["16x16"], "theme": "dark"}],
            "execution": {"taskSupport": "optional"},
            "namespace": "hypotheses",
            "policy": {"kind": "read", "side_effect_level": "read_only"},
            "source": {"kind": "harn", "id": "inspect", "binding": {"callableKind": "function"}},
            "_meta": {"vendor.example/version": 1}
          }],
          "components": {"schemas": {"Result": {"type": "object"}}}
        })
    }

    fn shared_invalid_catalogs() -> Vec<JsonValue> {
        let mut fixtures = Vec::new();
        let mut empty_name = valid_catalog_json();
        empty_name["tools"][0]["name"] = json!("");
        fixtures.push(empty_name);
        let mut scalar_input = valid_catalog_json();
        scalar_input["tools"][0]["inputSchema"] = json!(true);
        fixtures.push(scalar_input);
        let mut untyped_input = valid_catalog_json();
        untyped_input["tools"][0]["inputSchema"] = json!({});
        fixtures.push(untyped_input);
        let mut duplicate_audience = valid_catalog_json();
        duplicate_audience["tools"][0]["governance"]["audiences"] = json!(["mcp", "mcp"]);
        fixtures.push(duplicate_audience);
        let mut duplicate_size = valid_catalog_json();
        duplicate_size["tools"][0]["icons"][0]["sizes"] = json!(["16x16", "16x16"]);
        fixtures.push(duplicate_size);
        let mut scalar_component = valid_catalog_json();
        scalar_component["components"]["schemas"]["Result"] = json!(7);
        fixtures.push(scalar_component);
        let mut whitespace_info = valid_catalog_json();
        whitespace_info["info"]["name"] = json!("   ");
        fixtures.push(whitespace_info);
        let mut whitespace_icon = valid_catalog_json();
        whitespace_icon["tools"][0]["icons"][0]["src"] = json!("\t");
        fixtures.push(whitespace_icon);
        let mut whitespace_component = valid_catalog_json();
        let schema = whitespace_component["components"]["schemas"]
            .as_object_mut()
            .unwrap()
            .remove("Result")
            .unwrap();
        whitespace_component["components"]["schemas"][" "] = schema;
        whitespace_component["tools"][0]["outputSchema"] =
            json!({"$ref": "#/components/schemas/%20"});
        fixtures.push(whitespace_component);
        fixtures
    }

    #[test]
    fn schema_is_meta_valid_and_rejects_owned_shape_drift() {
        let schema = tool_catalog_json_schema();
        jsonschema::meta::validate(&schema).expect("harn-tools schema must be meta-valid");
        let validator = jsonschema::draft202012::new(&schema).expect("catalog schema");
        let valid = valid_catalog_json();
        assert!(validator.is_valid(&valid));
        assert!(serde_json::from_value::<ToolCatalog>(valid).is_ok());

        let mut omitted_output = valid_catalog_json();
        omitted_output["tools"][0]
            .as_object_mut()
            .unwrap()
            .remove("outputSchema");
        assert!(
            validator.is_valid(&omitted_output),
            "outputSchema is optional in the generated envelope schema"
        );
        assert!(serde_json::from_value::<ToolCatalog>(omitted_output).is_ok());

        let mut repeated_command = valid_catalog_json();
        repeated_command["tools"][0]["cli"]["command"] = json!(["inspect", "inspect"]);
        assert!(validator.is_valid(&repeated_command));
        assert!(serde_json::from_value::<ToolCatalog>(repeated_command).is_ok());

        for invalid in shared_invalid_catalogs().into_iter().chain([
            json!({"schema_version": "harn-tools/2.0", "tools": []}),
            json!({"schema_version": "harn-tools/1.0", "tools": [], "unknown": true}),
            json!({"schema_version": "harn-tools/1.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["shell"]},"cli":{"command":["x"],"hidden":false},"deferLoading":false}]}),
            json!({"schema_version": "harn-tools/1.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":["x"],"hidden":false},"deferLoading":false,"icons":[{"src":7}]}]}),
            json!({"schema_version": "harn-tools/1.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":["x"],"hidden":false},"deferLoading":false,"execution":{"taskSupport":"sometimes"}}]}),
            json!({"schema_version": "harn-tools/1.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":["-x"],"hidden":false},"deferLoading":false}]}),
            json!({"schema_version": "harn-tools/1.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":["x y"],"hidden":false},"deferLoading":false}]}),
            json!({"schema_version": "harn-tools/1.0", "tools": [{"name":"x","inputSchema":{"type":"object"},"governance":{"audiences":["cli"]},"cli":{"command":[],"hidden":false},"deferLoading":false}]}),
        ]) {
            assert!(!validator.is_valid(&invalid), "fixture unexpectedly valid: {invalid}");
            assert!(
                serde_json::from_value::<ToolCatalog>(invalid.clone()).is_err(),
                "Rust parser unexpectedly accepted: {invalid}"
            );
        }
    }

    #[test]
    fn structural_schema_defers_embedded_schema_semantics_to_harn() {
        let schema = tool_catalog_json_schema();
        let validator = jsonschema::draft202012::new(&schema).expect("catalog schema");
        let mut invalid_embedded_schema = valid_catalog_json();
        invalid_embedded_schema["tools"][0]["inputSchema"] = json!({"type": "object", "not": 7});
        assert!(
            validator.is_valid(&invalid_embedded_schema),
            "the portable artifact validates the envelope, not nested Draft semantics"
        );
        let error = serde_json::from_value::<ToolCatalog>(invalid_embedded_schema)
            .expect_err("Harn semantic validation must reject invalid embedded schemas");
        assert!(
            error
                .to_string()
                .contains("inputSchema is not a valid Draft 2020-12 JSON Schema"),
            "unexpected semantic validation error: {error}"
        );
    }

    #[test]
    fn serde_rejects_unknown_owned_fields_and_versions() {
        assert!(serde_json::from_value::<ToolCatalog>(json!({
            "schema_version": "harn-tools/1.0", "tools": [], "extra": true
        }))
        .is_err());
        assert!(serde_json::from_value::<ToolCatalog>(json!({
            "schema_version": "harn-tools/9.0", "tools": []
        }))
        .is_err());
    }

    #[test]
    fn semantic_validation_rejects_duplicate_names_paths_and_dangling_refs() {
        let mut duplicate_names = valid_catalog_json();
        let mut second = duplicate_names["tools"][0].clone();
        second["cli"]["command"] = json!(["other"]);
        duplicate_names["tools"]
            .as_array_mut()
            .unwrap()
            .push(second);
        assert!(serde_json::from_value::<ToolCatalog>(duplicate_names).is_err());

        let mut duplicate_paths = valid_catalog_json();
        let mut second = duplicate_paths["tools"][0].clone();
        second["name"] = json!("other");
        duplicate_paths["tools"]
            .as_array_mut()
            .unwrap()
            .push(second);
        assert!(serde_json::from_value::<ToolCatalog>(duplicate_paths).is_err());

        let mut dangling = valid_catalog_json();
        dangling["tools"][0]["outputSchema"] = json!({"$ref": "#/components/schemas/Missing"});
        assert!(serde_json::from_value::<ToolCatalog>(dangling).is_err());

        let mut external = valid_catalog_json();
        external["tools"][0]["outputSchema"] = json!({"$ref": "https://example.test/x"});
        let external: ToolCatalog =
            serde_json::from_value(external).expect("portable catalogs retain open JSON Schema");
        assert!(external.mcp_tool(&external.tools[0]).is_err());
    }

    #[test]
    fn semantic_validation_uses_draft_resource_and_anchor_resolution() {
        let mut anchored = valid_catalog_json();
        anchored["tools"][0]["outputSchema"] = json!({
            "$defs": {"result": {"$anchor": "node", "type": "string"}},
            "$ref": "#node"
        });
        serde_json::from_value::<ToolCatalog>(anchored)
            .expect("a local anchor is a valid Draft 2020-12 reference target");

        let mut dynamic_anchor = valid_catalog_json();
        dynamic_anchor["tools"][0]["outputSchema"] = json!({
            "$defs": {"result": {"$dynamicAnchor": "node", "type": "string"}},
            "$dynamicRef": "#node"
        });
        serde_json::from_value::<ToolCatalog>(dynamic_anchor)
            .expect("a local dynamic anchor is a valid Draft 2020-12 reference target");

        let mut dynamic_component = valid_catalog_json();
        dynamic_component["tools"][0]["outputSchema"] =
            json!({"$dynamicRef": "#/components/schemas/Result"});
        serde_json::from_value::<ToolCatalog>(dynamic_component)
            .expect("a dynamic reference can target a catalog component");

        let mut nested_resource = valid_catalog_json();
        nested_resource["tools"][0]["outputSchema"] = json!({
            "$id": "https://example.test/root",
            "allOf": [{
                "$id": "nested",
                "$defs": {"result": {"type": "string"}},
                "$ref": "#/$defs/result"
            }]
        });
        serde_json::from_value::<ToolCatalog>(nested_resource)
            .expect("a local pointer resolves from its containing schema resource");

        let mut legacy_definitions = valid_catalog_json();
        legacy_definitions["tools"][0]["outputSchema"] = json!({
            "definitions": {
                "Legacy": {"$ref": "#/components/schemas/Result"}
            },
            "$ref": "#/definitions/Legacy"
        });
        let legacy_catalog: ToolCatalog = serde_json::from_value(legacy_definitions)
            .expect("Draft 2020-12 retains definitions as a schema-valued map");
        let legacy_projected = legacy_catalog
            .mcp_tool(&legacy_catalog.tools[0])
            .expect("legacy definitions receive the same standalone MCP projection");
        assert_eq!(
            legacy_projected["outputSchema"]["definitions"]["Legacy"]["$ref"],
            "#/$defs/Result"
        );
        assert_eq!(
            legacy_projected["outputSchema"]["$defs"]["Result"]["type"],
            "object"
        );

        let mut dangling_anchor = valid_catalog_json();
        dangling_anchor["tools"][0]["outputSchema"] = json!({"$ref": "#missing"});
        assert!(serde_json::from_value::<ToolCatalog>(dangling_anchor).is_err());

        let mut dangling_dynamic_anchor = valid_catalog_json();
        dangling_dynamic_anchor["tools"][0]["outputSchema"] = json!({"$dynamicRef": "#missing"});
        assert!(serde_json::from_value::<ToolCatalog>(dangling_dynamic_anchor).is_err());

        let mut dangling_dynamic_component = valid_catalog_json();
        dangling_dynamic_component["tools"][0]["outputSchema"] =
            json!({"$dynamicRef": "#/components/schemas/Missing"});
        assert!(serde_json::from_value::<ToolCatalog>(dangling_dynamic_component).is_err());
    }

    #[test]
    fn schema_projection_preserves_reference_shaped_instance_data() {
        let literal = json!({
            "$id": "literal-id",
            "$ref": "#/components/schemas/Missing",
            "$dynamicRef": "https://example.test/missing"
        });
        let mut value = valid_catalog_json();
        value["tools"][0]["outputSchema"] = json!({
            "type": "object",
            "properties": {
                "constant": {"const": &literal},
                "choice": {"enum": [&literal]},
                "result": {"$ref": "#/components/schemas/Result"}
            }
        });
        let catalog: ToolCatalog = serde_json::from_value(value)
            .expect("reference-shaped instance data is not a schema reference");
        let projected = catalog
            .mcp_tool(&catalog.tools[0])
            .expect("instance data does not make MCP projection unsafe");
        assert_eq!(
            projected["outputSchema"]["properties"]["constant"]["const"],
            literal
        );
        assert_eq!(
            projected["outputSchema"]["properties"]["choice"]["enum"][0],
            literal
        );
        assert_eq!(
            projected["outputSchema"]["$defs"]["Result"]["type"],
            "object"
        );
        assert!(projected["outputSchema"]["$defs"].get("Missing").is_none());
    }

    #[test]
    fn explicit_null_optionals_are_accepted_and_omitted_when_reserialized() {
        let schema = tool_catalog_json_schema();
        let validator = jsonschema::draft202012::new(&schema).expect("catalog schema");
        let mut value = valid_catalog_json();
        value["info"]["version"] = JsonValue::Null;
        value["info"]["description"] = JsonValue::Null;
        for field in [
            "title",
            "description",
            "outputSchema",
            "annotations",
            "icons",
            "execution",
            "namespace",
            "source",
            "policy",
            "_meta",
        ] {
            value["tools"][0][field] = JsonValue::Null;
        }
        value["components"] = JsonValue::Null;
        assert!(
            validator.is_valid(&value),
            "schema must accept explicit nulls"
        );
        let catalog: ToolCatalog =
            serde_json::from_value(value).expect("Rust parser accepts nulls");
        let wire = serde_json::to_value(catalog).expect("catalog serializes");
        assert!(wire["info"].get("version").is_none());
        assert!(wire["tools"][0].get("outputSchema").is_none());
        assert!(wire.get("components").is_none());
    }

    #[test]
    fn mcp_projection_fails_closed_for_unsupported_resource_scope() {
        for (keyword, keyword_value) in [
            ("$id", json!("nested")),
            ("$anchor", json!("node")),
            ("$dynamicAnchor", json!("node")),
        ] {
            let mut value = valid_catalog_json();
            value["components"]["schemas"]["Result"][keyword] = keyword_value;
            let catalog: ToolCatalog = serde_json::from_value(value)
                .unwrap_or_else(|error| panic!("catalog must retain {keyword}: {error}"));
            let error = catalog
                .mcp_tool(&catalog.tools[0])
                .expect_err("resource-scoped component cannot be safely embedded");
            assert!(error.to_string().contains(keyword));
        }

        for (keyword, keyword_value) in [
            ("$dynamicRef", json!("https://example.test/schema#node")),
            ("$ref", json!("https://example.test/schema")),
        ] {
            let mut value = valid_catalog_json();
            value["tools"][0]["outputSchema"] = json!({(keyword): keyword_value});
            let catalog: ToolCatalog = serde_json::from_value(value)
                .unwrap_or_else(|error| panic!("catalog must retain {keyword}: {error}"));
            let error = catalog
                .mcp_tool(&catalog.tools[0])
                .expect_err("non-standalone entry schema must fail MCP preparation");
            assert!(error.to_string().contains(keyword));
        }
    }

    #[test]
    fn mcp_projection_prefers_annotation_title_and_bundles_components() {
        let mut value = valid_catalog_json();
        value["tools"][0]["title"] = json!("General catalog title");
        value["tools"][0]["annotations"]["title"] = json!("Human title");
        value["tools"][0]["outputSchema"] =
            json!({"$ref": "#/components/schemas/Result~1Envelope"});
        value["components"]["schemas"]["Result/Envelope"] = json!({
            "type": "object",
            "$defs": {"Label": {"type": "string"}},
            "properties": {
                "label": {"$ref": "#/$defs/Label"},
                "detail": {"$ref": "#/components/schemas/Detail"},
                "child": {"$ref": "#"}
            },
            "required": ["label", "detail"]
        });
        value["components"]["schemas"]["Detail"] = json!({"type": "string"});
        value["components"]["schemas"]["Unused"] = json!({"type": "integer"});
        let catalog: ToolCatalog = serde_json::from_value(value).expect("valid catalog");
        let projected = catalog.mcp_tool(&catalog.tools[0]).expect("MCP projection");
        assert_eq!(projected["title"], "General catalog title");
        assert_eq!(projected["annotations"]["title"], "Human title");
        assert_eq!(
            projected["outputSchema"]["$ref"],
            "#/$defs/Result~1Envelope"
        );
        assert_eq!(
            projected["outputSchema"]["$defs"]["Result/Envelope"]["type"],
            "object"
        );
        assert_eq!(
            projected["outputSchema"]["$defs"]["Result/Envelope"]["properties"]["detail"]["$ref"],
            "#/$defs/Detail"
        );
        assert_eq!(
            projected["outputSchema"]["$defs"]["Result/Envelope"]["properties"]["label"]["$ref"],
            "#/$defs/Result~1Envelope/$defs/Label"
        );
        assert_eq!(
            projected["outputSchema"]["$defs"]["Result/Envelope"]["properties"]["child"]["$ref"],
            "#/$defs/Result~1Envelope"
        );
        assert_eq!(
            projected["outputSchema"]["$defs"]["Detail"]["type"],
            "string"
        );
        assert!(projected["outputSchema"]["$defs"].get("Unused").is_none());
        assert!(projected["outputSchema"]["$defs"].get("Result").is_none());

        let structured = catalog
            .mcp_structured_content(
                &catalog.tools[0],
                json!({
                    "label": "root",
                    "detail": "visible",
                    "child": {"label": "nested", "detail": "visible"}
                }),
            )
            .expect("MCP result projection")
            .expect("declared output");
        let validator = jsonschema::draft202012::new(&projected["outputSchema"])
            .expect("standalone schema resolves every rebased ref");
        assert!(validator.is_valid(&structured));
    }

    #[test]
    fn non_object_output_schema_and_result_share_one_wrapper() {
        let mut value = valid_catalog_json();
        value["tools"][0]["outputSchema"] = json!({"type": "string"});
        let catalog: ToolCatalog = serde_json::from_value(value).expect("valid catalog");
        let entry = &catalog.tools[0];
        let projected = catalog.mcp_tool(entry).expect("MCP projection");
        let structured =
            tool_result_to_mcp_structured_content(entry, json!("ok")).expect("declared output");
        assert_eq!(structured, json!({"result": "ok"}));
        let validator = jsonschema::draft202012::new(&projected["outputSchema"])
            .expect("valid standalone output schema");
        assert!(validator.is_valid(&structured));
    }

    #[test]
    fn wire_and_typescript_preserve_optional_and_false_compatibility() {
        let catalog: ToolCatalog = serde_json::from_value(valid_catalog_json()).unwrap();
        let wire = serde_json::to_value(&catalog).unwrap();
        assert_eq!(wire["tools"][0]["cli"]["hidden"], false);
        assert_eq!(wire["tools"][0]["deferLoading"], false);
        assert_eq!(wire["tools"][0]["icons"][0]["theme"], "dark");

        let typescript = tool_catalog_typescript();
        assert!(typescript.ends_with('\n'));
        assert!(!typescript.ends_with("\n\n"));
        assert!(
            typescript.lines().all(|line| line == line.trim_end()),
            "generated TypeScript must not contain trailing line whitespace"
        );
        for expected in [
            "title?: string | null",
            "theme?: ToolIconTheme | null",
            "outputSchema?: JsonSchema202012 | null",
            "binding?: Readonly<Record<string, JsonValue>> | null",
            "_meta?: Readonly<Record<string, JsonValue>> | null",
            "hidden: boolean",
            "deferLoading: boolean",
        ] {
            assert!(
                typescript.contains(expected),
                "missing TS fragment {expected:?}"
            );
        }
    }
}
