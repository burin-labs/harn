use std::collections::BTreeSet;

use super::*;
use crate::wire::{PreparedWireGraph, WireShape, WireSourceRef};

const MAX_IR_BYTES: usize = 64 * 1024 * 1024;

/// Validate an untrusted standalone IR before any backend process starts.
pub fn validate_ir(bytes: &[u8]) -> Result<ValidatedCodegenIr, CodegenDiagnostic> {
    if bytes.len() > MAX_IR_BYTES {
        return Err(diagnostic(CodegenDiagnosticCode::InvalidIr, ""));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| diagnostic(CodegenDiagnosticCode::InvalidJson, ""))?;
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        != Some("harn-codegen/1.0")
    {
        return Err(diagnostic(
            CodegenDiagnosticCode::UnsupportedVersion,
            "/schema_version",
        ));
    }
    let ir: CodegenIr = serde_json::from_value(value)
        .map_err(|_| diagnostic(CodegenDiagnosticCode::InvalidIr, ""))?;
    validate_structure(&ir)?;
    if super::identity::expected_identity(&ir)? != ir.identity {
        return Err(diagnostic(
            CodegenDiagnosticCode::InvalidDigest,
            "/identity",
        ));
    }
    super::compiler::validate_catalog_commitment(&ir)?;
    Ok(ValidatedCodegenIr { ir })
}

pub(crate) fn finish_ir(mut ir: CodegenIr) -> Result<ValidatedCodegenIr, CodegenDiagnostic> {
    validate_structure(&ir)?;
    ir.identity = super::identity::expected_identity(&ir)?;
    Ok(ValidatedCodegenIr { ir })
}

fn validate_structure(ir: &CodegenIr) -> Result<(), CodegenDiagnostic> {
    source(&ir.package.source)?;
    for reference in &ir.sources {
        source(reference)?;
    }
    if ir.package.id.is_empty() || ir.package.name.is_empty() || ir.operations.is_empty() {
        return Err(diagnostic(CodegenDiagnosticCode::InvalidIr, "/package"));
    }
    let graph =
        PreparedWireGraph::prepare(ir.types.clone()).map_err(|error| CodegenDiagnostic {
            code: CodegenDiagnosticCode::UnsupportedWireType,
            operation: None,
            type_id: error.type_id,
            source: Some(Box::new(error.source)),
            structural_path: error.structural_path,
        })?;
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut used = BTreeSet::new();
    for operation in &ir.operations {
        if operation.id.is_empty()
            || operation.name.is_empty()
            || operation.binding.operation.is_empty()
            || !ids.insert(&operation.id)
            || !names.insert(&operation.name)
        {
            return Err(diagnostic(
                CodegenDiagnosticCode::DuplicateIdentity,
                "/operations",
            ));
        }
        source(&operation.source)?;
        source(&operation.binding.source)?;
        let input = graph
            .get(&operation.input)
            .ok_or_else(|| diagnostic(CodegenDiagnosticCode::DanglingType, "/operations/input"))?;
        if !matches!(
            graph
                .unaliased(&input.id)
                .map_err(|_| diagnostic(CodegenDiagnosticCode::InvalidIr, "/operations/input"))?
                .shape,
            WireShape::Record { .. }
        ) {
            return Err(diagnostic(
                CodegenDiagnosticCode::InvalidIr,
                "/operations/input",
            ));
        }
        used.insert(operation.input.clone());
        let CodegenOutput::Value { type_id, .. } = &operation.output else {
            return Err(CodegenDiagnostic {
                operation: Some(operation.id.clone()),
                source: Some(Box::new(operation.source.clone())),
                ..diagnostic(CodegenDiagnosticCode::MissingOutput, "/operations/output")
            });
        };
        used.insert(type_id.clone());
        used.extend(operation.application_error.iter().cloned());
    }
    let mut pending: Vec<_> = used.iter().cloned().collect();
    while let Some(id) = pending.pop() {
        let ty = graph
            .get(&id)
            .ok_or_else(|| diagnostic(CodegenDiagnosticCode::DanglingType, "/operations"))?;
        for edge in crate::wire::graph::references(&ty.shape) {
            if used.insert(edge.clone()) {
                pending.push(edge.clone());
            }
        }
    }
    if used.len() != ir.types.len() {
        return Err(diagnostic(CodegenDiagnosticCode::InvalidIr, "/types"));
    }
    let expected = required_features(&graph, &ir.operations);
    if ir.required_features != expected.into_iter().collect::<Vec<_>>() {
        return Err(diagnostic(
            CodegenDiagnosticCode::UnsupportedFeature,
            "/required_features",
        ));
    }
    Ok(())
}

