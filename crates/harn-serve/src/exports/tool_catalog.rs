//! Portable tool-contract projection for statically discovered exports.

use std::collections::BTreeMap;

use harn_vm::tool_registry::{
    ToolCatalog, ToolCatalogEntry, ToolCatalogSchemaVersion, ToolCliSpec, ToolExecution,
    ToolGovernance, ToolPresentationAnnotations, ToolRegistryInfo, ToolSource, ToolTaskSupport,
};

use super::{ExportCatalog, ExportedCallableKind};
use crate::DispatchError;

impl ExportCatalog {
    /// Project statically discovered exports into Harn's portable tool
    /// contract. This reads the parser/type-resolver output already owned by
    /// `ExportCatalog`; it never evaluates the module or constructs a runtime.
    pub fn tool_catalog(&self) -> Result<ToolCatalog, DispatchError> {
        let tools = self
            .functions
            .values()
            .map(|function| {
                if function.throws_type.is_some() && function.error_schema.is_none() {
                    return Err(DispatchError::Validation(format!(
                        "export {:?} declares a throws type that cannot be represented as JSON Schema",
                        function.name
                    )));
                }
                Ok(ToolCatalogEntry {
                    name: function.name.clone(),
                    title: function.title.clone(),
                    description: function.description.clone(),
                    input_schema: function.input_schema.clone(),
                    output_schema: function.output_schema.clone(),
                    error_schema: function.error_schema.clone(),
                    annotations: function
                        .annotations
                        .map(|annotations| ToolPresentationAnnotations {
                            title: None,
                            read_only_hint: annotations.read_only,
                            destructive_hint: annotations.destructive,
                            idempotent_hint: annotations.idempotent,
                            open_world_hint: annotations.open_world,
                        }),
                    icons: None,
                    execution: function.job.as_ref().map(|_| ToolExecution {
                        task_support: ToolTaskSupport::Optional,
                    }),
                    governance: ToolGovernance::default(),
                    cli: ToolCliSpec {
                        command: vec![function.name.clone()],
                        aliases: Vec::new(),
                        hidden: false,
                        arguments: BTreeMap::new(),
                    },
                    namespace: None,
                    defer_loading: false,
                    source: Some(ToolSource {
                        kind: "harn".to_string(),
                        id: Some(function.name.clone()),
                        binding: Some(BTreeMap::from([(
                            "callableKind".to_string(),
                            serde_json::json!(match &function.kind {
                                ExportedCallableKind::Function => "function",
                                ExportedCallableKind::Pipeline => "pipeline",
                            }),
                        )])),
                    }),
                    policy: None,
                    meta: None,
                })
            })
            .collect::<Result<Vec<_>, DispatchError>>()?;
        let catalog = ToolCatalog {
            schema_version: ToolCatalogSchemaVersion::V2,
            info: Some(ToolRegistryInfo {
                name: self
                    .script_path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("harn-exports")
                    .to_string(),
                version: None,
                description: self.instructions.clone(),
            }),
            cli: None,
            tools,
            components: None,
        };
        catalog.validate().map_err(DispatchError::Validation)?;
        Ok(catalog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_catalog_is_typed_and_does_not_execute_main() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contracts = dir.path().join("contracts.harn");
        let script = dir.path().join("offline.harn");
        std::fs::write(
            &contracts,
            concat!(
                "pub type Request = {kind: \"inspect\", hypothesis_id: string}\n",
                "pub type NotFound = {variant: \"NotFound\", message: string}\n",
                "pub type Forbidden = {variant: \"Forbidden\", scope: string}\n",
                "pub type InspectError = NotFound | Forbidden\n",
            ),
        )
        .expect("write contracts");
        std::fs::write(
            &script,
            r#"
import { InspectError, Request } from "./contracts"
fn main() { panic("tool schema --surface exports must never run main") }
/// Inspect a hypothesis
@annotations(readOnly: true, idempotent: true)
pub fn inspect(request: Request) -> Request throws InspectError { return request }
"#,
        )
        .expect("write script");

        let exports = ExportCatalog::from_path(&script).expect("static exports");
        let catalog = exports.tool_catalog().expect("portable tool catalog");
        assert_eq!(catalog.schema_version, ToolCatalogSchemaVersion::V2);
        assert_eq!(catalog.tools.len(), 1);
        let inspect = &catalog.tools[0];
        assert_eq!(inspect.name, "inspect");
        assert_eq!(inspect.title.as_deref(), Some("Inspect a hypothesis"));
        assert_eq!(
            inspect.input_schema["properties"]["request"]["properties"]["hypothesis_id"]["type"],
            "string"
        );
        assert_eq!(
            inspect.output_schema.as_ref().unwrap()["properties"]["kind"]["const"],
            "inspect"
        );
        let variants = inspect.error_schema.as_ref().unwrap()["anyOf"]
            .as_array()
            .expect("imported error union");
        assert_eq!(variants.len(), 2);
        let variants = variants
            .iter()
            .map(|variant| {
                variant["properties"]["variant"]["const"]
                    .as_str()
                    .expect("literal discriminant")
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            variants,
            std::collections::BTreeSet::from(["Forbidden", "NotFound"])
        );
        assert_eq!(
            inspect.annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
        assert!(!inspect.cli.hidden);
        assert!(!inspect.defer_loading);
    }

    #[test]
    fn result_error_parameter_does_not_imply_a_declared_throw() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("result.harn");
        std::fs::write(
            &script,
            r#"
pub type LookupError = {variant: "NotFound"}
pub fn lookup() -> Result<string, LookupError> {
  return Result.Ok("found")
}
"#,
        )
        .expect("write script");

        let exports = ExportCatalog::from_path(&script).expect("static exports");
        let catalog = exports.tool_catalog().expect("portable tool catalog");
        let lookup = catalog
            .tools
            .iter()
            .find(|tool| tool.name == "lookup")
            .expect("lookup tool");
        assert!(
            lookup.error_schema.is_none(),
            "Result<T, E> must not create errorSchema unless the callable declares throws E"
        );
    }

    #[test]
    fn unprojectable_declared_throw_fails_catalog_publication() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("callback-error.harn");
        std::fs::write(
            &script,
            r#"
pub fn lookup() -> string throws {variant: "CallbackFailed", callback: fn(int) -> int} {
  return "found"
}
"#,
        )
        .expect("write script");

        let exports = ExportCatalog::from_path(&script).expect("static exports");
        let error = exports
            .tool_catalog()
            .expect_err("function-valued errors have no portable schema");
        assert!(
            error
                .to_string()
                .contains("runtime-only Harn type \"closure\""),
            "{error}"
        );
    }
}
