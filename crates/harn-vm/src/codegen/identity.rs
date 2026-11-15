pub(crate) use crate::wire::identity::digest;
use serde_json::{json, Value};

use super::{CodegenDiagnostic, CodegenDiagnosticCode, CodegenIdentity, CodegenIr};

/// Catalog, executable binding and complete IR are separate commitments.
pub(crate) fn expected_identity(ir: &CodegenIr) -> Result<CodegenIdentity, CodegenDiagnostic> {
    let catalog: Value = serde_json::from_str(&ir.catalog_canonical_json)
        .map_err(|_| CodegenDiagnostic::new(CodegenDiagnosticCode::InvalidIr))?;
    if crate::canonical_json::to_string(&catalog) != ir.catalog_canonical_json {
        return Err(CodegenDiagnostic::new(CodegenDiagnosticCode::InvalidDigest));
    }
    let catalog_digest = digest(&catalog);
    // Commit executable semantics, including opaque native discovery schemas.
    // Presentation and physical source extents do not alter the binding ABI.
    let mut binding = json!({"schema_version":ir.schema_version,
        "required_features":ir.required_features,"package":ir.package,
        "types":ir.types,"operations":ir.operations,"mcp_tools":ir.mcp_tools});
    binding["package"].as_object_mut().unwrap().remove("source");
    for ty in binding["types"].as_array_mut().unwrap() {
        remove_declaration_documentation(ty);
        if let Some(fields) = ty["shape"]["fields"].as_array_mut() {
            fields.iter_mut().for_each(remove_declaration_documentation);
        }
        if let Some(variants) = ty["shape"]["variants"].as_array_mut() {
            for variant in variants {
                remove_declaration_documentation(variant);
                variant["fields"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .for_each(remove_declaration_documentation);
            }
        }
    }
    for operation in binding["operations"].as_array_mut().unwrap() {
        remove_declaration_documentation(operation);
        operation["binding"]
            .as_object_mut()
            .unwrap()
            .remove("source");
    }
    for tool in binding["mcp_tools"].as_array_mut().unwrap() {
        let object = tool
            .as_object_mut()
            .ok_or_else(|| CodegenDiagnostic::new(CodegenDiagnosticCode::InvalidMcpProjection))?;
        for key in ["title", "description", "icons"] {
            object.remove(key);
        }
        for key in ["inputSchema", "outputSchema"] {
            if let Some(schema) = object.get_mut(key) {
                remove_schema_documentation(schema)?;
            }
        }
        if let Some(schema) = object
            .get_mut("_meta")
            .and_then(|meta| meta.get_mut(crate::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY))
            .and_then(|contract| contract.get_mut("errorSchema"))
        {
            remove_schema_documentation(schema)?;
        }
    }
    let binding_digest = digest(&binding);
    let mut complete = serde_json::to_value(ir).expect("closed IR serializes");
    complete["identity"] = json!({"catalog_digest":catalog_digest,
        "binding_digest":binding_digest,"ir_digest":""});
    Ok(CodegenIdentity {
        catalog_digest,
        binding_digest,
        ir_digest: digest(&complete),
    })
}

fn remove_declaration_documentation(value: &mut Value) {
    let object = value
        .as_object_mut()
        .expect("typed declaration serializes as an object");
    object.remove("source");
    object.remove("documentation");
}

fn remove_schema_documentation(value: &mut Value) -> Result<(), CodegenDiagnostic> {
    crate::tool_registry::transform_codegen_schema(value, &mut |object| {
        for name in [
            "description",
            "title",
            "examples",
            "$comment",
            "x-harn-source",
        ] {
            object.remove(name);
        }
        for name in ["x-harn-field", "x-harn-variant"] {
            if let Some(value) = object.get_mut(name) {
                remove_declaration_documentation(value);
            }
        }
        Ok::<(), CodegenDiagnostic>(())
    })
}
