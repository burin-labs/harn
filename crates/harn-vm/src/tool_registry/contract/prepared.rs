//! Immutable, compiled execution view of one portable tool catalog.

use std::collections::BTreeMap;

use jsonschema::Validator;
use serde_json::Value as JsonValue;

use super::{
    decode_json_pointer_segment, try_transform_schema_nodes, ToolAudience, ToolCatalog,
    ToolCatalogEntry,
};

mod cli;

pub use cli::{PreparedCliArgument, PreparedCliCommand, PreparedCliTree};

const RUNTIME_SCHEMA_ROOT_URI: &str = "https://harn.invalid/prepared-tool-schema";

/// Failure while preparing a catalog for execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedToolCatalogError {
    message: String,
}

impl PreparedToolCatalogError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PreparedToolCatalogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PreparedToolCatalogError {}

/// Which side of an executable tool contract rejected a value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolContractPhase {
    Input,
    Output,
}

impl ToolContractPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// Stable runtime validation failure tied to one catalog entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolContractViolation {
    pub tool: String,
    pub phase: ToolContractPhase,
    pub violations: Vec<String>,
}

impl std::fmt::Display for ToolContractViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "tool {:?} {} violates its declared schema: {}",
            self.tool,
            self.phase.label(),
            self.violations.join("; ")
        )
    }
}

impl std::error::Error for ToolContractViolation {}

#[derive(Clone, Debug)]
struct PreparedToolEntry {
    input: Validator,
    output: Option<Validator>,
}

/// A validated catalog with every instance validator and adapter projection
/// compiled once at the load boundary.
#[derive(Clone, Debug)]
pub struct PreparedToolCatalog {
    catalog: ToolCatalog,
    entries: Vec<PreparedToolEntry>,
    names: BTreeMap<String, usize>,
    mcp_tools: Vec<JsonValue>,
    cli_tree: PreparedCliTree,
}

impl PreparedToolCatalog {
    /// Validate and compile an immutable execution view of `catalog`.
    pub fn prepare(catalog: ToolCatalog) -> Result<Self, PreparedToolCatalogError> {
        catalog.validate().map_err(PreparedToolCatalogError::new)?;
        let cli_tree = PreparedCliTree::prepare(&catalog)?;
        let components = catalog
            .components
            .as_ref()
            .map(|components| &components.schemas);
        let runtime_schemas = RuntimeSchemaResources::prepare(components)?;
        let mut entries = Vec::with_capacity(catalog.tools.len());
        let mut names = BTreeMap::new();
        let mut mcp_tools = Vec::new();
        for (index, tool) in catalog.tools.iter().enumerate() {
            let input_schema = runtime_schemas.rewrite(&tool.input_schema)?;
            let input = compile_validator(
                &tool.name,
                ToolContractPhase::Input,
                &input_schema,
                &runtime_schemas.registry,
            )?;
            let output = tool
                .output_schema
                .as_ref()
                .map(|schema| {
                    runtime_schemas.rewrite(schema).and_then(|schema| {
                        compile_validator(
                            &tool.name,
                            ToolContractPhase::Output,
                            &schema,
                            &runtime_schemas.registry,
                        )
                    })
                })
                .transpose()?;
            names.insert(tool.name.clone(), index);
            entries.push(PreparedToolEntry { input, output });
            if tool.governance.allows(ToolAudience::Mcp) {
                mcp_tools.push(
                    catalog
                        .mcp_tool(tool)
                        .map_err(|error| PreparedToolCatalogError::new(error.to_string()))?,
                );
            }
        }
        Ok(Self {
            catalog,
            entries,
            names,
            mcp_tools,
            cli_tree,
        })
    }

    pub fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    pub fn entry(&self, name: &str) -> Option<&ToolCatalogEntry> {
        self.names
            .get(name)
            .map(|index| &self.catalog.tools[*index])
    }

    pub fn mcp_tools(&self) -> &[JsonValue] {
        &self.mcp_tools
    }

    /// Validated, normalized command tree shared by CLI adapters and
    /// completion generators.
    pub fn cli_tree(&self) -> &PreparedCliTree {
        &self.cli_tree
    }

    pub fn validate_input(
        &self,
        name: &str,
        value: &JsonValue,
    ) -> Result<(), ToolContractViolation> {
        self.validate(name, ToolContractPhase::Input, value)
    }

    pub fn validate_output(
        &self,
        name: &str,
        value: &JsonValue,
    ) -> Result<(), ToolContractViolation> {
        self.validate(name, ToolContractPhase::Output, value)
    }

