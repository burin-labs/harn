//! Invariants the embedded provider catalog must hold: every model,
//! alias and QC default resolves, pricing is sane, deprecations are annotated,
//! and dedicated routes stay out of the tier aliases.

use super::super::*;

// ── Embedded providers.toml invariants ───────────────────────────────────
// These tests pin properties of the *system* — TOML parses, every
// alias resolves, every deprecated model has a note — without
// pinning specific catalog values. They survive future catalog
// churn and surface real schema breakage.

#[test]
fn embedded_providers_toml_parses_and_is_not_trivially_empty() {
    let config = default_config();
    assert!(
        config.providers.len() >= 10,
        "expected >=10 providers in embedded catalog, got {}",
        config.providers.len()
    );
    assert!(
        config.models.len() >= 20,
        "expected >=20 models in embedded catalog, got {}",
        config.models.len()
    );
    assert!(
        config.aliases.len() >= 15,
        "expected >=15 aliases in embedded catalog, got {}",
        config.aliases.len()
    );
    assert_eq!(config.default_provider.as_deref(), Some("anthropic"));
}

#[test]
fn embedded_catalog_every_deprecated_model_has_a_note() {
    let config = default_config();
    let offenders: Vec<&str> = config
        .models
        .iter()
        .filter(|(_, model)| {
            model.deprecated
                && model
                    .deprecation_note
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
        })
        .map(|(id, _)| id.as_str())
        .collect();
    assert!(
        offenders.is_empty(),
        "deprecated models missing a deprecation_note: {offenders:?}"
    );
}

#[test]
fn embedded_cerebras_catalog_separates_public_and_dedicated_routes() {
    let config = default_config();
    for id in ["gpt-oss-120b", "zai-glm-4.7"] {
        let model = config.models.get(id).expect("current public Cerebras row");
        assert_eq!(model.provider, "cerebras");
        assert_eq!(model.availability, ModelAvailability::Serverless);
        assert!(!model.deprecated);
    }

    let llama = config
        .models
        .get("llama-3.3-70b")
        .expect("legacy Cerebras row");
    assert_eq!(llama.provider, "cerebras");
    assert_eq!(llama.availability, ModelAvailability::Dedicated);
    assert!(llama.deprecated);
}

#[test]
fn embedded_openrouter_gpt_oss_120b_has_no_fragment_bleed() {
    // Regression for the provider-catalog leading-key bleed: the openrouter
    // `openai/gpt-oss-120b` row was the last model in its fragment with no
    // inline tier/open_weight/strengths, so the next fragment's leading bare
    // keys reattached to it after raw-text concatenation — mislabeling it as
    // `open_weight = false` with a spurious `vision` strength. It must now be
    // self-described: open weight, no vision, and a tier consistent with the
    // rest of its equivalence group.
    let config = default_config();
    let model = config
        .models
        .get("openai/gpt-oss-120b")
        .expect("openrouter gpt-oss-120b row");
    assert_eq!(model.provider, "openrouter");
    assert_eq!(
        model.open_weight,
        Some(true),
        "gpt-oss-120b is Apache-2.0 open weight, not the bled-in open_weight=false"
    );
    assert!(
        !model.strengths.iter().any(|s| s == "vision"),
        "gpt-oss-120b is text-only; the bled-in `vision` strength must be gone: {:?}",
        model.strengths
    );
    assert!(
        !model.strengths.is_empty(),
        "gpt-oss-120b must carry its own strengths, not None"
    );

    // tier is a property of the logical model: every active row in the
    // openai-gpt-oss-120b equivalence group must agree.
    let group_tiers: std::collections::BTreeSet<_> = config
        .models
        .values()
        .filter(|m| m.equivalence_group.as_deref() == Some("openai-gpt-oss-120b") && !m.deprecated)
        .map(|m| m.tier.clone())
        .collect();
    assert_eq!(
        group_tiers.len(),
        1,
        "openai-gpt-oss-120b group must share one tier, got {group_tiers:?}"
    );
}

