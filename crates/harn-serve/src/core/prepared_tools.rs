//! Statically prepared tool surfaces shared by every dispatch adapter.

use std::path::Path;

use harn_vm::tool_registry::ToolCatalog;

use super::DispatchCore;
use crate::{DispatchError, ExportCatalog};

#[derive(Debug)]
pub(super) struct PreparedTools {
    exports: ExportCatalog,
    contract: ToolCatalog,
    mcp: Vec<serde_json::Value>,
}

impl PreparedTools {
    pub(super) fn prepare(script_path: &Path) -> Result<Self, DispatchError> {
        Self::from_catalog(ExportCatalog::from_path(script_path)?)
    }

    fn from_catalog(exports: ExportCatalog) -> Result<Self, DispatchError> {
        let contract = exports.tool_catalog()?;
        let mcp = contract
            .mcp_tools()
            .map_err(|error| DispatchError::Validation(error.to_string()))?;
        Ok(Self {
            exports,
            contract,
            mcp,
        })
    }

    pub(super) fn exports(&self) -> &ExportCatalog {
        &self.exports
    }
}

impl DispatchCore {
    pub fn catalog(&self) -> &ExportCatalog {
        self.tools.exports()
    }

    pub fn tool_catalog(&self) -> &ToolCatalog {
        &self.tools.contract
    }

    pub(crate) fn mcp_tools(&self) -> &[serde_json::Value] {
        &self.tools.mcp
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
