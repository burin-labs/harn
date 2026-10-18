//! One package owner for linked hosts and command-line projections.
//!
//! Connector discovery retains the immutable package generation while the
//! caller selects outbound grants and initializes connector clients.

/// Crate-local source contribution to compiler and check cache identities.
pub const SOURCE_FINGERPRINT: &str = env!("HARN_SOURCE_FINGERPRINT");

pub mod env_guard;
pub mod format;
pub mod net;
pub mod package;
pub mod path_policy;
pub mod precompile;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use package::{
    try_load_provider_connectors, try_load_root_provider_connectors, PackageError,
    ResolvedProviderConnectors,
};
