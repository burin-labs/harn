//! The setup-UI default each provider offers, projected into the artifact.

use super::*;

#[test]
fn the_artifact_carries_each_providers_setup_ui_default() {
    let catalog = artifact_embedded(None, None);
    // The setup-UI offer is current-generation and distinct from the cheap
    // QC route, which deliberately stays on a sampling-tolerant model.
    assert_eq!(
        catalog
            .portal_defaults
            .get("anthropic")
            .map(|portal| portal.model_id.as_str()),
        Some("claude-sonnet-5-5")
    );
    let openai = catalog
        .portal_defaults
        .get("openai")
        .expect("openai has a setup default");
    assert_eq!(openai.model_id, "gpt-6-luna");
    assert_eq!(openai.reasoning_effort.as_deref(), Some("high"));
    for (provider, portal) in &catalog.portal_defaults {
        let row = catalog
            .models
            .iter()
            .find(|row| &row.provider == provider && row.id == portal.model_id)
            .unwrap_or_else(|| {
                panic!(
                    "{provider} portal default {} has no catalog row",
                    portal.model_id
                )
            });
        if let Some(effort) = &portal.reasoning_effort {
            let levels = serde_json::to_value(&row.reasoning).unwrap()["effort_levels"].clone();
            assert!(
                levels
                    .as_array()
                    .is_some_and(|levels| levels.iter().any(|level| level == effort)),
                "{provider} portal effort {effort} is not one of {levels}"
            );
        }
    }
    let config = config_from_artifact(&catalog);
    assert_eq!(
        config
            .provider_defaults
            .get("anthropic")
            .and_then(|defaults| defaults.portal.as_deref()),
        Some("claude-sonnet-5-5"),
        "a host serving a remote artifact keeps the setup-UI default"
    );
    assert_eq!(
        config
            .provider_defaults
            .get("openai")
            .and_then(|defaults| defaults.portal_reasoning_effort.as_deref()),
        Some("high")
    );
}

#[test]
fn the_artifact_ranks_credential_gated_automatic_defaults() {
    let catalog = artifact_embedded(None, None);
    let ranked: Vec<_> = catalog
        .automatic_defaults
        .iter()
        .map(|entry| (entry.provider.as_str(), entry.model_id.as_str()))
        .collect();
    assert_eq!(
        ranked,
        [
            ("anthropic", "claude-sonnet-5-5"),
            ("openai", "gpt-6.1-sol")
        ]
    );
    for CatalogAutomaticDefault { provider, model_id } in &catalog.automatic_defaults {
        assert!(
            catalog
                .models
                .iter()
                .any(|row| &row.provider == provider && &row.id == model_id),
            "{provider} automatic default {model_id} has no catalog row"
        );
    }
    let config = config_from_artifact(&catalog);
    let anthropic = &config.provider_defaults["anthropic"];
    assert_eq!(anthropic.automatic.as_deref(), Some("claude-sonnet-5-5"));
    assert!(anthropic.automatic_order < config.provider_defaults["openai"].automatic_order);
}
