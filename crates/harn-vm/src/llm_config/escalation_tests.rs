use super::*;

const CATALOG: &str = r#"
[providers.cheap]
base_url = "https://cheap.example/v1"
auth_env = "CHEAP_API_KEY"
[providers.front]
base_url = "https://front.example/v1"
auth_env = "FRONT_API_KEY"
[providers.other]
base_url = "https://other.example/v1"
auth_env = "OTHER_API_KEY"
[providers.local]
base_url = "http://localhost:11434"

[aliases.cheap-default]
id = "small-a"
provider = "cheap"

[models."small-a"]
name = "Small A"
provider = "cheap"
context_window = 128000
tier = "small"
family = "fam-a"
lineage = "lin-a"
[models."small-a".pricing]
input_per_mtok = 0.1
output_per_mtok = 0.4

[models."front-a"]
name = "Front A"
provider = "front"
context_window = 200000
tier = "frontier"
family = "fam-a"
lineage = "lin-a"
strengths = ["coding", "agentic"]
[models."front-a".pricing]
input_per_mtok = 5.0
output_per_mtok = 15.0

[models."front-b"]
name = "Front B"
provider = "other"
context_window = 200000
tier = "frontier"
family = "fam-b"
lineage = "lin-b"
strengths = ["coding"]
[models."front-b".pricing]
input_per_mtok = 5.0
output_per_mtok = 15.0

[models."old-front"]
name = "Old Front"
provider = "other"
context_window = 200000
tier = "frontier"
family = "fam-c"
lineage = "lin-c"
strengths = ["coding", "agentic", "reasoning"]
deprecated = true

[models."dedicated-front"]
name = "Dedicated Front"
provider = "other"
context_window = 200000
tier = "frontier"
family = "fam-d"
lineage = "lin-d"
strengths = ["coding", "agentic", "reasoning"]
availability = "dedicated"

[models."local-front"]
name = "Local Front"
provider = "local"
context_window = 200000
tier = "frontier"
family = "fam-e"
lineage = "lin-e"
strengths = ["coding", "agentic", "reasoning"]

[models."weak-mid"]
name = "Weak Mid"
provider = "front"
context_window = 8000
tier = "mid"
family = "fam-f"
lineage = "lin-f"

[models."orphan-route"]
name = "Orphan Route"
provider = "nowhere"
context_window = 0
tier = "frontier"
strengths = ["coding"]
"#;

fn config() -> ProvidersConfig {
    parse_config_toml(CATALOG).expect("fixture parses")
}

fn options(primary: &str) -> EscalationTargetOptions {
    EscalationTargetOptions {
        primary_model: primary.to_string(),
        available_only: true,
        max_price_multiple: None,
        exclude: Vec::new(),
    }
}

fn pick(options: &EscalationTargetOptions, unavailable: &[&str]) -> EscalationSelection {
    pick_escalation_target(
        &config(),
        options,
        |provider| !unavailable.contains(&provider),
        |_, _| ToolChannelFacts::default(),
    )
}

fn chosen(selection: &EscalationSelection) -> &str {
    &selection
        .target
        .as_ref()
        .unwrap_or_else(|| panic!("expected a target: {selection:?}"))
        .model
}

#[test]
fn stronger_tier_with_family_diversity_wins() {
    let selection = pick(&options("small-a"), &[]);

    assert_eq!(chosen(&selection), "front-b", "{selection:?}");
    let target = selection.target.as_ref().expect("target");
    assert_eq!(target.provider, "other");
    assert!(target
        .reasons
        .iter()
        .any(|reason| reason == "different family from primary"));
    assert_eq!(selection.fallback_code, None);
}

#[test]
fn unusable_rows_are_skipped_and_counted() {
    let selection = pick(&options("small-a"), &[]);

    let skipped = &selection.route_diagnostic.skipped;
    assert_eq!(skipped.deprecated, 1, "{skipped:?}");
    assert_eq!(skipped.dedicated, 1, "{skipped:?}");
    assert_eq!(skipped.not_hosted, 2, "local-front and the orphan route");
    assert_eq!(skipped.not_strong_enough, 1, "weak-mid");
    assert_eq!(skipped.is_primary, 1);
}

