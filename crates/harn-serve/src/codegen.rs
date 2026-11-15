use std::path::Path;

use harn_vm::codegen::{CodegenDiagnostic, CodegenDiagnosticCode, ValidatedCodegenIr};
use harn_vm::tool_registry::PreparedToolCatalog;
use harn_vm::wire::WireSourceRef;

/// Compile the public exported surface through the ordinary catalog owner.
pub fn compile_codegen_ir(
    path: &Path,
    operations: &[String],
) -> Result<ValidatedCodegenIr, CodegenDiagnostic> {
    compile_codegen_ir_with_sources(path, operations).map(|(ir, _)| ir)
}

/// Compile portable data and retain its original files only as host authority.
pub fn compile_codegen_ir_with_sources(
    path: &Path,
    operations: &[String],
) -> Result<(ValidatedCodegenIr, harn_modules::SourceDocuments), CodegenDiagnostic> {
    let exports = crate::ExportCatalog::from_path(path).map_err(|_| CodegenDiagnostic {
        source: Some(Box::new(WireSourceRef::DocumentPointer {
            document: path.to_string_lossy().into_owned(),
            pointer: String::new(),
        })),
        ..CodegenDiagnostic::new(CodegenDiagnosticCode::SourceUnavailable)
    })?;
    let catalog = exports
        .tool_catalog()
        .map_err(|_| CodegenDiagnostic::new(CodegenDiagnosticCode::InvalidIr))?;
    let catalog = harn_vm::codegen::normalize_catalog_sources(catalog, exports.source_documents())?;
    let prepared = PreparedToolCatalog::prepare(catalog)
        .map_err(|_| CodegenDiagnostic::new(CodegenDiagnosticCode::InvalidIr))?;
    let sources = codegen_source_refs_for_documents(exports.source_documents())?;
    let ir = harn_vm::codegen::compile_catalog_with_sources(&prepared, operations, &sources)?;
    Ok((ir, exports.source_documents().clone()))
}

/// Logical references follow the complete graph retained by export loading.
pub fn codegen_source_refs(path: &Path) -> Result<Vec<WireSourceRef>, CodegenDiagnostic> {
    let exports = crate::ExportCatalog::from_path(path)
        .map_err(|_| CodegenDiagnostic::new(CodegenDiagnosticCode::SourceUnavailable))?;
    codegen_source_refs_for_documents(exports.source_documents())
}

/// Complete source spans for either static exports or an executed registry.
pub fn codegen_source_refs_for_documents(
    documents: &harn_modules::SourceDocuments,
) -> Result<Vec<WireSourceRef>, CodegenDiagnostic> {
    documents
        .physical_paths()
        .map(|file| {
            let end = std::fs::metadata(file)
                .ok()
                .filter(|metadata| metadata.is_file())
                .and_then(|metadata| u32::try_from(metadata.len()).ok())
                .ok_or_else(|| CodegenDiagnostic::new(CodegenDiagnosticCode::SourceUnavailable))?;
            Ok(WireSourceRef::TextSpan {
                document: documents.document_for(file).expect("graph source").into(),
                start: 0,
                end,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_pathological_fixture_compiles_through_the_export_owner() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codegen/pathological/server.harn");
        let exports = crate::ExportCatalog::from_path(&path).unwrap();
        let catalog = exports.tool_catalog().expect("valid public catalog");
        PreparedToolCatalog::prepare(catalog).expect("all schemas and codecs prepare");
        let (ir, documents) = compile_codegen_ir_with_sources(&path, &[]).unwrap();
        assert_eq!(ir.ir().operations.len(), 4);
        assert_eq!(documents.physical_paths().count(), 4);
        let details: std::collections::BTreeSet<_> = ir
            .ir()
            .types
            .iter()
            .filter_map(|ty| match &ty.shape {
                harn_vm::wire::WireShape::Record {
                    nominal: Some(nominal),
                    ..
                } if nominal.name == "Detail" => Some(&nominal.declaration_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            details.len(),
            2,
            "same-named private declarations retain their defining modules"
        );
        for feature in [
            harn_vm::codegen::CodegenFeature::Base64Bytes,
            harn_vm::codegen::CodegenFeature::CanonicalDecimal,
            harn_vm::codegen::CodegenFeature::Sets,
            harn_vm::codegen::CodegenFeature::Tuples,
            harn_vm::codegen::CodegenFeature::NominalTypes,
            harn_vm::codegen::CodegenFeature::RecursiveTypes,
            harn_vm::codegen::CodegenFeature::DeclaredApplicationErrors,
            harn_vm::codegen::CodegenFeature::McpTasks,
        ] {
            assert!(
                ir.ir().required_features.contains(&feature),
                "fixture reaches {feature:?}"
            );
        }
        harn_vm::codegen::validate_ir(&ir.canonical_json()).unwrap();
    }

    #[test]
    fn relocated_exports_keep_all_digests_and_only_private_source_paths_change() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let make = |root: &Path| {
            std::fs::write(root.join("types.harn"), "pub struct Item { value: int }").unwrap();
            // This import is not needed by a type or the exported body. Its file
            // must still be protected from the generator's source read authority.
            std::fs::write(
                root.join("deferred.harn"),
                "pub fn unused() -> int { return 1 }",
            )
            .unwrap();
            let path = root.join("server.harn");
            std::fs::write(&path, "import { Item } from \"./types\"\nimport { unused } from \"./deferred\"\npub fn echo(input: Item) -> Item { return input }").unwrap();
            compile_codegen_ir_with_sources(&path, &[]).unwrap()
        };
        let (left, left_sources) = make(first.path());
        let (right, right_sources) = make(second.path());
        assert_eq!(left.ir().identity, right.ir().identity);
        assert_eq!(left.canonical_json(), right.canonical_json());
        let text = String::from_utf8(left.canonical_json()).unwrap();
        assert!(!text.contains(first.path().to_str().unwrap()));
        assert!(!text.contains(second.path().to_str().unwrap()));
        assert_eq!(left_sources.physical_paths().count(), 3);
        assert_eq!(right_sources.physical_paths().count(), 3);
        let document = left_sources
            .document_for(&first.path().join("deferred.harn").canonicalize().unwrap())
            .unwrap();
        assert!(left.ir().sources.iter().any(|source| matches!(source,
            WireSourceRef::TextSpan { document: found, .. } if found == document)));
        assert_ne!(
            left_sources.resolve(document),
            right_sources.resolve(document)
        );
        harn_vm::codegen::validate_ir(&left.canonical_json()).unwrap();
    }
}
