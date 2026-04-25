//! Repo scanner host capability.
//!
//! Ports `Sources/BurinCore/Scanner/` — deterministic project-wide file
//! enumeration honoring `.gitignore` and friends, plus an incremental mode
//! driven by a watch token. Implementation lands in issue B4.

use crate::registry::{BuiltinRegistry, HostlibCapability};

/// Scanner capability handle.
#[derive(Default)]
pub struct ScannerCapability;

impl HostlibCapability for ScannerCapability {
    fn module_name(&self) -> &'static str {
        "scanner"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register_unimplemented("hostlib_scanner_scan_project", "scanner", "scan_project");
        registry.register_unimplemented(
            "hostlib_scanner_scan_incremental",
            "scanner",
            "scan_incremental",
        );
    }
}
