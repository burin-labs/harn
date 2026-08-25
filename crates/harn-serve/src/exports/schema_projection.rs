//! Public JSON projection for exported Harn callables.
//!
//! The module graph owns visible imported types, while the VM bridge owns
//! authority injection. This seam combines those contracts once so every
//! transport advertises the same caller-owned schema.

use std::path::Path;

use harn_parser::{SNode, TypedParam};

use super::ExportedParam;

pub(super) fn resolver_for_module(path: &Path, program: &[SNode]) -> harn_vm::TypeSchemaResolver {
    let module_graph = harn_modules::build(&[path.to_path_buf()]);
    let mut schema_program = module_graph
        .imported_type_declarations_for_file(path)
        .unwrap_or_default();
    // Local declarations come last because the resolver uses declaration
    // order for shadowing, matching the module checker's visible scope.
    schema_program.extend_from_slice(program);
    harn_vm::TypeSchemaResolver::from_program(&schema_program)
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
        .map(|param| ExportedParam {
            name: param.name.clone(),
            type_expr: param.type_expr.clone(),
            input_schema: param
                .type_expr
                .as_ref()
                .and_then(|type_expr| resolver.json_schema_for_input_type_expr(type_expr))
                .unwrap_or_else(|| serde_json::json!({})),
            has_default: param.default_value.is_some(),
            rest: param.rest,
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
