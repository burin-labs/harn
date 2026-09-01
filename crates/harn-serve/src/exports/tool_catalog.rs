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
            .map(|function| ToolCatalogEntry {
                name: function.name.clone(),
                title: function.title.clone(),
                description: function.description.clone(),
                input_schema: function.input_schema.clone(),
                output_schema: function.output_schema.clone(),
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
            .collect();
        let catalog = ToolCatalog {
            schema_version: ToolCatalogSchemaVersion::V1,
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
            "pub type Request = {kind: \"inspect\", hypothesis_id: string}\n",
        )
        .expect("write contracts");
        std::fs::write(
            &script,
            r#"
import { Request } from "./contracts"
fn main() { panic("tool schema --surface exports must never run main") }
/// Inspect a hypothesis
@annotations(readOnly: true, idempotent: true)
pub fn inspect(request: Request) -> Request { return request }
"#,
        )
        .expect("write script");

        let exports = ExportCatalog::from_path(&script).expect("static exports");
        let catalog = exports.tool_catalog().expect("portable tool catalog");
        assert_eq!(catalog.schema_version, ToolCatalogSchemaVersion::V1);
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
        assert_eq!(
            inspect.annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
        assert!(!inspect.cli.hidden);
        assert!(!inspect.defer_loading);
    }
}