    fn validate(
        &self,
        name: &str,
        phase: ToolContractPhase,
        value: &JsonValue,
    ) -> Result<(), ToolContractViolation> {
        let Some(index) = self.names.get(name).copied() else {
            return Err(ToolContractViolation {
                tool: name.to_string(),
                phase,
                violations: vec!["tool is absent from the prepared catalog".to_string()],
            });
        };
        let validator = match phase {
            ToolContractPhase::Input => Some(&self.entries[index].input),
            ToolContractPhase::Output => self.entries[index].output.as_ref(),
        };
        let Some(validator) = validator else {
            return Ok(());
        };
        let violations = validator
            .iter_errors(value)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(ToolContractViolation {
                tool: name.to_string(),
                phase,
                violations,
            })
        }
    }
}

fn compile_validator(
    tool: &str,
    phase: ToolContractPhase,
    schema: &JsonValue,
    registry: &jsonschema::Registry,
) -> Result<Validator, PreparedToolCatalogError> {
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_registry(registry)
        .with_base_uri(RUNTIME_SCHEMA_ROOT_URI)
        .build(schema)
        .map_err(|error| {
            PreparedToolCatalogError::new(format!(
                "tool {tool:?} {} schema could not be compiled: {error}",
                phase.label()
            ))
        })
}

struct RuntimeSchemaResources {
    component_uris: BTreeMap<String, String>,
    registry: jsonschema::Registry<'static>,
}

