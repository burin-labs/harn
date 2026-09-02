//! Statically prepared tool surfaces shared by every dispatch adapter.

use std::path::Path;

use harn_vm::tool_registry::{PreparedToolCatalog, ToolCatalog};

use super::DispatchCore;
use crate::{DispatchError, ExportCatalog};

#[derive(Debug)]
pub(super) struct PreparedTools {
    exports: ExportCatalog,
    contract: PreparedToolCatalog,
}

impl PreparedTools {
    pub(super) fn prepare(script_path: &Path) -> Result<Self, DispatchError> {
        Self::from_catalog(ExportCatalog::from_path(script_path)?)
    }

    fn from_catalog(exports: ExportCatalog) -> Result<Self, DispatchError> {
        let contract = PreparedToolCatalog::prepare(exports.tool_catalog()?)
            .map_err(|error| DispatchError::Validation(error.to_string()))?;
        Ok(Self { exports, contract })
    }

    pub(super) fn exports(&self) -> &ExportCatalog {
        &self.exports
    }

    pub(super) fn classify_result(
        &self,
        tool: &str,
        result: Result<harn_vm::VmValue, harn_vm::VmError>,
    ) -> Result<(harn_vm::VmValue, serde_json::Value), DispatchError> {
        match harn_vm::tool_registry::classify_tool_result(&self.contract, tool, result) {
            Ok(harn_vm::tool_registry::ToolInvocationOutcome::Success { value, json }) => {
                Ok((value, json))
            }
            Ok(harn_vm::tool_registry::ToolInvocationOutcome::ApplicationError(error)) => {
                Err(DispatchError::Application(error))
            }
            Err(harn_vm::tool_registry::ToolInvocationError::Contract(error)) => {
                Err(DispatchError::Contract(error))
            }
            Err(harn_vm::tool_registry::ToolInvocationError::Runtime(error)) => {
                Err(super::classify_vm_error(error))
            }
        }
    }

    pub(super) fn classify_failure(&self, tool: &str, error: harn_vm::VmError) -> DispatchError {
        match harn_vm::tool_registry::classify_tool_failure(&self.contract, tool, error) {
            harn_vm::tool_registry::ToolFailureClassification::Application(error) => {
                DispatchError::Application(error)
            }
            harn_vm::tool_registry::ToolFailureClassification::Contract(error) => {
                DispatchError::Contract(error)
            }
            harn_vm::tool_registry::ToolFailureClassification::Runtime(error) => {
                super::classify_vm_error(error)
            }
        }
    }
}

impl DispatchCore {
    pub fn catalog(&self) -> &ExportCatalog {
        self.tools.exports()
    }

    pub fn tool_catalog(&self) -> &ToolCatalog {
        self.tools.contract.catalog()
    }

    pub(crate) fn mcp_tools(&self) -> &[serde_json::Value] {
        self.tools.contract.mcp_tools()
    }

    pub(crate) fn prepared_tool_catalog(&self) -> &PreparedToolCatalog {
        &self.tools.contract
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_preparation_rejects_an_invalid_tool_catalog_before_requests() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            "pub fn inspect(input: string) -> string { return input }\n",
        )
        .expect("write script");
        let mut exports = ExportCatalog::from_path(&script).expect("exports");
        exports
            .functions
            .get_mut("inspect")
            .expect("inspect export")
            .input_schema = serde_json::json!({});

        let error = PreparedTools::from_catalog(exports).expect_err("invalid catalog must fail");
        assert!(error.message().contains("inputSchema"));
    }
}
