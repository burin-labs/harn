//! Catalog configuration, overlay semantics, and source diagnostics.

mod aggregate;
pub mod chat_wire;
mod data_controls;
pub mod overlay_audit;
mod parsing;
mod provider_def;
mod query;
mod usage_accounting_audit;

#[cfg(test)]
mod data_controls_tests;
#[cfg(test)]
mod diagnostic_tests;
#[cfg(test)]
mod local_runtime_tests;
#[cfg(test)]
mod overlay_audit_tests;
#[cfg(test)]
mod overlay_tests;

pub use crate::{model_def::*, presentation::*, pricing::*, ModelOperation};
pub use aggregate::*;
pub use chat_wire::*;
pub use data_controls::*;
pub use overlay_audit::*;
pub use parsing::*;
pub use provider_def::*;
pub use query::*;
pub use usage_accounting_audit::*;

pub const LOGICAL_MODEL_DEFAULT_PREFIX: &str = "logical:";
pub const FAST_TIER_ID: &str = "fast";
/// Requesting the standard reasoning mode is equivalent to omitting a mode.
pub const STANDARD_MODE_ID: &str = "standard";

/// Whether a defaults selector names an exact logical model rather than a route glob.
pub fn is_logical_model_selector(selector: &str) -> bool {
    selector.starts_with(LOGICAL_MODEL_DEFAULT_PREFIX)
}

/// Packaged defaults with an optional explicit overlay, independent of runtime state.
pub fn embedded_config(overlay: Option<&ProvidersConfig>) -> ProvidersConfig {
    let mut config = packaged_config().clone();
    if let Some(overlay) = overlay {
        config.merge_from(overlay);
    }
    config
}

/// Parse the packaged defaults once. Every caller shares the same validated snapshot.
pub fn packaged_config() -> &'static ProvidersConfig {
    static CONFIG: std::sync::OnceLock<ProvidersConfig> = std::sync::OnceLock::new();
    CONFIG.get_or_init(|| {
        parse_config_toml(crate::EMBEDDED_PROVIDERS_TOML)
            .expect("embedded providers.toml must parse")
    })
}