#[test]
fn embedded_catalog_every_model_targets_a_registered_provider() {
    let config = default_config();
    let known: std::collections::BTreeSet<&str> =
        config.providers.keys().map(String::as_str).collect();
    let orphans: Vec<(&str, &str)> = config
        .models
        .iter()
        .filter(|(_, model)| !known.contains(model.provider.as_str()))
        .map(|(id, model)| (id.as_str(), model.provider.as_str()))
        .collect();
    assert!(
        orphans.is_empty(),
        "models reference unknown providers: {orphans:?}"
    );
}

#[test]
fn embedded_catalog_every_alias_targets_a_registered_provider() {
    let config = default_config();
    let known: std::collections::BTreeSet<&str> =
        config.providers.keys().map(String::as_str).collect();
    let orphans: Vec<(&str, &str)> = config
        .aliases
        .iter()
        .filter(|(_, alias)| !known.contains(alias.provider.as_str()))
        .map(|(name, alias)| (name.as_str(), alias.provider.as_str()))
        .collect();
    assert!(
        orphans.is_empty(),
        "aliases reference unknown providers: {orphans:?}"
    );
}

#[test]
fn embedded_catalog_every_qc_default_targets_a_known_model() {
    let config = default_config();
    let orphans: Vec<(&str, &str)> = config
        .qc_defaults
        .iter()
        .filter(|(_, model_id)| !config.models.contains_key(model_id.as_str()))
        .map(|(provider, model_id)| (provider.as_str(), model_id.as_str()))
        .collect();
    assert!(
        orphans.is_empty(),
        "qc_defaults reference unknown models: {orphans:?}"
    );
}

#[test]
fn embedded_catalog_pricing_rates_are_non_negative() {
    let config = default_config();
    for (id, model) in &config.models {
        let Some(pricing) = &model.pricing else {
            continue;
        };
        assert!(
            pricing.input_per_mtok >= 0.0 && pricing.output_per_mtok >= 0.0,
            "{id}: negative pricing — in={} out={}",
            pricing.input_per_mtok,
            pricing.output_per_mtok
        );
        if let Some(rate) = pricing.cache_read_per_mtok {
            assert!(rate >= 0.0, "{id}: negative cache_read rate {rate}");
        }
        if let Some(rate) = pricing.cache_write_per_mtok {
            assert!(rate >= 0.0, "{id}: negative cache_write rate {rate}");
        }
    }
}

#[test]
fn model_availability_parses_known_strings() {
    assert_eq!(
        ModelAvailability::parse("serverless"),
        Some(ModelAvailability::Serverless)
    );
    assert_eq!(
        ModelAvailability::parse("dedicated"),
        Some(ModelAvailability::Dedicated)
    );
    assert_eq!(
        ModelAvailability::parse("unknown"),
        Some(ModelAvailability::Unknown)
    );
    assert_eq!(ModelAvailability::parse("provisioned"), None);
    for value in [
        ModelAvailability::Serverless,
        ModelAvailability::Dedicated,
        ModelAvailability::Unknown,
    ] {
        assert_eq!(ModelAvailability::parse(value.as_str()), Some(value));
    }
}

#[test]
fn embedded_catalog_marks_together_dedicated_route_as_dedicated() {
    let config = default_config();
    let model = config
        .models
        .get("Qwen/Qwen3-Coder-Next-FP8")
        .expect("Together Qwen3 Coder Next FP8 is cataloged");
    assert_eq!(model.provider, "together");
    assert_eq!(model.availability, ModelAvailability::Dedicated);
}