#[test]
fn credential_filter_changes_the_choice_and_can_empty_the_set() {
    let without_other = pick(&options("small-a"), &["other"]);
    assert_eq!(chosen(&without_other), "front-a");

    let mut offline = options("small-a");
    offline.available_only = false;
    assert_eq!(chosen(&pick(&offline, &["other", "front"])), "front-b");

    let none = pick(&options("small-a"), &["other", "front"]);
    assert_eq!(none.fallback_code, Some("no_available_credential"));
    assert_eq!(none.route_diagnostic.skipped.no_credential, 2);
    assert!(none.target.is_none());
}

#[test]
fn primary_resolution_never_matches_a_raw_substring() {
    for unresolved in ["", "   ", "unheard-of", "my-small-a-finetune", "small-ab"] {
        let selection = pick(&options(unresolved), &[]);
        assert_eq!(
            selection.fallback_code,
            Some("unknown_primary"),
            "{unresolved:?}: {selection:?}"
        );
        assert!(selection.route_diagnostic.primary.is_none());
    }

    for (selector, matched_by) in [
        ("SMALL-A", "id"),
        ("cheap-default", "alias"),
        ("small-a-preview", "id_prefix"),
        ("small-a:2026-01", "id_prefix"),
    ] {
        let selection = pick(&options(selector), &[]);
        let primary = selection
            .route_diagnostic
            .primary
            .as_ref()
            .unwrap_or_else(|| panic!("{selector}: {selection:?}"));
        assert_eq!(
            (primary.id.as_str(), primary.matched_by),
            ("small-a", matched_by)
        );
        assert_eq!(chosen(&selection), "front-b");
    }
}

#[test]
fn capable_primary_does_not_escalate() {
    let selection = pick(&options("front-a"), &[]);

    assert_eq!(selection.fallback_code, Some("no_stronger_model"));
    assert!(selection.route_diagnostic.primary_already_capable);
}

#[test]
fn same_lineage_at_or_below_the_primary_tier_is_a_sidegrade() {
    let mut config = config();
    let twin = config.models.get_mut("front-b").expect("row");
    twin.lineage = Some("lin-f".to_string());
    twin.tier = Some("mid".to_string());

    let selection = pick_escalation_target(
        &config,
        &options("weak-mid"),
        |_| true,
        |_, _| ToolChannelFacts::default(),
    );

    assert_eq!(selection.route_diagnostic.skipped.same_lineage_sidegrade, 1);
    assert_eq!(chosen(&selection), "front-a");
}

#[test]
fn exclude_matches_ids_providers_families_and_lineages() {
    let mut excluding = options("small-a");
    excluding.exclude = vec!["fam-b".to_string()];
    assert_eq!(chosen(&pick(&excluding, &[])), "front-a");

    excluding.exclude = vec!["other/front-b".to_string(), "lin-a".to_string()];
    let all = pick(&excluding, &[]);
    assert_eq!(all.fallback_code, Some("all_excluded"), "{all:?}");
    assert_eq!(all.route_diagnostic.skipped.excluded, 2);
}

#[test]
fn exclude_matches_a_provider_id_with_underscores_however_it_is_spelled() {
    let mut config = config();
    let gateway = config.providers.remove("other").expect("provider");
    config.providers.insert("other_gw".to_string(), gateway);
    for model in config.models.values_mut() {
        if model.provider == "other" {
            model.provider = "other_gw".to_string();
        }
    }
    for spelling in ["other_gw", "other-gw", "OTHER_GW", "other_gw/front-b"] {
        let mut excluding = options("small-a");
        excluding.exclude = vec![spelling.to_string()];
        let selection = pick_escalation_target(
            &config,
            &excluding,
            |_| true,
            |_, _| ToolChannelFacts::default(),
        );
        assert_eq!(chosen(&selection), "front-a", "{spelling}: {selection:?}");
    }
}

