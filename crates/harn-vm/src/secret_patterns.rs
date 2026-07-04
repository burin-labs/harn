//! Shared high-confidence secret pattern catalog used by both redaction and
//! the `secret_scan` builtin.
//!
//! The catalog data itself lives in the dependency-free
//! [`harn_secret_catalog`] crate so downstream host consumers that must stay
//! off the Harn runtime (e.g. the Burin TUI's fast-lane `util` crate) can share
//! the exact same single source of truth. This module simply re-exports it
//! under the historical `crate::secret_patterns::*` path the VM's redaction and
//! scan paths already import.

pub(crate) use harn_secret_catalog::{
    SecretPatternSpec, DEFAULT_SECRET_PATTERN_SPECS, PRECISION_HEURISTIC,
};
