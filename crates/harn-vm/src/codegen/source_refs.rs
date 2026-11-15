//! Project compiler provenance without granting portable data file authority.

use std::path::{Path, PathBuf};

use harn_modules::SourceDocuments;

use super::{CodegenDiagnostic, CodegenDiagnosticCode};
use crate::tool_registry::ToolCatalog;
use crate::wire::metadata::{FieldOrigin, VariantOrigin};
use crate::wire::WireSourceRef;

/// Normalize only typed compiler provenance against the host's resolved map.
/// Arbitrary schema values, protocol bindings and application data are opaque.
pub fn normalize_catalog_sources(
    mut catalog: ToolCatalog,
    documents: &SourceDocuments,
) -> Result<ToolCatalog, CodegenDiagnostic> {
    for tool in &mut catalog.tools {
        if let Some(source) = &mut tool.source {
            if let Some(location) = &mut source.location {
                normalize(location, documents)?;
            }
        }
        for schema in std::iter::once(&mut tool.input_schema)
            .chain(tool.output_schema.iter_mut())
            .chain(tool.error_schema.iter_mut())
        {
            normalize_schema(schema, documents)?;
        }
    }
    if let Some(components) = &mut catalog.components {
        for schema in components.schemas.values_mut() {
            normalize_schema(schema, documents)?;
        }
    }
    Ok(catalog)
}

fn normalize_schema(
    schema: &mut serde_json::Value,
    documents: &SourceDocuments,
) -> Result<(), CodegenDiagnostic> {
    crate::tool_registry::transform_codegen_schema(schema, &mut |object| {
        if let Some(value) = object.get_mut("x-harn-source") {
            let mut source: WireSourceRef =
                serde_json::from_value(value.clone()).map_err(|_| invalid())?;
            normalize(&mut source, documents)?;
            *value = serde_json::to_value(source).expect("typed source serializes");
        }
        if let Some(value) = object.get_mut("x-harn-field") {
            let mut field: FieldOrigin =
                serde_json::from_value(value.clone()).map_err(|_| invalid())?;
            normalize(&mut field.source, documents)?;
            *value = serde_json::to_value(field).expect("typed field serializes");
        }
        if let Some(value) = object.get_mut("x-harn-variant") {
            let mut variant: VariantOrigin =
                serde_json::from_value(value.clone()).map_err(|_| invalid())?;
            normalize(&mut variant.source, documents)?;
            *value = serde_json::to_value(variant).expect("typed variant serializes");
        }
        Ok(())
    })
}

fn normalize(
    source: &mut WireSourceRef,
    documents: &SourceDocuments,
) -> Result<(), CodegenDiagnostic> {
    let document = match source {
        WireSourceRef::TextSpan { document, .. }
        | WireSourceRef::DocumentPointer { document, .. } => document,
        WireSourceRef::Synthetic { .. } => return Ok(()),
    };
    if documents.resolve(document).is_some() {
        return Ok(());
    }
    let path = if Path::new(document).is_absolute() {
        Some(PathBuf::from(document.as_str()))
    } else {
        let uri = url::Url::parse(document).map_err(|_| unavailable())?;
        match uri.scheme() {
            "file" => Some(uri.to_file_path().map_err(|()| unavailable())?),
            // These are local logical documents. An unknown name cannot borrow
            // the physical authority of a different compilation context.
            "harn"
                if document.starts_with("harn:source/")
                    || document.starts_with("harn:package/") =>
            {
                return Err(unavailable())
            }
            scheme if scheme.len() == 1 => return Err(unavailable()),
            _ => None,
        }
    };
    if let Some(path) = path {
        *document = documents
            .document_for(&path)
            .ok_or_else(unavailable)?
            .into();
    }
    Ok(())
}

fn invalid() -> CodegenDiagnostic {
    CodegenDiagnostic::new(CodegenDiagnosticCode::InvalidIr)
}

fn unavailable() -> CodegenDiagnostic {
    CodegenDiagnostic::new(CodegenDiagnosticCode::SourceUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_source_normalization_never_walks_application_fields_or_resolves_new_paths() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.harn");
        std::fs::write(&path, "pub fn main() -> int { return 1 }").unwrap();
        let path = path.canonicalize().unwrap();
        let graph = harn_modules::build(std::slice::from_ref(&path));
        let documents = graph.source_documents(&path).unwrap();
        let mut catalog = super::super::tests::catalog("source normalization")
            .catalog()
            .clone();
        let source = WireSourceRef::TextSpan {
            document: path.to_string_lossy().into_owned(),
            start: 0,
            end: 1,
        };
        catalog.tools[0].input_schema["x-harn-source"] = json!(source);
        catalog.tools[0].input_schema["properties"]["x-harn-source"] =
            json!({"const":path.to_string_lossy()});
        let normalized = normalize_catalog_sources(catalog.clone(), &documents).unwrap();
        assert_eq!(
            normalized.tools[0].input_schema["x-harn-source"]["document"],
            documents.document_for(&path).unwrap()
        );
        assert_eq!(
            normalized.tools[0].input_schema["properties"]["x-harn-source"],
            catalog.tools[0].input_schema["properties"]["x-harn-source"]
        );
        catalog.tools[0].input_schema["x-harn-source"]["document"] =
            json!(root.path().join("unobserved.harn"));
        assert_eq!(
            normalize_catalog_sources(catalog.clone(), &documents)
                .unwrap_err()
                .code,
            CodegenDiagnosticCode::SourceUnavailable
        );
        catalog.tools[0].input_schema["x-harn-source"]["document"] = json!("main.harn");
        assert!(normalize_catalog_sources(catalog, &documents).is_err());
    }
}
