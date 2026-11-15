//! Runtime schemas remain available when a contract has no closed portable type.

use super::*;
use crate::value::VmValue;
use crate::wire::{PreparedWireType, WireFailure};

type WirePreparation = Result<PreparedWireType, WireFailure>;

#[derive(Clone, Debug)]
pub(super) struct PreparedWireEntry {
    input: WirePreparation,
    output: Option<WirePreparation>,
    error: Option<WirePreparation>,
}

impl PreparedWireEntry {
    pub(super) fn prepare(
        tool: &ToolCatalogEntry,
        resources: &RuntimeSchemaResources,
    ) -> Result<Self, PreparedToolCatalogError> {
        let prepared = Self {
            input: prepare_schema(&tool.name, &tool.input_schema, resources)?,
            output: tool
                .output_schema
                .as_ref()
                .map(|schema| prepare_schema(&tool.name, schema, resources))
                .transpose()?,
            error: tool
                .error_schema
                .as_ref()
                .map(|schema| prepare_schema(&tool.name, schema, resources))
                .transpose()?,
        };
        Ok(prepared)
    }

    fn get(&self, phase: ToolContractPhase) -> Option<&WirePreparation> {
        match phase {
            ToolContractPhase::Input => Some(&self.input),
            ToolContractPhase::Output => self.output.as_ref(),
            ToolContractPhase::ApplicationError => self.error.as_ref(),
        }
    }
}

fn prepare_schema(
    tool: &str,
    schema: &JsonValue,
    resources: &RuntimeSchemaResources,
) -> Result<WirePreparation, PreparedToolCatalogError> {
    let resource_failure = || {
        PreparedToolCatalogError::new(format!(
            "tool {tool:?} has invalid runtime schema resources"
        ))
    };
    let schema = resources.rewrite(schema)?;
    let registry = resources
        .registry
        .add(RUNTIME_SCHEMA_ROOT_URI, schema)
        .and_then(|builder| builder.prepare())
        .map_err(|_| resource_failure())?;
    let resolver = registry
        .resolver(jsonschema::uri::from_str(RUNTIME_SCHEMA_ROOT_URI).expect("fixed runtime URI"));
    let root = resolver.lookup("").map_err(|_| resource_failure())?;
    let (schema, resolver, _) = root.into_inner();
    let annotations = crate::wire::schema::wire_annotations(schema, resolver.clone());
    let mut builder = if annotations.declared {
        crate::wire::schema::SchemaWireBuilder::runtime(registry.clone())
    } else {
        crate::wire::schema::SchemaWireBuilder::new()
    };
    let prepared = builder
        .lower(schema, resolver, RUNTIME_SCHEMA_ROOT_URI)
        .and_then(|root| {
            builder
                .finish()
                .map(|graph| PreparedWireType::from_graph(graph, root))
        });
    if annotations.declared {
        prepared.map(Ok).map_err(|failure| {
            PreparedToolCatalogError::new(format!(
                "tool {tool:?} has invalid declared wire contract: {failure}"
            ))
        })
    } else {
        Ok(prepared)
    }
}

impl PreparedToolCatalog {
    /// A caller can require portable capability instead of accepting a gradual schema.
    pub fn prepared_wire_type(
        &self,
        name: &str,
        phase: ToolContractPhase,
    ) -> Result<Option<&PreparedWireType>, &WireFailure> {
        let Some(index) = self.names.get(name) else {
            return Ok(None);
        };
        self.entries[*index]
            .wire
            .get(phase)
            .map(Result::as_ref)
            .transpose()
    }

    pub fn decode_input(
        &self,
        name: &str,
        value: &JsonValue,
    ) -> Result<VmValue, ToolContractViolation> {
        self.validate_input(name, value)?;
        match self.prepared_wire_type(name, ToolContractPhase::Input) {
            Ok(Some(wire)) => wire
                .decode(value)
                .map_err(|failure| wire_violation(name, ToolContractPhase::Input, failure)),
            // Existing runtime-only and gradual schemas are intentionally not codegen IR.
            _ => Ok(crate::value::json::json_to_vm_value(value)),
        }
    }

    pub(crate) fn encode_wire_value(
        &self,
        name: &str,
        phase: ToolContractPhase,
        value: &VmValue,
    ) -> Result<JsonValue, ToolContractViolation> {
        match self.prepared_wire_type(name, phase) {
            Ok(Some(wire)) => wire
                .encode(value)
                .map_err(|failure| wire_violation(name, phase, failure)),
            _ => crate::llm::helpers::vm_value_to_export_json_strict(value, "").map_err(|_| {
                ToolContractViolation {
                    tool: name.into(),
                    phase,
                    wire_failure: None,
                    violations: vec![ToolContractViolationDetail {
                        structural_path: String::new(),
                        schema_path: String::new(),
                        keyword: "portableJson".into(),
                        missing_property: None,
                    }],
                }
            }),
        }
    }
}

fn wire_violation(
    name: &str,
    phase: ToolContractPhase,
    failure: WireFailure,
) -> ToolContractViolation {
    ToolContractViolation {
        tool: name.into(),
        phase,
        violations: vec![ToolContractViolationDetail {
            structural_path: failure.structural_path.clone(),
            schema_path: String::new(),
            keyword: "wireType".into(),
            missing_property: None,
        }],
        wire_failure: Some(Box::new(failure)),
    }
}