pub(crate) fn required_features(
    graph: &PreparedWireGraph,
    operations: &[CodegenOperation],
) -> BTreeSet<CodegenFeature> {
    let mut features = BTreeSet::from([CodegenFeature::RequestCancellation]);
    for ty in graph.types() {
        match &ty.shape {
            WireShape::Integer { .. }
            | WireShape::Literal {
                value: crate::wire::WireLiteral::Integer { .. },
            } => {
                features.insert(CodegenFeature::ExactJsonInteger);
            }
            WireShape::Bytes { .. } => {
                features.insert(CodegenFeature::Base64Bytes);
            }
            WireShape::Decimal { .. } => {
                features.insert(CodegenFeature::CanonicalDecimal);
            }
            WireShape::Set { .. } => {
                features.insert(CodegenFeature::Sets);
            }
            WireShape::Tuple { .. } => {
                features.insert(CodegenFeature::Tuples);
            }
            WireShape::Map { .. } => {
                features.insert(CodegenFeature::StringMaps);
            }
            WireShape::Record {
                fields, nominal, ..
            } => {
                if nominal.is_some() {
                    features.insert(CodegenFeature::NominalTypes);
                }
                if fields.iter().any(|field| field.optional) {
                    features.insert(CodegenFeature::OptionalFields);
                }
            }
            WireShape::Enum { .. } => {
                features.insert(CodegenFeature::NominalTypes);
            }
            WireShape::Union { .. } => {
                features.insert(CodegenFeature::DisjointUnions);
            }
            _ => {}
        }
    }
    if graph.has_recursive_types() {
        features.insert(CodegenFeature::RecursiveTypes);
    }
    if operations
        .iter()
        .any(|operation| operation.application_error.is_some())
    {
        features.insert(CodegenFeature::DeclaredApplicationErrors);
    }
    if operations.iter().any(|operation| {
        operation.execution.task_support != crate::tool_registry::ToolTaskSupport::Forbidden
    }) {
        features.insert(CodegenFeature::McpTasks);
    }
    features
}

/// Response capability and identity checks precede filesystem path handling.
pub fn validate_response(
    request: &CodegenRequest,
    response: CodegenResponse,
) -> Result<Vec<GeneratedArtifact>, CodegenDiagnostic> {
    if response.identity != request.ir.identity {
        return Err(diagnostic(
            CodegenDiagnosticCode::ResponseIdentityMismatch,
            "/identity",
        ));
    }
    let supported: BTreeSet<_> = response.supported_features.iter().copied().collect();
    if supported.len() != response.supported_features.len()
        || request
            .ir
            .required_features
            .iter()
            .any(|feature| !supported.contains(feature))
    {
        return Err(diagnostic(
            CodegenDiagnosticCode::UnsupportedFeature,
            "/supported_features",
        ));
    }
    if response.artifacts.is_empty() {
        return Err(diagnostic(
            CodegenDiagnosticCode::MalformedPluginResponse,
            "/artifacts",
        ));
    }
    let sources = request.ir.source_refs();
    for artifact in &response.artifacts {
        for mapping in &artifact.source_mappings {
            if mapping.start > mapping.end
                || mapping.end as usize > artifact.content.len()
                || !artifact.content.is_char_boundary(mapping.start as usize)
                || !artifact.content.is_char_boundary(mapping.end as usize)
            {
                return Err(diagnostic(
                    CodegenDiagnosticCode::InvalidSourceMapping,
                    "/artifacts/source_mappings",
                ));
            }
            source(&mapping.source)?;
            if !sources.contains(&&mapping.source) {
                return Err(diagnostic(
                    CodegenDiagnosticCode::InvalidSourceMapping,
                    "/artifacts/source_mappings",
                ));
            }
        }
    }
    Ok(response.artifacts)
}

fn source(source: &WireSourceRef) -> Result<(), CodegenDiagnostic> {
    crate::wire::graph::validate_source(source)
        .map_err(|()| diagnostic(CodegenDiagnosticCode::InvalidIr, "/source"))
}

pub(crate) fn diagnostic(code: CodegenDiagnosticCode, path: &str) -> CodegenDiagnostic {
    CodegenDiagnostic {
        structural_path: path.into(),
        ..CodegenDiagnostic::new(code)
    }
}
