//! Public JSON projection for exported Harn callables.
//!
//! The module graph owns visible imported types, while the VM bridge owns
//! authority injection. This seam combines those contracts once so every
//! transport advertises the same caller-owned schema.

use std::path::Path;

use harn_parser::{SNode, TypeExpr, TypedParam};

use super::ExportedParam;

/// Whether a declared type expression accepts a JSON object value: a bare
/// `dict`, an inline object shape, a `DictType`, or any union/intersection
/// member that does. Backs [`ExportedParam::accepts_json_object`].
pub fn type_expr_accepts_json_object(type_expr: &TypeExpr) -> bool {
    match type_expr {
        TypeExpr::Named(name) => name == "dict",
        TypeExpr::Shape(_) | TypeExpr::DictType(_, _) => true,
        TypeExpr::Union(types) | TypeExpr::Intersection(types) => {
            types.iter().any(type_expr_accepts_json_object)
        }
        _ => false,
    }
}

pub(super) fn resolver_for_module(
    path: &Path,
    program: &[SNode],
    source_len: usize,
) -> Result<(harn_vm::TypeSchemaResolver, harn_modules::SourceDocuments), crate::DispatchError> {
    use harn_vm::wire::{
        declaration_name, ResolvedWireDeclaration, ResolvedWireScope, WireSourceRef,
    };
    use std::collections::BTreeMap;
    let path = std::fs::canonicalize(path).map_err(|_| {
        crate::DispatchError::Validation("export source identity is unavailable".into())
    })?;
    let module_graph = harn_modules::build(std::slice::from_ref(&path));
    let documents = module_graph.source_documents(&path).map_err(|_| {
        crate::DispatchError::Validation("export source identity is unavailable".into())
    })?;
    let mut sites = module_graph
        .imported_type_declaration_sites_for_file(&path)
        .ok_or_else(|| {
            crate::DispatchError::Validation("export type imports could not be resolved".into())
        })?;
    sites.extend(
        program
            .iter()
            .filter(|node| declaration_name(node).is_some())
            .map(|declaration| harn_modules::LocatedTypeDeclaration {
                file: path.clone(),
                declaration: declaration.clone(),
            }),
    );
    let identity = |file: &Path, name: &str| {
        let module = documents
            .document_for(file)
            .expect("resolved graph declaration");
        format!("{module}::{name}")
    };
    let mut root_bindings = BTreeMap::new();
    let mut declarations = BTreeMap::new();
    for site in &sites {
        let Some(name) = declaration_name(&site.declaration) else {
            continue;
        };
        if let Some(visible) = module_graph.type_declaration_visible_from(&path, name) {
            root_bindings.insert(name.to_owned(), identity(&visible.file, name));
        }
        let id = identity(&site.file, name);
        let (_, node) = harn_parser::peel_attributes(&site.declaration);
        let source = if site.file.is_absolute() {
            WireSourceRef::TextSpan {
                document: documents
                    .document_for(&site.file)
                    .expect("graph source")
                    .into(),
                start: node.span.start.try_into().map_err(|_| {
                    crate::DispatchError::Validation("export source extent is too large".into())
                })?,
                end: node.span.end.try_into().map_err(|_| {
                    crate::DispatchError::Validation("export source extent is too large".into())
                })?,
            }
        } else {
            WireSourceRef::Synthetic {
                owner: documents
                    .document_for(&site.file)
                    .expect("graph source")
                    .into(),
            }
        };
        let bindings = module_graph
            .type_declaration_bindings(site)
            .into_iter()
            .map(|(name, file)| {
                let id = identity(&file, &name);
                (name, id)
            })
            .collect();
        declarations.insert(
            id.clone(),
            ResolvedWireDeclaration {
                id,
                source,
                declaration: site.declaration.clone(),
                bindings,
            },
        );
    }
    let resolver = harn_vm::TypeSchemaResolver::from_resolved_wire_scope(ResolvedWireScope {
        source: WireSourceRef::TextSpan {
            document: documents.document_for(&path).expect("entry source").into(),
            start: 0,
            end: source_len.try_into().map_err(|_| {
                crate::DispatchError::Validation("export source extent is too large".into())
            })?,
        },
        declarations: declarations.into_values().collect(),
        bindings: root_bindings,
    })
    .map_err(|error| crate::DispatchError::Validation(error.to_string()))?;
    Ok((resolver, documents))
}

