use std::collections::BTreeSet;

use super::*;
use crate::tool_registry::{PreparedToolCatalog, ToolCatalog, ToolExecution, ToolTaskSupport};
use crate::wire::{WireFailure, WireSourceRef};

/// Compile only explicitly selected operations, or the complete catalog.
pub fn compile_catalog(
    prepared: &PreparedToolCatalog,
    operations: &[String],
) -> Result<ValidatedCodegenIr, CodegenDiagnostic> {
    compile_catalog_with_sources(prepared, operations, &[])
}

pub fn compile_catalog_with_sources(
    prepared: &PreparedToolCatalog,
    operations: &[String],
    sources: &[WireSourceRef],
) -> Result<ValidatedCodegenIr, CodegenDiagnostic> {
    let catalog = prepared.catalog();
    let selection: BTreeSet<_> = operations.iter().map(String::as_str).collect();
    if selection.len() != operations.len()
        || selection.iter().any(|name| prepared.entry(name).is_none())
    {
        return Err(validation::diagnostic(
            CodegenDiagnosticCode::InvalidSelection,
            "/operations",
        ));
    }
    let selected: Vec<_> = catalog
        .tools
        .iter()
        .filter(|tool| selection.is_empty() || selection.contains(tool.name.as_str()))
        .collect();
    if selected.is_empty() {
        return Err(validation::diagnostic(
            CodegenDiagnosticCode::InvalidSelection,
            "/operations",
        ));
    }
    let resources = prepared.schema_resources();
    let mut roots = Vec::new();
    for tool in &selected {
        for (phase, schema) in [
            ("input", Some(&tool.input_schema)),
            ("output", tool.output_schema.as_ref()),
            ("error", tool.error_schema.as_ref()),
        ] {
            if let Some(schema) = schema {
                roots.push((
                    uri(&tool.name, phase),
                    resources.rewrite(schema).map_err(|_| {
                        validation::diagnostic(
                            CodegenDiagnosticCode::UnsupportedWireType,
                            "/operations",
                        )
                    })?,
                ));
            }
        }
    }
    let registry = resources
        .registry
        .extend(roots)
        .and_then(|builder| builder.prepare())
        .map_err(|_| {
            validation::diagnostic(CodegenDiagnosticCode::UnsupportedWireType, "/operations")
        })?;
    let mut builder = crate::wire::schema::SchemaWireBuilder::new();
    let mut lowered = Vec::new();
    let mut mcp_tools = Vec::new();
    for tool in selected {
        let lower = |builder: &mut crate::wire::schema::SchemaWireBuilder, phase: &str| {
            let uri = uri(&tool.name, phase);
            let resolver =
                registry.resolver(jsonschema::uri::from_str(&uri).expect("encoded schema URI"));
            let located = resolver.lookup("").map_err(|_| {
                validation::diagnostic(CodegenDiagnosticCode::UnsupportedWireType, "/operations")
            })?;
            let (schema, resolver, _) = located.into_inner();
            builder
                .lower(schema, resolver, &uri)
                .map_err(|failure| wire_error(&tool.name, failure))
        };
        let input = lower(&mut builder, "input")?;
        let output = match &tool.output_schema {
            Some(_) => CodegenOutput::Value {
                type_id: lower(&mut builder, "output")?,
                mcp_envelope: catalog
                    .mcp_output_envelope(tool)
                    .map_err(|_| {
                        validation::diagnostic(
                            CodegenDiagnosticCode::InvalidMcpProjection,
                            "/operations/output",
                        )
                    })?
                    .expect("output schema exists"),
            },
            None => {
                return Err(CodegenDiagnostic {
                    operation: Some(tool.name.clone()),
                    ..CodegenDiagnostic::new(CodegenDiagnosticCode::MissingOutput)
                })
            }
        };
        let application_error = tool
            .error_schema
            .as_ref()
            .map(|_| lower(&mut builder, "error"))
            .transpose()?;
        let source = tool
            .source
            .as_ref()
            .and_then(|source| source.location.clone())
            .unwrap_or_else(|| WireSourceRef::DocumentPointer {
                document: "urn:harn:tool-catalog".into(),
                pointer: format!(
                    "/tools/{}",
                    catalog
                        .tools
                        .iter()
                        .position(|candidate| std::ptr::eq(candidate, tool))
                        .expect("selected catalog entry")
                ),
            });
        let binding_kind = match tool.source.as_ref().map(|source| source.kind.as_str()) {
            Some("harn") => CodegenBindingKind::HarnExport,
            Some("openapi") => CodegenBindingKind::OpenApi,
            Some("mcp") => CodegenBindingKind::Mcp,
            _ => CodegenBindingKind::Registry,
        };
        lowered.push(CodegenOperation {
            id: format!("operation:{}", tool.name),
            name: tool.name.clone(),
            source: source.clone(),
            documentation: tool.description.clone(),
            input,
            output,
            application_error,
            execution: CodegenExecution {
                task_support: tool
                    .execution
                    .map_or(ToolTaskSupport::Forbidden, |execution| {
                        execution.task_support
                    }),
                cancellation: CodegenCancellation::McpRequest,
            },
            governance: tool.governance.clone(),
            policy: tool.policy,
            cli: tool.cli.clone(),
            binding: CodegenBinding {
                kind: binding_kind,
                operation: tool
                    .source
                    .as_ref()
                    .and_then(|source| source.id.clone())
                    .unwrap_or_else(|| tool.name.clone()),
                source,
            },
        });
        let mut unary = tool.clone();
        unary.execution = Some(ToolExecution {
            task_support: ToolTaskSupport::Forbidden,
        });
        mcp_tools.push(catalog.mcp_tool(&unary).map_err(|_| {
            validation::diagnostic(CodegenDiagnosticCode::InvalidMcpProjection, "/mcp_tools")
        })?);
    }
    let graph = builder
        .finish()
        .map_err(|failure| wire_error("", failure))?;
    let package_name = catalog
        .info
        .as_ref()
        .map_or("tools", |info| info.name.as_str())
        .to_string();
    let mut ir = CodegenIr {
        schema_version: CodegenSchemaVersion::V1,
        required_features: validation::required_features(&graph, &lowered)
            .into_iter()
            .collect(),
        identity: CodegenIdentity {
            catalog_digest: String::new(),
            binding_digest: String::new(),
            ir_digest: String::new(),
        },
        catalog_canonical_json: crate::canonical_json::to_string(
            &serde_json::to_value(catalog).expect("catalog serializes"),
        ),
        sources: Vec::new(),
        package: CodegenPackage {
            id: format!("package:{package_name}"),
            name: package_name,
            source: WireSourceRef::Synthetic {
                owner: "tool_catalog".into(),
            },
        },
        types: graph.types().cloned().collect(),
        operations: lowered,
        mcp_tools,
    };
    let census: std::collections::BTreeMap<_, _> = ir
        .declaration_source_refs()
        .into_iter()
        .chain(sources)
        .map(|source| {
            (
                crate::canonical_json::to_string(
                    &serde_json::to_value(source).expect("source reference serializes"),
                ),
                source.clone(),
            )
        })
        .collect();
    ir.sources = census.into_values().collect();
    validation::finish_ir(ir)
}