impl RuntimeSchemaResources {
    fn prepare(
        components: Option<&BTreeMap<String, JsonValue>>,
    ) -> Result<Self, PreparedToolCatalogError> {
        let component_uris = components
            .into_iter()
            .flat_map(|components| components.keys())
            .enumerate()
            .map(|(index, name)| {
                (
                    name.clone(),
                    format!("https://harn.invalid/prepared-tool-components/{index}"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let resources = components
            .into_iter()
            .flat_map(|components| components.iter())
            .map(|(name, schema)| {
                let uri = component_uris
                    .get(name)
                    .expect("every component has a prepared URI")
                    .clone();
                rewrite_component_refs(schema.clone(), &component_uris).map(|schema| (uri, schema))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let registry = jsonschema::Registry::new()
            .draft(jsonschema::Draft::Draft202012)
            .extend(resources)
            .and_then(|builder| builder.prepare())
            .map_err(|error| {
                PreparedToolCatalogError::new(format!(
                    "tool catalog components could not be prepared: {error}"
                ))
            })?;
        Ok(Self {
            component_uris,
            registry,
        })
    }

    fn rewrite(&self, schema: &JsonValue) -> Result<JsonValue, PreparedToolCatalogError> {
        rewrite_component_refs(schema.clone(), &self.component_uris)
    }
}

fn rewrite_component_refs(
    mut schema: JsonValue,
    component_uris: &BTreeMap<String, String>,
) -> Result<JsonValue, PreparedToolCatalogError> {
    try_transform_schema_nodes(&mut schema, &mut |object| {
        for keyword in ["$ref", "$dynamicRef"] {
            let Some(reference) = object.get(keyword).and_then(JsonValue::as_str) else {
                continue;
            };
            let Some(path) = reference.strip_prefix("#/components/schemas/") else {
                continue;
            };
            let (encoded_name, nested_path) = path
                .split_once('/')
                .map_or((path, None), |(name, path)| (name, Some(path)));
            let name = decode_json_pointer_segment(encoded_name).ok_or_else(|| {
                PreparedToolCatalogError::new(format!(
                    "invalid schema component reference {reference:?}"
                ))
            })?;
            let uri = component_uris.get(&name).ok_or_else(|| {
                PreparedToolCatalogError::new(format!(
                    "dangling schema component reference {reference:?}"
                ))
            })?;
            object.insert(
                keyword.to_string(),
                JsonValue::String(match nested_path {
                    Some(path) => format!("{uri}#/{path}"),
                    None => uri.clone(),
                }),
            );
        }
        Ok::<(), PreparedToolCatalogError>(())
    })?;
    Ok(schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::{
        ToolCatalogComponents, ToolCatalogSchemaVersion, ToolCliSpec, ToolGovernance,
    };
    use serde_json::json;

    fn catalog() -> ToolCatalog {
        ToolCatalog {
            schema_version: ToolCatalogSchemaVersion::V1,
            info: None,
            cli: None,
            tools: vec![ToolCatalogEntry {
                name: "create_widget".to_string(),
                title: None,
                description: None,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "widget": {"$ref": "#/components/schemas/Widget"}
                    },
                    "required": ["widget"],
                    "additionalProperties": false
                }),
                output_schema: Some(json!({"$ref": "#/components/schemas/Widget"})),
                annotations: None,
                icons: None,
                execution: None,
                governance: ToolGovernance::default(),
                cli: ToolCliSpec {
                    command: vec!["widgets".to_string(), "create".to_string()],
                    aliases: Vec::new(),
                    hidden: false,
                    arguments: BTreeMap::new(),
                },
                namespace: None,
                defer_loading: false,
                source: None,
                policy: None,
                meta: None,
            }],
            components: Some(ToolCatalogComponents {
                schemas: BTreeMap::from([(
                    "Widget".to_string(),
                    json!({
                        "type": "object",
                        "properties": {"kind": {"const": "widget"}, "count": {"type": "integer", "minimum": 1}},
                        "required": ["kind", "count"],
                        "additionalProperties": false
                    }),
                )]),
            }),
        }
    }

    #[test]
    fn compiles_shared_components_once_and_validates_both_phases() {
        let prepared = PreparedToolCatalog::prepare(catalog()).expect("prepare catalog");
        prepared
            .validate_input(
                "create_widget",
                &json!({"widget": {"kind": "widget", "count": 1}}),
            )
            .expect("valid input");
        let input_error = prepared
            .validate_input(
                "create_widget",
                &json!({"widget": {"kind": "widget", "count": 0}, "ignored": true}),
            )
            .expect_err("invalid input");
        assert_eq!(input_error.phase, ToolContractPhase::Input);
        assert!(!input_error.violations.is_empty());

        prepared
            .validate_output("create_widget", &json!({"kind": "widget", "count": 2}))
            .expect("valid output");
        let output_error = prepared
            .validate_output("create_widget", &json!({"kind": "other", "count": 2}))
            .expect_err("invalid output");
        assert_eq!(output_error.phase, ToolContractPhase::Output);
    }

    #[test]
    fn mcp_projection_excludes_non_mcp_tools() {
        let mut catalog = catalog();
        catalog.tools[0].governance.audiences = vec![ToolAudience::Cli];
        let prepared = PreparedToolCatalog::prepare(catalog).expect("prepare catalog");
        assert!(prepared.mcp_tools().is_empty());
    }

    #[test]
    fn runtime_validation_preserves_resource_scope_without_weakening_mcp_projection() {
        let mut cli_catalog = catalog();
        cli_catalog.tools[0].governance.audiences = vec![ToolAudience::Cli];
        cli_catalog.components.as_mut().unwrap().schemas.insert(
            "Widget".to_string(),
            json!({
                "$id": "https://example.invalid/widget",
                "$defs": {
                    "positiveCount": {"type": "integer", "minimum": 1}
                },
                "type": "object",
                "properties": {
                    "kind": {"const": "widget"},
                    "count": {"$ref": "#/$defs/positiveCount"}
                },
                "required": ["kind", "count"],
                "additionalProperties": false
            }),
        );

        let prepared = PreparedToolCatalog::prepare(cli_catalog.clone())
            .expect("CLI runtime preserves JSON Schema resource scope");
        prepared
            .validate_input(
                "create_widget",
                &json!({"widget": {"kind": "widget", "count": 1}}),
            )
            .expect("local resource reference validates");
        prepared
            .validate_input(
                "create_widget",
                &json!({"widget": {"kind": "widget", "count": 0}}),
            )
            .expect_err("local resource reference rejects invalid input");

        cli_catalog.tools[0].governance.audiences = vec![ToolAudience::Mcp];
        let error = PreparedToolCatalog::prepare(cli_catalog)
            .expect_err("MCP cannot safely relocate resource-scoped components");
        assert!(error
            .to_string()
            .contains("$id changes JSON Schema resource scope"));
    }

    #[test]
    fn runtime_preparation_does_not_fetch_external_schema_resources() {
        let mut catalog = catalog();
        catalog.tools[0].governance.audiences = vec![ToolAudience::Cli];
        catalog.tools[0].input_schema = json!({
            "type": "object",
            "properties": {
                "value": {"$ref": "https://example.invalid/schema.json"}
            }
        });

        let error = PreparedToolCatalog::prepare(catalog)
            .expect_err("external resources are unavailable at the execution boundary");
        assert!(
            error
                .to_string()
                .contains("https://example.invalid/schema.json"),
            "{error}"
        );
    }
}