pub(super) fn public_params(params: &[TypedParam]) -> &[TypedParam] {
    &params[harn_vm::leading_authority_param_count(params)..]
}

pub(super) fn exported_params(
    params: &[TypedParam],
    resolver: &harn_vm::TypeSchemaResolver,
) -> Vec<ExportedParam> {
    params
        .iter()
        .map(|param| {
            let item_schema = param
                .type_expr
                .as_ref()
                .and_then(|type_expr| resolver.json_schema_for_input_type_expr(type_expr))
                .unwrap_or_else(|| serde_json::json!({}));
            ExportedParam {
                name: param.name.clone(),
                type_expr: param.type_expr.clone(),
                input_schema: if param.rest {
                    serde_json::json!({"type": "array", "items": item_schema})
                } else {
                    item_schema
                },
                has_default: param.default_value.is_some(),
                rest: param.rest,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::ExportCatalog;

    fn catalog_from_source(source: &str) -> ExportCatalog {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(&path, source).expect("write script");
        ExportCatalog::from_path(&path).expect("catalog")
    }

    #[test]
    fn omits_host_injected_authority_from_public_schema() {
        let catalog = catalog_from_source(
            r"
pub fn inspect(harness: Harness, hypothesis_id: string) -> string {
  return hypothesis_id
}
pub fn dispatch(postgres: HarnessPostgres, process: HarnessProcess, input: dict) -> dict {
  return input
}
",
        );

        let inspect = catalog.function("inspect").expect("inspect export");
        assert_eq!(inspect.params.len(), 1);
        assert_eq!(inspect.params[0].name, "hypothesis_id");
        assert!(inspect.input_schema["properties"].get("harness").is_none());

        let dispatch = catalog.function("dispatch").expect("dispatch export");
        assert_eq!(dispatch.params.len(), 1);
        assert_eq!(dispatch.params[0].name, "input");
        assert!(dispatch.input_schema["properties"]
            .get("postgres")
            .is_none());
        assert!(dispatch.input_schema["properties"].get("process").is_none());
    }

    #[test]
    fn projects_imported_type_aliases_structurally() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contracts = dir.path().join("contracts.harn");
        let server = dir.path().join("server.harn");
        std::fs::write(
            &contracts,
            "pub type Request = {kind: \"inspect\", hypothesis_id: string}\n",
        )
        .expect("write contracts");
        std::fs::write(
            &server,
            r#"
import { Request } from "./contracts"
pub fn inspect(harness: Harness, request: Request) -> Request { return request }
"#,
        )
        .expect("write server");

        let catalog = ExportCatalog::from_path(&server).expect("catalog");
        let inspect = catalog.function("inspect").expect("inspect export");
        assert_eq!(
            inspect.input_schema["properties"]["request"]["properties"]["hypothesis_id"]["type"],
            "string"
        );
        assert_eq!(
            inspect.output_schema.as_ref().expect("output")["properties"]["kind"]["const"],
            "inspect"
        );
    }

    #[test]
    fn projects_imported_nominal_and_generic_return_types_structurally() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contracts = dir.path().join("contracts.harn");
        let server = dir.path().join("server.harn");
        std::fs::write(
            &contracts,
            r"
pub struct Envelope<T> {
  value: T
  labels: list<string>
  scores?: dict<string, int>
}
pub enum Outcome<T> {
  Success(value: T)
  Failure(message: string)
}
pub type Result = Outcome<Envelope<string>> | nil
",
        )
        .expect("write contracts");
        std::fs::write(
            &server,
            r#"
import { Result } from "./contracts"
pub fn inspect() -> Result { return nil }
"#,
        )
        .expect("write server");

        let catalog = ExportCatalog::from_path(&server).expect("catalog");
        let schema = catalog
            .function("inspect")
            .expect("inspect export")
            .output_schema
            .as_ref()
            .expect("output schema");
        let outcome = &schema["anyOf"][0];
        let success = &outcome["oneOf"][0];
        let envelope = &success["properties"]["fields"]["prefixItems"][0];
        assert_eq!(success["properties"]["variant"]["const"], "Success");
        assert_eq!(envelope["properties"]["value"]["type"], "string");
        assert_eq!(envelope["properties"]["labels"]["items"]["type"], "string");
        assert_eq!(
            envelope["properties"]["scores"]["anyOf"][0]["additionalProperties"]["type"],
            "integer"
        );
        assert_eq!(schema["anyOf"][1]["type"], "null");
    }
}
