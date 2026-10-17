//! Queries over one explicit catalog and capability overlay.

use crate::capabilities::{
    CapabilitiesFile, CapabilityProbeReceipt, CapabilityResolution, CapabilityRoute,
    CapabilityRules,
};
use crate::config::ProvidersConfig;

/// A borrowed catalog snapshot. Capturing runtime overlays belongs to the
/// adapter; every query here uses only these inputs.
pub struct CatalogView<'a> {
    pub(crate) config: &'a ProvidersConfig,
    rules: CapabilityRules<'a>,
}

impl<'a> CatalogView<'a> {
    pub fn new(
        config: &'a ProvidersConfig,
        base: &'a CapabilitiesFile,
        overlay: Option<&'a CapabilitiesFile>,
    ) -> Self {
        Self {
            config,
            rules: CapabilityRules::new(base, overlay),
        }
    }

    pub fn capabilities(&self, provider: &str, model: &str) -> CapabilityResolution {
        let model = capability_model_id_for_config(provider, model, self.config);
        let responses_api = self
            .config
            .providers
            .get(provider)
            .is_some_and(|definition| {
                definition
                    .features
                    .iter()
                    .any(|feature| feature == "responses_api")
            });
        self.rules.resolve(CapabilityRoute {
            provider,
            model: &model,
            responses_api,
        })
    }

    pub fn artifact(&self) -> crate::ProviderCatalogArtifact {
        self.artifact_with_measurements(&CatalogMeasurements::default())
    }

    pub fn artifact_with_measurements(
        &self,
        measurements: &CatalogMeasurements,
    ) -> crate::ProviderCatalogArtifact {
        crate::catalog::project(self, measurements)
    }
}

/// Endpoint-owned observations captured by the runtime before projection.
#[derive(Default)]
pub struct CatalogMeasurements(
    std::collections::BTreeMap<String, std::collections::BTreeMap<String, CapabilityProbeReceipt>>,
);

impl CatalogMeasurements {
    pub fn insert(&mut self, provider: String, model: String, receipt: CapabilityProbeReceipt) {
        self.0.entry(provider).or_default().insert(model, receipt);
    }

    pub(crate) fn get(&self, provider: &str, model: &str) -> Option<&CapabilityProbeReceipt> {
        self.0.get(provider)?.get(model)
    }
}

/// Capability rules may name the wire identity when the provider declares it.
pub fn capability_model_id_for_config(
    provider: &str,
    model_id: &str,
    config: &ProvidersConfig,
) -> String {
    if !config.providers.get(provider).is_some_and(|provider| {
        provider
            .features
            .iter()
            .any(|feature| feature == "wire_model_capabilities")
    }) {
        return model_id.to_string();
    }
    config
        .models
        .get(model_id)
        .filter(|model| model.provider == provider)
        .and_then(|model| model.wire_model.clone())
        .unwrap_or_else(|| model_id.to_string())
}

pub fn configured_provider_selector<'a>(
    config: &ProvidersConfig,
    selector: &'a str,
) -> Option<&'a str> {
    let (provider, model) = selector.split_once(':')?;
    if model.is_empty() {
        return None;
    }
    config.providers.contains_key(provider).then_some(provider)
}

pub fn normalize_model_id_with_config(raw: &str, config: &ProvidersConfig) -> String {
    for prefix in [
        "ollama:",
        "ollama/",
        "local:",
        "huggingface:",
        "hf:",
        "cerebras/",
    ] {
        if let Some(stripped) = raw.strip_prefix(prefix) {
            return stripped.to_string();
        }
    }
    if let Some((provider, model)) = raw.split_once(':') {
        if !model.is_empty()
            && (provider == "mock" || configured_provider_selector(config, raw).is_some())
        {
            return model.to_string();
        }
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_catalog_controls_wire_identity_and_transport_authority() {
        let mut config = crate::config::parse_config_toml(
            r#"
[providers.proxy]
features = ["wire_model_capabilities"]
[models.private-alias]
name = "Private alias"
provider = "proxy"
wire_model = "wire-model"
context_window = 8192
[models.other-provider]
name = "Other provider"
provider = "elsewhere"
wire_model = "wire-model"
context_window = 8192
"#,
        )
        .expect("catalog fixture");
        let rules: CapabilitiesFile = toml::from_str(
            r#"
[[provider.proxy]]
model_match = "wire-model"
vision = true
responses_api = true
"#,
        )
        .expect("capability fixture");
        let view = CatalogView::new(&config, &rules, None);
        let alias = view.capabilities("proxy", "private-alias");
        assert!(alias.matched() && alias.capabilities().vision);
        assert!(!alias.capabilities().responses_api);
        assert!(!view.capabilities("proxy", "other-provider").matched());
        assert!(!view.capabilities("proxy", "unserved").matched());

        config
            .providers
            .get_mut("proxy")
            .unwrap()
            .features
            .push("responses_api".into());
        assert!(
            CatalogView::new(&config, &rules, None)
                .capabilities("proxy", "private-alias")
                .capabilities()
                .responses_api
        );
    }
}