/// Hosted means the artifact's verdict, not "has an API-key variable": a cloud
/// provider that authenticates by signed requests declares no key variable.
#[test]
fn a_hosted_provider_without_an_api_key_variable_is_still_hosted() {
    let mut config = config();
    config.providers.insert(
        "signed".to_string(),
        parse_config_toml(
            "[providers.signed]\nbase_url = \"https://signed.example\"\nauth_style = \"aws_sigv4\"\n",
        )
        .expect("provider parses")
        .providers
        .remove("signed")
        .expect("provider"),
    );
    config.models.get_mut("front-b").expect("row").provider = "signed".to_string();

    let selection = pick_escalation_target(
        &config,
        &options("small-a"),
        |_| true,
        |_, _| ToolChannelFacts::default(),
    );

    assert_eq!(chosen(&selection), "front-b", "{selection:?}");
}

#[test]
fn price_ceiling_without_a_priced_primary_refuses_instead_of_going_uncapped() {
    let mut capped = options("weak-mid");
    capped.max_price_multiple = Some(2.0);
    let selection = pick(&capped, &[]);

    assert_eq!(selection.fallback_code, Some("no_model_within_price"));
    assert!(selection.target.is_none(), "{selection:?}");
    assert!(selection
        .reason
        .as_deref()
        .is_some_and(|reason| reason.contains("no positive catalog price")));
}

#[test]
fn an_alias_without_a_catalog_row_keeps_its_own_provider() {
    let mut config = config();
    config.aliases.insert(
        "proxy-alias".to_string(),
        parse_config_toml("[aliases.x]\nid = \"proxy/open-model\"\nprovider = \"cheap\"\n")
            .expect("alias parses")
            .aliases
            .remove("x")
            .expect("alias"),
    );
    let selection = pick_escalation_target(
        &config,
        &options("proxy-alias"),
        |_| true,
        |_, _| ToolChannelFacts::default(),
    );

    let primary = selection
        .route_diagnostic
        .primary
        .as_ref()
        .expect("resolved");
    assert_eq!(
        (
            primary.id.as_str(),
            primary.provider.as_str(),
            primary.matched_by
        ),
        ("proxy/open-model", "cheap", "alias")
    );
    assert_ne!(
        selection.fallback_code,
        Some("unknown_primary"),
        "{selection:?}"
    );
    assert!(selection.target.is_some(), "{selection:?}");
}

#[test]
fn an_alias_provider_wins_over_its_row_provider() {
    let mut config = config();
    let proxied = parse_config_toml("[aliases.x]\nid = \"small-a\"\nprovider = \"front\"\n")
        .expect("alias parses")
        .aliases
        .remove("x")
        .expect("alias");
    config.aliases.insert("proxied-small".to_string(), proxied);
    let seen = std::cell::RefCell::new(Vec::new());
    let selection = pick_escalation_target(
        &config,
        &options("proxied-small"),
        |_| true,
        |provider, model| {
            if model == "small-a" {
                seen.borrow_mut().push(provider.to_string());
            }
            ToolChannelFacts::default()
        },
    );

    assert_eq!(
        selection
            .route_diagnostic
            .primary
            .as_ref()
            .map(|p| p.provider.as_str()),
        Some("front")
    );
    assert!(
        seen.borrow().iter().all(|provider| provider == "front"),
        "{seen:?}"
    );
}

#[test]
fn tier_outranks_every_secondary_signal() {
    let mut config = config();
    config
        .models
        .retain(|id, _| id == "small-a" || id == "weak-mid");
    let extra = parse_config_toml(
        r#"
[models."bare-front"]
name = "Bare Front"
provider = "other"
context_window = 8000
tier = "frontier"
family = "fam-x"
lineage = "lin-x"

[models."busy-mid"]
name = "Busy Mid"
provider = "other"
context_window = 3000000
tier = "mid"
family = "fam-y"
lineage = "lin-y"
strengths = ["coding", "agentic", "tool_use", "reasoning"]
"#,
    )
    .expect("rows parse");
    config.models.extend(extra.models);

    let selection = pick_escalation_target(
        &config,
        &options("small-a"),
        |_| true,
        |_, _| ToolChannelFacts {
            tools: true,
            ..Default::default()
        },
    );

    assert_eq!(chosen(&selection), "bare-front", "{selection:?}");

    // Control: the mid-tier row really does carry the higher secondary score.
    let mut without_front = options("small-a");
    without_front.exclude = vec!["bare-front".to_string()];
    let rival = pick_escalation_target(
        &config,
        &without_front,
        |_| true,
        |_, _| ToolChannelFacts {
            tools: true,
            ..Default::default()
        },
    );
    assert_eq!(chosen(&rival), "busy-mid");
    assert!(
        rival.target.expect("rival").score > selection.target.expect("target").score,
        "the control must outscore the winner, or the test proves nothing"
    );
}

