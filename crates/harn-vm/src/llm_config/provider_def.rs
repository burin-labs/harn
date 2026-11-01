//! Resolve catalog endpoints against the active session environment.

use super::ProviderDef;

/// A provider endpoint configuration diagnostic containing no supplied values.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    thiserror::Error,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderEndpointConfigurationError {
    #[error("provider endpoint is not a URL")]
    InvalidUrl,
    #[error("provider endpoint must be an absolute HTTP(S) URL")]
    NotAbsoluteHttp,
}

impl ProviderEndpointConfigurationError {
    /// Schema for the value-free diagnostic vocabulary exported to hosts.
    pub fn json_schema() -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(Self))
            .expect("endpoint configuration diagnostic schema encodes")
    }
}

pub(crate) fn validate_http_provider_endpoint(
    endpoint: &str,
) -> Result<(), ProviderEndpointConfigurationError> {
    let parsed =
        url::Url::parse(endpoint).map_err(|_| ProviderEndpointConfigurationError::InvalidUrl)?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(ProviderEndpointConfigurationError::NotAbsoluteHttp);
    }
    Ok(())
}

/// Validate a concrete configured endpoint without opening a connection.
/// Catalog templates remain the native transport's responsibility.
pub(crate) fn validate_provider_endpoint_configuration(
    name: &str,
) -> Result<(), ProviderEndpointConfigurationError> {
    let Some(provider) = super::provider_config(name) else {
        return Ok(());
    };
    if provider.protocol.as_deref() == Some("acp") {
        return Ok(());
    }
    let endpoint = resolve_base_url(&provider);
    // Native adapters fill the embedded catalog's endpoint templates from
    // platform configuration. Only the unchanged embedded template defers.
    let catalog_template = super::default_config()
        .providers
        .get(name)
        .is_some_and(|catalog| catalog.base_url.contains('{') && endpoint == catalog.base_url);
    if catalog_template {
        return Ok(());
    }
    validate_http_provider_endpoint(&endpoint)
}

/// Resolve the configured base URL for a provider, checking `base_url_env`,
/// any named `region_env`, then the catalog `base_url`. Host-verified runtime
/// endpoints are applied by [`super::provider_config`] before this
/// public catalog DTO reaches a transport consumer.
pub fn resolve_base_url(pdef: &ProviderDef) -> String {
    if let Some(env_name) = &pdef.base_url_env {
        if let Some(val) = crate::stdlib::process::session_env_var(env_name)
            .ok()
            .flatten()
        {
            // Strip surrounding quotes that some .env parsers leave intact.
            let trimmed = val.trim().trim_matches('"').trim_matches('\'');
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    if let Some(env_name) = &pdef.region_env {
        if let Some(val) = crate::stdlib::process::session_env_var(env_name)
            .ok()
            .flatten()
        {
            let region = val.trim().trim_matches('"').trim_matches('\'');
            if !region.is_empty() {
                let endpoint = pdef
                    .regions
                    .get(region)
                    .or_else(|| pdef.regions.get(&region.to_ascii_lowercase()));
                if let Some(endpoint) = endpoint {
                    let base_url = endpoint.base_url.trim();
                    if !base_url.is_empty() {
                        return base_url.to_string();
                    }
                }
            }
        }
    }
    pdef.base_url.clone()
}
