//! Invariants the embedded provider catalog must hold: every model,
//! alias and QC default resolves, pricing is sane, deprecations are annotated,
//! and dedicated routes stay out of the tier aliases.

use super::super::*;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

// ── Embedded providers.toml invariants ───────────────────────────────────
// These tests pin properties of the *system* — TOML parses, every
// alias resolves, every deprecated model has a note — without
// pinning specific catalog values. They survive future catalog
// churn and surface real schema breakage.

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
fn glm_5_3_flash_routes_and_launch_pricing_are_live() {
    let config = default_config();
    let direct = config
        .models
        .get("glm-5.3-flash")
        .expect("direct Z.AI GLM-5.3-Flash route is catalogued");
    let openrouter = config
        .models
        .get("z-ai/glm-5.3-flash")
        .expect("OpenRouter GLM-5.3-Flash route is catalogued");
    assert_eq!(direct.provider, "zai");
    assert_eq!(openrouter.provider, "openrouter");
    assert!(direct.capabilities.iter().any(|value| value == "vision"));
    assert!(direct.capabilities.iter().any(|value| value == "video"));

    let pricing = direct.pricing.as_ref().expect("direct route is priced");
    let at = |value| OffsetDateTime::parse(value, &Rfc3339).unwrap();
    let promotional = pricing.effective_at(at("2026-08-28T12:00:00Z"));
    assert_eq!(promotional.input_per_mtok, 0.075);
    assert_eq!(promotional.output_per_mtok, 0.25);
    assert_eq!(promotional.cache_read_per_mtok, Some(0.015));
    let restored = pricing.effective_at(at("2026-09-09T16:00:00Z"));
    assert_eq!(restored.input_per_mtok, 0.15);
    assert_eq!(restored.output_per_mtok, 0.50);
    assert_eq!(restored.cache_read_per_mtok, Some(0.03));

    let glm = config
        .aliases
        .get("glm")
        .expect("flagship GLM alias exists");
    assert_eq!(
        glm.id, "glm-5.3",
        "Flash does not retarget the flagship alias"
    );
    let flash = config
        .aliases
        .get("glm-flash")
        .expect("stable Flash alias exists");
    assert_eq!(flash.id, "glm-5.3-flash");
}

#[test]
fn gemini_3_8_flash_routes_and_introductory_pricing_are_live() {
    let config = default_config();
    let direct = config
        .models
        .get("gemini-3.8-flash")
        .expect("direct Google Gemini 3.8 Flash route is catalogued");
    let openrouter = config
        .models
        .get("google/gemini-3.8-flash")
        .expect("OpenRouter Gemini 3.8 Flash route is catalogued");
    assert_eq!(direct.provider, "gemini");
    assert_eq!(openrouter.provider, "openrouter");

    // Limits read off Google's live models endpoint on 2026-09-03.
    assert_eq!(direct.context_window, 1_048_576);
    for capability in ["tools", "vision", "streaming", "prompt_caching", "thinking"] {
        assert!(
            direct.capabilities.iter().any(|value| value == capability),
            "3.8 Flash must declare {capability}"
        );
    }

    // The introductory period runs 2026-09-02 through 2026-12-31; the regular
    // rate applies on either side of it. Pinning both sides keeps a promotion
    // that silently never expires from reading as correct pricing.
    let pricing = direct.pricing.as_ref().expect("direct route is priced");
    let at = |value| OffsetDateTime::parse(value, &Rfc3339).unwrap();
    let promotional = pricing.effective_at(at("2026-09-15T12:00:00Z"));
    assert_eq!(promotional.input_per_mtok, 0.75);
    assert_eq!(promotional.output_per_mtok, 3.75);
    assert_eq!(promotional.cache_read_per_mtok, Some(0.075));
    let restored = pricing.effective_at(at("2027-01-02T12:00:00Z"));
    assert_eq!(restored.input_per_mtok, 1.50);
    assert_eq!(restored.output_per_mtok, 7.50);
    assert_eq!(restored.cache_read_per_mtok, Some(0.15));
    let before = pricing.effective_at(at("2026-08-01T12:00:00Z"));
    assert_eq!(
        before.input_per_mtok, 1.50,
        "the promotion must not apply before it starts"
    );

    // Negative control. The `gemini-*` inference rule routes ANY gemini-shaped
    // string to the Gemini provider, so "it routes" alone is satisfied by an id
    // that does not exist. What separates a supported model from an invented
    // one is the catalog row, so assert the neighbouring ids have none.
    for unknown in [
        "gemini-3.9-flash",
        "gemini-3.8-flash-cyber",
        "gemini-3.8-pro",
    ] {
        assert!(
            !config.models.contains_key(unknown),
            "{unknown} is not a model Google serves us; it must not gain a row"
        );
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

/// A `[model_ladders.*]` step is a model reference the runtime dispatches
/// without asking anyone: `std/agent/judge` resolves its default judge from
/// the `judge` ladder, and `std/agent/sitrep` its summarizer from `sitrep`.
/// A step naming a retired model therefore fails at the provider, mid-run,
/// on a call nobody chose — and a completion judge that cannot reach a model
/// terminates the run `completion_unverified`, which reads like an honest
/// refusal rather than a dead route.
///
/// The tier aliases already carry this rule
/// (`embedded_catalog_tier_aliases_resolve_to_active_models`). Ladder steps
/// are the same kind of reference and did not, which is how the `judge`
/// ladder shipped pointing at `gemini-2.5-flash` after Google stopped
/// serving that id.
///
/// `mock` is exempt: it is Harn's in-process test transport, and its ids are
/// deliberately not catalog routes.
#[test]
fn embedded_catalog_model_ladder_steps_resolve_to_active_models() {
    let config = default_config();
    let mut offenders: Vec<String> = Vec::new();
    for (ladder_name, ladder) in &config.model_ladders {
        for step in &ladder.steps {
            if step.provider.as_deref() == Some("mock") {
                continue;
            }
            let id = match config.aliases.get(&step.model) {
                Some(alias) => alias.id.clone(),
                None => step.model.clone(),
            };
            match config.models.get(&id) {
                None => offenders.push(format!(
                    "{ladder_name} -> `{}` has no catalog row",
                    step.model
                )),
                Some(entry) if entry.deprecated => offenders.push(format!(
                    "{ladder_name} -> `{}` is deprecated ({})",
                    step.model,
                    entry.deprecation_note.as_deref().unwrap_or("no note")
                )),
                Some(_) => {}
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "model ladder steps must name active catalog models: {offenders:?}"
    );
}
