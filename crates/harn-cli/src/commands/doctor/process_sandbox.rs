//! Runtime-owned process filesystem-confinement facts for `harn doctor`.

use harn_vm::process_sandbox::enforcement::{active_enforcement, Enforcement, MechanismLimit};
use harn_vm::process_sandbox::{
    active_backend_filesystem_available, active_backend_filesystem_mechanism,
    active_backend_mechanism_limits, active_backend_name,
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
    /// How the mechanism on this host falls short of the row, such as an
    /// older Landlock ABI. Empty when it holds everything the row states.
    pub mechanism_limits: Vec<MechanismLimit>,
}

impl ProcessSandboxInfo {
    /// The one-line doctor check detail.
    pub(super) fn detail(&self) -> String {
        let mut detail = format!(
            "backend={} filesystem_mechanism={} active={} enforcement=[{}]",
            self.backend,
            self.filesystem_mechanism,
            self.active,
            self.enforcement_receipt
                .as_deref()
                .unwrap_or("no process sandbox")
        );
        if !self.mechanism_limits.is_empty() {
            let limits: Vec<&str> = self
                .mechanism_limits
                .iter()
                .map(|limit| limit.as_str())
                .collect();
            detail.push_str(&format!(" limits=[{}]", limits.join(", ")));
        }
        detail
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
        mechanism_limits: active_backend_mechanism_limits(),
    }
}
