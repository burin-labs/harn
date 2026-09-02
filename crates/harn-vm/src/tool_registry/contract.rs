//! Versioned, transport-neutral Harn tool catalog data contract.
//!
//! Runtime closures and adapter configuration deliberately do not appear here.
//! OpenAPI and Harn exports normalize into this contract; CLI, MCP, docs, and
//! native clients project from it.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value as JsonValue};
use ts_rs::{Config, TS};

mod cli;
mod prepared;
mod schema;

use schema::{
    decode_json_pointer_segment, encode_json_pointer_segment, try_transform_schema_nodes,
    try_visit_schema_nodes,
};

pub use crate::mcp_tasks::McpTaskSupport as ToolTaskSupport;
use crate::tool_annotations::{SideEffectLevel, ToolKind};
pub use cli::{
    ToolCliArgumentSpec, ToolCliBooleanStyle, ToolCliCommandSpec, ToolCliSpec, ToolCliTreeSpec,
    ToolCliValueHint,
};
pub use prepared::{
    PreparedCliArgument, PreparedCliCommand, PreparedCliTree, PreparedToolCatalog,
    PreparedToolCatalogError, ToolApplicationError, ToolContractPhase, ToolContractViolation,
    ToolContractViolationDetail, ToolThrownClassification,
};

