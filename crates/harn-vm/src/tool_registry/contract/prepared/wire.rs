//! Runtime schemas remain available when a contract has no closed portable type.

use super::*;
use crate::value::VmValue;
use crate::wire::{PreparedWireType, WireFailure, WireFailureCode, WireSourceRef};

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
            input: prepare_schema(&tool.input_schema, resources),
            output: tool
                .output_schema
                .as_ref()
                .map(|schema| prepare_schema(schema, resources)),
            error: tool
                .error_schema
                .as_ref()
                .map(|schema| prepare_schema(schema, resources)),
        };
        for (phase, schema) in [
            (ToolContractPhase::Input, Some(&tool.input_schema)),
            (ToolContractPhase::Output, tool.output_schema.as_ref()),
            (
                ToolContractPhase::ApplicationError,
                tool.error_schema.as_ref(),
            ),
        ] {
            let Some(schema) = schema else { continue };
            let mut declared_wire = false;
            let mut schema = schema.clone();
            crate::tool_registry::transform_codegen_schema(&mut schema, &mut |object| {
                declared_wire |=
                    object.contains_key("x-harn-wire-id") || object.contains_key("x-harn-type");
                Ok::<(), std::convert::Infallible>(())
            })
            .expect("infallible metadata census");
            if declared_wire {
                if let Some(Err(failure)) = prepared.get(phase) {
                    return Err(PreparedToolCatalogError::new(format!(
                        "tool {:?} has invalid declared wire contract: {failure}",
                        tool.name
                    )));
                }
            }
        }
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

fn prepare_schema(schema: &JsonValue, resources: &RuntimeSchemaResources) -> WirePreparation {
    let failure = || WireFailure {
        code: WireFailureCode::InvalidContract,
        type_id: None,
        source: WireSourceRef::Synthetic {
            owner: "tool_catalog".into(),
        },
        structural_path: String::new(),
    };
    let schema = resources.rewrite(schema).map_err(|_| failure())?;
    let registry = resources
        .registry
        .add(RUNTIME_SCHEMA_ROOT_URI, schema)
        .and_then(|builder| builder.prepare())
        .map_err(|_| failure())?;
    let resolver = registry
        .resolver(jsonschema::uri::from_str(RUNTIME_SCHEMA_ROOT_URI).expect("fixed runtime URI"));
    let root = resolver.lookup("").map_err(|_| failure())?;
    let (schema, resolver, _) = root.into_inner();
    let mut builder = crate::wire::schema::SchemaWireBuilder::new();
    let root = builder.lower(schema, resolver, RUNTIME_SCHEMA_ROOT_URI)?;
    Ok(PreparedWireType::from_graph(builder.finish()?, root))
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
        wire_failure: Some(failure),
    }
}
