//! Runtime-owned process filesystem-confinement facts for `harn doctor`.

use harn_vm::process_sandbox::enforcement::{active_enforcement, Enforcement};
use harn_vm::process_sandbox::{
    active_backend_filesystem_available, active_backend_filesystem_mechanism, active_backend_name,
};
use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProcessSandboxInfo {
    pub backend: String,
    pub filesystem_mechanism: String,
    pub active: bool,
    /// What the backend holds a confined child to, per dimension, from the
    /// runtime's enforcement table. Empty on a platform with no backend.
    pub enforcement: BTreeMap<&'static str, Enforcement>,
    /// The same cells as one line, or `None` with no backend.
    pub enforcement_receipt: Option<String>,
}

impl ProcessSandboxInfo {
    /// The one-line doctor check detail.
    pub(super) fn detail(&self) -> String {
        format!(
            "backend={} filesystem_mechanism={} active={} enforcement=[{}]",
            self.backend,
            self.filesystem_mechanism,
            self.active,
            self.enforcement_receipt
                .as_deref()
                .unwrap_or("no process sandbox")
        )
    }
}

pub(super) fn process_sandbox_info() -> ProcessSandboxInfo {
    ProcessSandboxInfo {
        backend: active_backend_name().to_string(),
        filesystem_mechanism: active_backend_filesystem_mechanism().to_string(),
        active: active_backend_filesystem_available(),
        enforcement: active_enforcement()
            .map(|row| {
                row.cells()
                    .map(|(dimension, cell)| (dimension.as_str(), cell))
                    .collect()
            })
            .unwrap_or_default(),
        enforcement_receipt: active_enforcement().map(|row| row.receipt()),
    }
}
