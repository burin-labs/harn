//! Runtime-owned process filesystem-confinement facts for `harn doctor`.

use harn_vm::process_sandbox::{
    active_backend_filesystem_available, active_backend_filesystem_mechanism, active_backend_name,
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProcessSandboxInfo {
    pub backend: String,
    pub filesystem_mechanism: String,
    pub active: bool,
}

pub(super) fn process_sandbox_info() -> ProcessSandboxInfo {
    ProcessSandboxInfo {
        backend: active_backend_name().to_string(),
        filesystem_mechanism: active_backend_filesystem_mechanism().to_string(),
        active: active_backend_filesystem_available(),
    }
}
