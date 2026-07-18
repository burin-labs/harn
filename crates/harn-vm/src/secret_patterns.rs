//! Shared high-confidence secret pattern catalog used by both redaction and
//! the `secret_scan` builtin.
//!
//! The catalog data itself lives in the dependency-free
//! [`harn_secret_catalog`] crate so downstream host consumers that must stay
//! off the Harn runtime (e.g. the Burin TUI's fast-lane `util` crate) can share
//! the exact same single source of truth. This module simply re-exports it
//! under the historical `crate::secret_patterns::*` path the VM's redaction and
//! scan paths already import.

use std::sync::OnceLock;

use regex::Regex;

pub(crate) use harn_secret_catalog::{
    SecretPatternSpec, DEFAULT_SECRET_PATTERN_SPECS, PRECISION_HEURISTIC,
};

/// One catalog entry paired with its process-wide compiled matcher.
pub(crate) struct CompiledSecretPattern {
    pub(crate) spec: &'static SecretPatternSpec,
    pub(crate) regex: Regex,
}

static COMPILED_DEFAULT_SECRET_PATTERNS: OnceLock<Vec<CompiledSecretPattern>> = OnceLock::new();

pub(crate) fn compiled_default_secret_patterns() -> &'static [CompiledSecretPattern] {
    COMPILED_DEFAULT_SECRET_PATTERNS.get_or_init(|| {
        DEFAULT_SECRET_PATTERN_SPECS
            .iter()
            .map(|spec| CompiledSecretPattern {
                spec,
                regex: Regex::new(spec.regex).unwrap_or_else(|error| {
                    panic!("invalid {} secret regex: {error}", spec.detector)
                }),
            })
            .collect()
    })
}

/// Compile the shared secret catalog before VM execution can reach a scanner
/// or persistence path on an already-deep call stack.
pub(crate) fn initialize_default_secret_patterns() {
    let _ = compiled_default_secret_patterns();
}

#[cfg(test)]
pub(crate) fn default_secret_patterns_initialized() -> bool {
    COMPILED_DEFAULT_SECRET_PATTERNS.get().is_some()
}