#[test]
fn embedded_catalog_dedicated_models_are_not_targeted_by_tier_aliases() {
    // A dedicated-only model behind a tier alias would silently fail
    // every serverless caller; the catalog must keep those routes
    // separated.
    let config = default_config();
    let dedicated: std::collections::BTreeSet<(&str, &str)> = config
        .models
        .iter()
        .filter(|(_, model)| model.availability == ModelAvailability::Dedicated)
        .map(|(id, model)| (model.provider.as_str(), id.as_str()))
        .collect();
    for (name, alias) in &config.aliases {
        if matches!(
            name.as_str(),
            "frontier"
                | "mid"
                | "small"
                | "tier/frontier"
                | "tier/mid"
                | "tier/small"
                | "sonnet"
                | "opus"
                | "haiku"
        ) {
            assert!(
                !dedicated.contains(&(alias.provider.as_str(), alias.id.as_str())),
                "tier alias `{name}` targets dedicated-only route `{}/{}`",
                alias.provider,
                alias.id,
            );
        }
    }
}

#[test]
fn embedded_catalog_tier_aliases_resolve_to_active_models() {
    // Canonical tiers must resolve to active catalog entries; routing the
    // loop into a sunsetted model is a release blocker.
    for alias in ["frontier", "mid", "small"] {
        let (model, _provider) = resolve_tier_model(alias, None)
            .unwrap_or_else(|| panic!("tier alias `{alias}` must resolve"));
        let entry = model_catalog_entry(&model).unwrap_or_else(|| {
            panic!("tier alias `{alias}` -> `{model}` must be a registered catalog entry")
        });
        assert!(
            !entry.deprecated,
            "tier alias `{alias}` resolves to deprecated model `{model}` ({:?})",
            entry.deprecation_note
        );
    }
}

#[test]
fn gpt_5_5_fast_serving_tier_rides_service_tier() {
    // OpenAI exposes provider-agnostic fast mode through `service_tier`.
    let entry = model_catalog_entry("gpt-5.5").expect("gpt-5.5 catalog entry");
    let fast = entry
        .serving_tiers
        .iter()
        .find(|tier| tier.id == "fast")
        .expect("gpt-5.5 advertises a fast tier");
    let request = fast.request.as_ref().expect("fast tier has request knob");
    assert_eq!(request.param, "service_tier");
    assert_eq!(fast.status.as_deref(), Some("ga"));
}

/// The curated short list is what the "no credentials" error, onboarding copy
/// and `harn models recommend` all read. A typo would silently shrink that
/// list to nothing useful, so pin that every id resolves and that the list
/// stays short enough to print in one line.
#[test]
fn featured_providers_resolve_and_stay_short() {
    let config = default_config();
    let featured = &config.presentation.featured_providers;
    assert!(
        !featured.is_empty(),
        "the embedded catalog must curate a featured provider list"
    );
    assert!(
        featured.len() <= 8,
        "featured_providers is a short list, got {}: {featured:?}",
        featured.len()
    );
    for name in featured {
        assert!(
            config.providers.contains_key(name),
            "featured provider {name} is not declared in the catalog"
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    for name in featured {
        assert!(seen.insert(name), "featured provider {name} listed twice");
    }
    assert!(
        featured.iter().any(|name| config
            .providers
            .get(name)
            .is_some_and(|provider| provider.auth_style == "none")),
        "featured_providers should include a keyless local option: {featured:?}"
    );
}

/// The setup guide names the curated providers in prose so a reader sees a
/// familiar name immediately. That copy is a second projection of the catalog
/// list, so pin it: a provider added to or removed from `featured_providers`
/// must reach the page people are sent to when their credential is missing.
#[test]
fn setup_guide_lists_every_featured_credential_variable() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/src/provider-setup.md"
    );
    let doc =
        std::fs::read_to_string(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"));
    let config = default_config();
    for name in &config.presentation.featured_providers {
        let provider = config
            .providers
            .get(name)
            .unwrap_or_else(|| panic!("featured provider {name} is not in the catalog"));
        let Some(env) = auth_env_names(&provider.auth_env).first().cloned() else {
            continue;
        };
        assert!(
            doc.contains(&format!("`{env}`")),
            "docs/src/provider-setup.md does not name `{env}` for featured provider {name}"
        );
    }
}