pub(crate) fn validate_catalog_commitment(ir: &CodegenIr) -> Result<(), CodegenDiagnostic> {
    let catalog: ToolCatalog = serde_json::from_str(&ir.catalog_canonical_json).map_err(|_| {
        validation::diagnostic(CodegenDiagnosticCode::InvalidIr, "/catalog_canonical_json")
    })?;
    let prepared = PreparedToolCatalog::prepare(catalog).map_err(|_| {
        validation::diagnostic(CodegenDiagnosticCode::InvalidIr, "/catalog_canonical_json")
    })?;
    let selected: Vec<_> = ir
        .operations
        .iter()
        .map(|operation| operation.name.clone())
        .collect();
    let expected = compile_catalog_with_sources(&prepared, &selected, &ir.sources)?;
    if expected.ir() != ir {
        return Err(validation::diagnostic(CodegenDiagnosticCode::InvalidIr, ""));
    }
    Ok(())
}

fn uri(name: &str, phase: &str) -> String {
    format!(
        "https://harn.invalid/codegen/operations/{}/{phase}",
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC)
    )
}

fn wire_error(operation: &str, failure: WireFailure) -> CodegenDiagnostic {
    CodegenDiagnostic {
        code: CodegenDiagnosticCode::UnsupportedWireType,
        operation: if operation.is_empty() {
            None
        } else {
            Some(operation.into())
        },
        type_id: failure.type_id,
        source: Some(Box::new(failure.source)),
        structural_path: failure.structural_path,
    }
}
