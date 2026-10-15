//! Native catalog loading and refresh around the shared catalog contract.

use crate::llm;
use crate::llm_config;
use harn_provider_catalog::{artifact_to_json, CatalogMeasurements, CatalogView};

pub use harn_provider_catalog::artifact::*;
pub(crate) use harn_provider_catalog::config_from_artifact;
pub use harn_provider_catalog::{
    schema_json, schema_value, swift_binding, swift_binding_embedded, typescript_declarations,
    validate_artifact, PROVIDER_CATALOG_GENERATOR, PROVIDER_CATALOG_SCHEMA_ID,
    PROVIDER_CATALOG_SCHEMA_VERSION,
};

pub const HARN_DISABLE_CATALOG_REFRESH_ENV: &str = "HARN_DISABLE_CATALOG_REFRESH";
pub const HARN_PROVIDER_CATALOG_URL_ENV: &str = "HARN_PROVIDER_CATALOG_URL";
pub const HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV: &str = "HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED";
pub const HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV: &str = "HARN_PROVIDER_CATALOG_TRUSTED_KEYS";
pub const DEFAULT_PROVIDER_CATALOG_URL: &str =
    "https://harnlang.com/provider-catalog/provider-catalog.json";
const DEFAULT_REMOTE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
const REMOTE_CACHE_DIR: &str = "provider-catalog";
const REMOTE_CACHE_BODY_FILE: &str = "catalog.json";
const REMOTE_CACHE_META_FILE: &str = "catalog.meta.json";

mod decision_contract;
mod harn_binding;
mod predicate_admission;
mod remote;
pub use decision_contract::{decision_contract_for_route, DecisionContract};
pub use harn_binding::harn_declarations;
pub use predicate_admission::{predicate_model_catalog_identity, validate_predicate_models};
pub use remote::{refresh_runtime_catalog, CatalogRefreshOptions, CatalogRefreshReport};

#[cfg(test)]
mod decision_contract_tests;
#[cfg(test)]
mod operation_tests;
#[cfg(test)]
mod released_row_kind_tests;
#[cfg(test)]
mod supersession_tests;
#[cfg(test)]
mod tests;

/// Project the captured runtime catalog, including cached endpoint measurements.
pub fn artifact() -> ProviderCatalogArtifact {
    let config = llm_config::effective_config();
    let overlay = llm::capabilities::current_user_overrides();
    let view = CatalogView::new(&config, llm::capabilities::builtin_file(), overlay.as_ref());
    let mut measurements = CatalogMeasurements::default();
    for (id, model) in &config.models {
        if let Some(receipt) = llm::capabilities::cached_measurement(&model.provider, id) {
            measurements.insert(model.provider.clone(), id.clone(), receipt);
        }
    }
    view.artifact_with_measurements(&measurements)
}

/// Project packaged configuration and explicit overlays without ambient state.
pub fn artifact_embedded(
    explicit_overlay: Option<&llm_config::ProvidersConfig>,
    explicit_capabilities: Option<&llm::capabilities::CapabilitiesFile>,
) -> ProviderCatalogArtifact {
    let config = llm_config::embedded_config(explicit_overlay);
    CatalogView::new(
        &config,
        llm::capabilities::builtin_file(),
        explicit_capabilities,
    )
    .artifact()
}

pub fn artifact_from_config_and_capabilities(
    config: &llm_config::ProvidersConfig,
    capabilities: &llm::capabilities::CapabilitiesFile,
) -> ProviderCatalogArtifact {
    CatalogView::new(config, capabilities, None).artifact()
}

/// Capture runtime configuration with explicit execution overlays.
pub fn artifact_with_overrides(
    llm_config_overrides: Option<&llm_config::ProvidersConfig>,
    llm_capability_overrides: Option<&llm::capabilities::CapabilitiesFile>,
) -> ProviderCatalogArtifact {
    let config = llm_config::effective_config_with_user_overrides(llm_config_overrides);
    CatalogView::new(
        &config,
        llm::capabilities::builtin_file(),
        llm_capability_overrides,
    )
    .artifact()
}

pub fn artifact_json() -> Result<String, serde_json::Error> {
    artifact_to_json(&artifact())
}

pub fn artifact_json_embedded(
    explicit_overlay: Option<&llm_config::ProvidersConfig>,
    explicit_capabilities: Option<&llm::capabilities::CapabilitiesFile>,
) -> Result<String, serde_json::Error> {
    artifact_to_json(&artifact_embedded(explicit_overlay, explicit_capabilities))
}

pub fn validate_current() -> ProviderCatalogValidation {
    validate_artifact(&artifact())
}