#[test]
fn price_ceiling_is_relative_to_the_primary() {
    let mut capped = options("small-a");
    capped.max_price_multiple = Some(2.0);
    let selection = pick(&capped, &[]);
    assert_eq!(selection.fallback_code, Some("no_model_within_price"));
    assert_eq!(selection.route_diagnostic.skipped.over_price, 2);

    capped.max_price_multiple = Some(100.0);
    assert_eq!(chosen(&pick(&capped, &[])), "front-b");
}

#[test]
fn unreliable_native_channel_loses_ties_and_names_a_reliable_format() {
    let unreliable_b = |_: &str, model: &str| ToolChannelFacts {
        text: true,
        native_unreliable: model == "front-b",
        ..Default::default()
    };
    let selection = pick_escalation_target(&config(), &options("small-a"), |_| true, unreliable_b);
    assert_eq!(chosen(&selection), "front-a", "front-b is down-ranked");

    let sole = pick_escalation_target(
        &config(),
        &options("small-a"),
        |provider| provider == "other",
        unreliable_b,
    );
    assert_eq!(
        chosen(&sole),
        "front-b",
        "a penalty never excludes a sole option"
    );
    assert_eq!(sole.target.expect("target").tool_format, Some("text"));
}

/// Negative control for "a malformed catalog never throws": every degenerate
/// catalog must yield a coded fallback, never a panic.
#[test]
fn degenerate_catalogs_degrade_to_a_fallback_code() {
    let empty = pick_escalation_target(
        &ProvidersConfig::default(),
        &options("small-a"),
        |_| true,
        |_, _| ToolChannelFacts::default(),
    );
    assert_eq!(empty.fallback_code, Some("unknown_primary"));

    let mut only_primary = config();
    only_primary.models.retain(|id, _| id == "small-a");
    let alone = pick_escalation_target(
        &only_primary,
        &options("small-a"),
        |_| true,
        |_, _| ToolChannelFacts::default(),
    );
    assert_eq!(alone.fallback_code, Some("no_stronger_model"));

    let mut orphans_only = config();
    orphans_only
        .models
        .retain(|id, _| id == "small-a" || id == "orphan-route");
    let orphaned = pick_escalation_target(
        &orphans_only,
        &options("small-a"),
        |_| true,
        |_, _| ToolChannelFacts::default(),
    );
    assert_eq!(orphaned.fallback_code, Some("no_stronger_model"));
}

#[test]
fn serialized_shape_is_a_step_on_success_and_a_code_on_failure() {
    let ok = serde_json::to_value(pick(&options("small-a"), &[])).expect("serializes");
    assert_eq!(ok["model"], "front-b");
    assert_eq!(ok["provider"], "other");
    assert!(ok["score"].as_f64().is_some());
    assert!(ok["reasons"].as_array().is_some_and(|r| !r.is_empty()));
    assert!(ok.get("fallback_code").is_none(), "{ok}");
    assert_eq!(ok["route_diagnostic"]["primary"]["id"], "small-a");

    let failed = serde_json::to_value(pick(&options("nope"), &[])).expect("serializes");
    assert_eq!(failed["fallback_code"], "unknown_primary");
    assert!(failed.get("model").is_none(), "{failed}");
    assert!(failed["reason"].as_str().is_some());
}

#[test]
fn fallback_codes_are_stable_strings() {
    for (code, expected) in [
        (EscalationFallbackCode::UnknownPrimary, "unknown_primary"),
        (EscalationFallbackCode::NoStrongerModel, "no_stronger_model"),
        (EscalationFallbackCode::AllExcluded, "all_excluded"),
        (
            EscalationFallbackCode::NoModelWithinPrice,
            "no_model_within_price",
        ),
        (
            EscalationFallbackCode::NoAvailableCredential,
            "no_available_credential",
        ),
    ] {
        assert_eq!(code.as_code(), expected);
    }
}
