//! The VM's view of the shared high-confidence secret pattern catalog, used by
//! both redaction and the `secret_scan` builtin.
//!
//! Nothing is defined here. The catalog *data* lives in the dependency-free
//! [`harn_secret_catalog`] crate so downstream host consumers that must stay off
//! the Harn runtime (e.g. the Burin TUI's fast-lane `util` crate) can share the
//! exact same single source of truth, and the *compiled matchers* live in
//! [`harn_kernel::pure`] beside the scanner that already builds them — scanning
//! and redaction are two questions asked of one catalog, so they share one
//! compiled copy. This module keeps the historical
//! `crate::secret_patterns::*` import path and owns the VM's policy of warming
//! that copy during startup.

pub(crate) use harn_kernel::pure::compiled_secret_patterns as compiled_default_secret_patterns;

/// Compile the shared secret catalog before VM execution can reach a scanner
/// or persistence path on an already-deep call stack.
pub(crate) fn initialize_default_secret_patterns() {
    let _ = compiled_default_secret_patterns();
}

#[cfg(test)]
pub(crate) fn default_secret_patterns_initialized() -> bool {
    harn_kernel::pure::secret_patterns_compiled()
}