pub const TOOL_CATALOG_SCHEMA_VERSION: &str = "harn-tools/2.0";
pub const TOOL_CATALOG_SCHEMA_ARTIFACT: &str = "schemas/harn-tools-v2.schema.json";
const JSON_SCHEMA_2020_12_URI: &str = "https://json-schema.org/draft/2020-12/schema";
pub const TOOL_CATALOG_TYPESCRIPT_ARTIFACT: &str = "harn-tools.ts";
/// Harn-owned metadata nested below MCP `_meta`.
///
/// MCP reserves reverse-DNS names for vendor extensions. Keeping the complete
/// typed-tool payload below one key prevents collisions with caller metadata
/// and leaves room for compatible additions to the Harn projection.
pub const HARN_MCP_TOOL_CONTRACT_META_KEY: &str = "com.harnlang/toolContract";

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
    #[serde(rename = "harn-tools/2.0")]
    V2,
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
    /// Portable shape of values deliberately thrown by this tool.
    ///
    /// Runtime faults remain adapter errors. Only a raw Harn `throw` from a
    /// handler with this contract is application error data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable, type = "JsonSchema202012 | null")]
    #[schemars(schema_with = "json_schema_document")]
    pub error_schema: Option<JsonValue>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub cli: Option<ToolCliTreeSpec>,
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
    cli: Option<ToolCliTreeSpec>,
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
            cli: raw.cli,
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
            if tool
                .meta
                .as_ref()
                .is_some_and(|meta| meta.contains_key(HARN_MCP_TOOL_CONTRACT_META_KEY))
            {
                return Err(format!(
                    "{context}._meta[{HARN_MCP_TOOL_CONTRACT_META_KEY:?}] is reserved for Harn-owned adapter projections"
                ));
            }
            if let Some(meta) = tool.meta.as_ref() {
                for key in meta.keys() {
                    crate::mcp_protocol::validate_application_meta_key(key)
                        .map_err(|error| format!("{context}._meta key {key:?} {error}"))?;
                }
            }
            validate_schema_document(&tool.input_schema, true, &format!("{context}.inputSchema"))?;
            validate_portable_schema_extensions(
                &tool.input_schema,
                &format!("{context}.inputSchema"),
            )?;
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
                validate_portable_schema_extensions(
                    output_schema,
                    &format!("{context}.outputSchema"),
                )?;
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
            if let Some(error_schema) = tool.error_schema.as_ref() {
                validate_schema_document(error_schema, false, &format!("{context}.errorSchema"))?;
                validate_portable_schema_extensions(
                    error_schema,
                    &format!("{context}.errorSchema"),
                )?;
                validate_defs_do_not_shadow_components(
                    error_schema,
                    component_schemas,
                    &format!("{context}.errorSchema"),
                )?;
                validate_refs(
                    error_schema,
                    component_schemas,
                    &format!("{context}.errorSchema"),
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
    try_visit_schema_nodes(schema, &mut |object| {
        if let Some(dialect) = object.get("$schema").and_then(JsonValue::as_str) {
            if dialect.trim_end_matches('#') != JSON_SCHEMA_2020_12_URI {
                return Err(format!(
                    "{field} declares unsupported JSON Schema dialect {dialect:?}; expected Draft 2020-12 ({JSON_SCHEMA_2020_12_URI})"
                ));
            }
        }
        Ok(())
    })?;
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
    if let Some(error_schema) = entry.error_schema.as_ref() {
        let error_schema = standalone_schema(error_schema, components)?;
        tool.as_object_mut()
            .expect("MCP tool projection is an object")
            .entry("_meta")
            .or_insert_with(|| json!({}))[HARN_MCP_TOOL_CONTRACT_META_KEY] =
            json!({"errorSchema": error_schema});
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
    if let Some(meta) = entry.meta.as_ref() {
        let projected = tool
            .as_object_mut()
            .expect("MCP tool projection is an object")
            .entry("_meta")
            .or_insert_with(|| json!({}));
        let projected = projected
            .as_object_mut()
            .expect("Harn creates MCP tool metadata as an object");
        for (key, value) in meta {
            if key == HARN_MCP_TOOL_CONTRACT_META_KEY {
                continue;
            }
            projected.insert(key.clone(), value.clone());
        }
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

fn validate_portable_schema_extensions(value: &JsonValue, field: &str) -> Result<(), String> {
    try_visit_schema_nodes(value, &mut |object| {
        let Some(kind) = object.get("x-harn-type").and_then(JsonValue::as_str) else {
            return Ok(());
        };
        if matches!(kind, "closure" | "builtin") {
            return Err(format!(
                "{field} contains runtime-only Harn type {kind:?}, which has no portable JSON value"
            ));
        }
        Ok(())
    })
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

fn tool_output_to_mcp_structured_content(
    output_schema: Option<&JsonValue>,
    result: JsonValue,
) -> Option<JsonValue> {
    output_schema.map(|schema| {
        if mcp_schema_has_object_root(schema) {
            result
        } else {
            json!({"result": result})
        }
    })
}

fn mcp_output_schema(output_schema: &JsonValue) -> JsonValue {
    if mcp_schema_has_object_root(output_schema) {
        output_schema.clone()
    } else {
        json!({
            "type": "object",
            "properties": {"result": output_schema},
            "required": ["result"],
            "additionalProperties": false,
        })
    }
}

fn mcp_schema_has_object_root(schema: &JsonValue) -> bool {
    schema_requires_object(schema, schema, &mut std::collections::BTreeSet::new())
}

fn schema_requires_object<'a>(
    root: &'a JsonValue,
    schema: &'a JsonValue,
    visited_refs: &mut std::collections::BTreeSet<&'a str>,
) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    let typed_as_object = match object.get("type") {
        Some(JsonValue::String(kind)) => kind == "object",
        Some(JsonValue::Array(kinds)) => {
            !kinds.is_empty() && kinds.iter().all(|kind| kind.as_str() == Some("object"))
        }
        _ => false,
    };
    if typed_as_object {
        return true;
    }

    if let Some(reference) = object.get("$ref").and_then(JsonValue::as_str) {
        if let Some(pointer) = reference.strip_prefix('#') {
            if visited_refs.insert(pointer) {
                let referenced = if pointer.is_empty() {
                    Some(root)
                } else {
                    root.pointer(pointer)
                };
                let requires_object = referenced.is_some_and(|referenced| {
                    schema_requires_object(root, referenced, visited_refs)
                });
                visited_refs.remove(pointer);
                if requires_object {
                    return true;
                }
            }
        }
    }

    if object
        .get("allOf")
        .and_then(JsonValue::as_array)
        .is_some_and(|schemas| {
            schemas
                .iter()
                .any(|schema| schema_requires_object(root, schema, visited_refs))
        })
    {
        return true;
    }

    ["anyOf", "oneOf"].into_iter().any(|keyword| {
        object
            .get(keyword)
            .and_then(JsonValue::as_array)
            .is_some_and(|schemas| {
                !schemas.is_empty()
                    && schemas
                        .iter()
                        .all(|schema| schema_requires_object(root, schema, visited_refs))
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
    schema["$id"] = json!("https://harnlang.com/schemas/harn-tools-v2.schema.json");
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
    declaration!(ToolCliValueHint);
    declaration!(ToolCliBooleanStyle);
    declaration!(ToolCliArgumentSpec);
    declaration!(ToolCliCommandSpec);
    declaration!(ToolCliTreeSpec);
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
mod tests;

#[cfg(test)]
#[path = "contract/application_contract_tests.rs"]
mod application_contract_tests;
