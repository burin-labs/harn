//! Resolve catalog endpoints against the active session environment.

use super::ProviderDef;

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
