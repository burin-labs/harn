use super::*;
#[test]
fn calculate_cost_uses_catalog_model_pricing() {
    let _guard = crate::llm::env_guard();
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.models.insert(
        "gpt-4o-mini".to_string(),
        crate::llm_config::ModelDef {
            name: "Test GPT-4o Mini".to_string(),
            display_name: None,
            blurb: None,
            provider: "openai".to_string(),
            context_window: 128_000,
            logical_model: None,
            equivalence_group: None,
            served_variant: None,
            wire_model: None,
            api_dialect: None,
            rate_limits: None,
            performance: None,
            architecture: None,
            local_memory: None,
            runtime_context_window: None,
            stream_timeout: None,
            capabilities: Vec::new(),
            pricing: Some(crate::llm_config::ModelPricing {
                input_per_mtok: 10.0,
                output_per_mtok: 20.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
                input_token_bands: Vec::new(),
                promotions: Vec::new(),
            }),
            deprecated: false,
            deprecation_note: None,
            superseded_by: None,
            serving_tiers: Vec::new(),
            quality_tags: Vec::new(),
            availability: crate::llm_config::ModelAvailability::default(),
            tier: None,
            open_weight: None,
            strengths: Vec::new(),
            benchmarks: std::collections::BTreeMap::new(),
            family: None,
            lineage: None,
            complementary_with: Vec::new(),
            avoid_as_reviewer_for: Vec::new(),
        },
    );
    crate::llm_config::set_user_overrides(Some(overlay));

    // 1000*10 + 1000*20 = 30000; /1e6 = 0.03, exactly.
    assert_eq!(
        calculate_cost_decimal("gpt-4o-mini", 1000, 1000),
        Decimal::from_str("0.03").unwrap()
    );

    crate::llm_config::clear_user_overrides();
}

#[test]
fn calculate_cost_is_zero_for_unknown_model() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    assert_eq!(
        calculate_cost_decimal("definitely-unpriced-model", 1_000, 1_000),
        Decimal::ZERO
    );
}

#[test]
fn authored_rate_decimal_recovers_the_written_literal_not_float_noise() {
    // Catalog rates are short literals parsed from TOML into f64; many
    // (0.15, 0.8, 0.08) are not exactly representable in binary. The
    // recovery must reconstruct the *authored* decimal, never the f64's
    // binary-rounding tail that `from_f64_retain` would expose.
    for (raw, written) in [
        (0.15_f64, "0.15"),
        (0.8, "0.8"),
        (0.08, "0.08"),
        (4.0, "4"),
        (0.0, "0"),
        (3.75, "3.75"),
    ] {
        let recovered = authored_rate_decimal(raw);
        assert_eq!(
            recovered,
            Decimal::from_str(written).unwrap(),
            "rate {raw} should recover as {written}"
        );
    }
    // It must NOT equal the lossy `from_f64_retain` decimal.
    assert_ne!(
        authored_rate_decimal(0.1),
        Decimal::from_f64_retain(0.1).unwrap()
    );
}

#[test]
fn calculate_cost_decimal_is_exact_for_inexact_catalog_rates() {
    let _guard = crate::llm::env_guard();
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.models.insert(
        "gpt-4o-mini".to_string(),
        crate::llm_config::ModelDef {
            name: "Test GPT-4o Mini".to_string(),
            display_name: None,
            blurb: None,
            provider: "openai".to_string(),
            context_window: 128_000,
            logical_model: None,
            equivalence_group: None,
            served_variant: None,
            wire_model: None,
            api_dialect: None,
            rate_limits: None,
            performance: None,
            architecture: None,
            local_memory: None,
            runtime_context_window: None,
            stream_timeout: None,
            capabilities: Vec::new(),
            // Inexact-in-binary rates, like the real catalog.
            pricing: Some(crate::llm_config::ModelPricing {
                input_per_mtok: 0.15,
                output_per_mtok: 0.60,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
                input_token_bands: Vec::new(),
                promotions: Vec::new(),
            }),
            deprecated: false,
            deprecation_note: None,
            superseded_by: None,
            serving_tiers: Vec::new(),
            quality_tags: Vec::new(),
            availability: crate::llm_config::ModelAvailability::default(),
            tier: None,
            open_weight: None,
            strengths: Vec::new(),
            benchmarks: std::collections::BTreeMap::new(),
            family: None,
            lineage: None,
            complementary_with: Vec::new(),
            avoid_as_reviewer_for: Vec::new(),
        },
    );
    crate::llm_config::set_user_overrides(Some(overlay));

    // 1000 * 0.15 + 500 * 0.60 = 150 + 300 = 450; /1e6 = 0.00045 exactly.
    assert_eq!(
        calculate_cost_decimal("gpt-4o-mini", 1000, 500),
        Decimal::from_str("0.00045").unwrap()
    );

    crate::llm_config::clear_user_overrides();
}

#[test]
fn calculate_cost_for_mock_uses_the_modeled_catalog_price() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();

    let mocked = calculate_cost_for_provider("mock", "gpt-4o-mini", 3_000, 4_000);
    let live = calculate_cost_for_provider("openai", "gpt-4o-mini", 3_000, 4_000);
    assert!(mocked > 0.001);
    assert!((mocked - live).abs() < 1e-12);
}

#[test]
fn calculate_cost_for_provider_falls_back_to_provider_economics() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    let cost =
        calculate_cost_for_provider("openai", "some-bespoke-openai-deployment", 1_000, 1_000);
    let (input_per_1k, output_per_1k, _) = crate::llm_config::provider_economics("openai");
    let expected = (1_000.0 * input_per_1k.unwrap() + 1_000.0 * output_per_1k.unwrap()) / 1_000.0;
    assert!(
        (cost - expected).abs() < 1e-9,
        "cost={cost}, expected={expected}"
    );
}

#[test]
fn self_hosted_routes_are_priced_at_zero_and_paid_routes_stay_unpriced() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();

    // Every shipped local provider also spells `cost_per_1k_* = 0.0`, so these
    // would pass without the `local_runtime` fallback below. They pin the
    // catalog's coherence, not the fallback.
    for provider in ["llamacpp", "ollama", "mlx", "vllm"] {
        let cost = pricing_aware_call_cost(provider, "any-locally-served-model", 1_000, 1_000);
        assert_eq!(
            cost,
            Some(0.0),
            "{provider} declares local_runtime, so its rate is known-zero, not unknown"
        );
    }

    // The fallback itself: a self-hosted provider that never spells a rate is
    // still known-zero. Without this, adding a local runtime and forgetting
    // `cost_per_1k_*` silently reintroduces the unpriced-call stop.
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.providers.insert(
        "rateless-local".to_string(),
        crate::llm_config::ProviderDef {
            local_runtime: Some(crate::llm_config::LocalRuntimeDef::default()),
            cost_per_1k_in: None,
            cost_per_1k_out: None,
            ..crate::llm_config::ProviderDef::default()
        },
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    assert!(crate::llm_config::provider_is_self_hosted("rateless-local"));
    assert_eq!(
        pricing_aware_call_cost("rateless-local", "whatever", 1_000, 1_000),
        Some(0.0),
        "a self-hosted provider that declares no rate is still known-zero"
    );
    crate::llm_config::clear_user_overrides();

    // Negative pin: the fix must not launder unknown pricing into a free
    // ride for a paid provider that simply has no catalog row.
    assert_eq!(
        pricing_aware_call_cost("some-unlisted-paid-provider", "whatever", 1_000, 1_000),
        None,
        "a provider with neither catalog pricing nor a local runtime stays unpriced"
    );

    // The predicate both readers resolve "bills nothing" through.
    assert!(crate::llm_config::provider_is_self_hosted("llamacpp"));
    assert!(!crate::llm_config::provider_is_self_hosted("openai"));
    assert!(!crate::llm_config::provider_is_self_hosted(
        "some-unlisted-paid-provider"
    ));
}

#[test]
fn calculate_cost_for_provider_with_cache_applies_cache_read_discount() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();

    let without_cache =
        calculate_cost_for_provider("anthropic", "claude-sonnet-4-20250514", 1_000, 1_000);
    let with_cache = pricing_aware_call_cost_with_cache(
        "anthropic",
        "claude-sonnet-4-20250514",
        1_000,
        1_000,
        500,
        0,
    )
    .expect("catalog-priced model");

    assert!(with_cache > 0.0);
    assert!(
        with_cache < without_cache,
        "cache reads should be priced below uncached prompt input"
    );
}

#[test]
fn pricing_detail_reports_source() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    let exact = pricing_detail_for("anthropic", "claude-sonnet-4-20250514").unwrap();
    assert_eq!(exact.source, PricingSource::CatalogModel);
    assert!(exact.cache_read_per_1k.is_some());

    let provider_only = pricing_detail_for("openai", "some-bespoke-openai-deployment").unwrap();
    assert_eq!(provider_only.source, PricingSource::ProviderEconomics);
    assert!(provider_only.cache_read_per_1k.is_none());

    assert!(pricing_detail_for("local", "no-such-local-model").is_some()); // local has 0/0
    assert!(pricing_detail_for("nonexistent_provider", "ghost-model").is_none());
}

#[test]
fn pricing_aware_call_cost_distinguishes_unpriced_from_zero() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();

    // Known catalog model: Some(cost) matching the priced arithmetic.
    let priced = pricing_aware_call_cost("anthropic", "claude-sonnet-4-20250514", 1_000, 1_000);
    let expected =
        calculate_cost_for_provider("anthropic", "claude-sonnet-4-20250514", 1_000, 1_000);
    assert!(priced.is_some());
    assert!((priced.unwrap() - expected).abs() < 1e-9);

    // Genuinely unpriced (provider not in the catalog economics table):
    // None, not a misleading 0.0. `calculate_cost_for_provider` coerces
    // the same case to 0.0, which is exactly the ambiguity this helper
    // exists to remove.
    assert_eq!(
        pricing_aware_call_cost("nonexistent_provider", "ghost-model", 1_000, 1_000),
        None
    );
    assert_eq!(
        calculate_cost_for_provider("nonexistent_provider", "ghost-model", 1_000, 1_000),
        0.0
    );
}

#[test]
fn format_usd_amount_auto_precision_and_grouping() {
    assert_eq!(format_usd_amount(0.000_045, None, false), "$0.000045");
    assert_eq!(format_usd_amount(1.234_5, None, false), "$1.2345");
    assert_eq!(format_usd_amount(1234.5, None, false), "$1,234.50");
    assert_eq!(format_usd_amount(-1234.5, None, false), "-$1,234.50");
    assert_eq!(format_usd_amount(1234.5, None, true), "+$1,234.50");
    assert_eq!(format_usd_amount(0.123_456_789, Some(2), false), "$0.12");
    assert_eq!(format_usd_amount(1.0, Some(0), false), "$1");
}

#[test]
fn format_usd_handles_fractional_carry_into_whole() {
    // 0.00027 * 300_000 produces 80.999… in IEEE-754; the formatter
    // must round-then-render rather than splitting the rounded fraction
    // back into a separate component (regression: "$80.1.0000").
    let amount = 0.000_27_f64 * 300_000.0;
    assert!((amount - 81.0).abs() < 1e-6);
    assert_eq!(format_usd_amount(amount, None, false), "$81.0000");
}

#[test]
fn fast_tier_bills_premium_pricing_when_served_fast() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();

    // Opus 4.8 fast mode is 2x standard ($5/$25 -> $10/$50 per MTok).
    let standard = pricing_detail_for_tier("anthropic", "claude-opus-4-8", false, 0).unwrap();
    let fast = pricing_detail_for_tier("anthropic", "claude-opus-4-8", true, 0).unwrap();
    assert_eq!(standard.source, PricingSource::CatalogModel);
    assert_eq!(fast.source, PricingSource::CatalogServingTier);
    assert!((fast.input_per_1k - 2.0 * standard.input_per_1k).abs() < 1e-9);
    assert!((fast.output_per_1k - 2.0 * standard.output_per_1k).abs() < 1e-9);

    // A model with no fast tier ignores the flag and bills standard.
    let no_fast =
        pricing_detail_for_tier("anthropic", "claude-sonnet-4-20250514", true, 0).unwrap();
    assert_eq!(no_fast.source, PricingSource::CatalogModel);
}

#[test]
fn project_call_cost_excludes_cached_input_from_full_rate() {
    // OpenAI (subset) convention: cache tokens are folded into `input_tokens`,
    // so subtracting them yields fewer full-rate tokens than the no-cache call.
    let detail = pricing_detail_for("anthropic", "claude-sonnet-4-20250514").unwrap();
    let with_cache = project_call_cost(&detail, 10_000, 500, 8_000, 0);
    let no_cache = project_call_cost(&detail, 10_000, 500, 0, 0);
    assert!(with_cache < no_cache);
}

#[test]
fn project_call_cost_openai_subset_convention_subtracts_cache() {
    // OpenAI reports cache tokens inside `input_tokens`. Billable input is the
    // remainder after removing the cached subset; cache billed at cache rate.
    let detail = pricing_detail_for("anthropic", "claude-sonnet-4-20250514").unwrap();
    let cache_read_rate = detail.cache_read_per_1k.unwrap_or(detail.input_per_1k);
    let got = project_call_cost(&detail, 10_000, 500, 8_000, 0);
    let expected =
        (2_000.0 * detail.input_per_1k + 500.0 * detail.output_per_1k + 8_000.0 * cache_read_rate)
            / 1000.0;
    assert!((got - expected).abs() < 1e-9);
}

#[test]
fn project_call_cost_anthropic_separate_convention_bills_full_input() {
    // Anthropic reports `input_tokens` already excluding cache, with cache in
    // separate fields (cache_read > input). The 200 real non-cached input
    // tokens must be billed at the full input rate, not zeroed out.
    let detail = pricing_detail_for("anthropic", "claude-sonnet-4-20250514").unwrap();
    let cache_read_rate = detail.cache_read_per_1k.unwrap_or(detail.input_per_1k);
    let got = project_call_cost(&detail, 200, 500, 10_000, 0);
    let expected =
        (200.0 * detail.input_per_1k + 500.0 * detail.output_per_1k + 10_000.0 * cache_read_rate)
            / 1000.0;
    assert!((got - expected).abs() < 1e-9);
    // Regression guard for the pre-fix bug: the old code computed billable
    // input as (input - cache_read - cache_write).max(0), which for
    // input=200, cache_read=10000 clamped to 0 — dropping the real input
    // term entirely. That buggy cost omits the 200*input_per_1k the correct
    // cost includes, so the fixed result must exceed it.
    let buggy = (500.0 * detail.output_per_1k + 10_000.0 * cache_read_rate) / 1000.0;
    assert!(got > buggy);
}

#[test]
fn cache_savings_uses_catalog_cache_pricing() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();

    let savings =
        cache_savings_usd_for_provider("anthropic", "claude-sonnet-4-20250514", 1000, 1000, 0);
    assert!((savings - 0.0027).abs() < 0.0000001);

    let write_delta =
        cache_savings_usd_for_provider("anthropic", "claude-sonnet-4-20250514", 1000, 0, 1000);
    assert!((write_delta + 0.00075).abs() < 0.0000001);

    crate::llm_config::clear_user_overrides();
}

#[test]
fn cache_hit_ratio_handles_subset_and_separate_anthropic_counts() {
    assert!((cache_hit_ratio(1000, 250, 0) - 0.25).abs() < f64::EPSILON);
    assert!((cache_hit_ratio(100, 900, 0) - 0.9).abs() < f64::EPSILON);
    assert_eq!(cache_hit_ratio(0, 0, 0), 0.0);
}

#[test]
fn token_budget_guard_restores_prior_state_on_drop() {
    let _guard_outer = crate::llm::env_guard();
    reset_cost_state();

    let outer = install_llm_token_budget(100);
    assert_eq!(peek_total_tokens(), 0);
    // Simulate accumulation by writing the thread-local directly.
    LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = 50);

    // Nested guard wipes accumulation and installs a tighter cap.
    {
        let _inner = install_llm_token_budget(10);
        assert_eq!(peek_total_tokens(), 0);
        LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = 5);
    }

    // Outer scope restored on inner drop.
    assert_eq!(peek_total_tokens(), 50);
    drop(outer);
    assert_eq!(peek_total_tokens(), 0);

    reset_cost_state();
}

#[test]
fn set_budget_rearms_in_place_without_resetting_accumulation() {
    let _guard_outer = crate::llm::env_guard();
    reset_cost_state();

    // Install a $1.00 cap and spend $0.60 against it.
    let _budget = install_llm_cost_budget(1.0);
    LLM_ACCUMULATED_COST.with(|a| *a.borrow_mut() = 0.60);

    // Tighten the cap below current spend: the next preflight must trip,
    // and the already-accumulated total must be preserved (not reset).
    set_llm_cost_budget(Some(0.50));
    assert!((peek_total_cost() - 0.60).abs() < f64::EPSILON);
    LLM_BUDGET.with(|b| assert_eq!(*b.borrow(), Some(0.50)));

    // Loosen the cap: spend stays, ceiling rises, room reopens.
    set_llm_cost_budget(Some(2.0));
    assert!((peek_total_cost() - 0.60).abs() < f64::EPSILON);
    LLM_BUDGET.with(|b| assert_eq!(*b.borrow(), Some(2.0)));

    // Clear the cap entirely.
    set_llm_cost_budget(None);
    LLM_BUDGET.with(|b| assert_eq!(*b.borrow(), None));

    // Negative ceilings clamp to zero (a hard stop), matching `install_*`.
    set_llm_cost_budget(Some(-5.0));
    LLM_BUDGET.with(|b| assert_eq!(*b.borrow(), Some(0.0)));

    reset_cost_state();
}

#[test]
fn set_token_budget_rearms_in_place_without_resetting_accumulation() {
    let _guard_outer = crate::llm::env_guard();
    reset_cost_state();

    let _budget = install_llm_token_budget(100);
    LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = 60);

    set_llm_token_budget(Some(50));
    assert_eq!(peek_total_tokens(), 60);
    LLM_TOKEN_BUDGET.with(|b| assert_eq!(*b.borrow(), Some(50)));

    set_llm_token_budget(None);
    assert_eq!(peek_total_tokens(), 60);
    LLM_TOKEN_BUDGET.with(|b| assert_eq!(*b.borrow(), None));

    reset_cost_state();
}

#[test]
fn token_budget_raises_categorized_error_when_exhausted() {
    let _guard_outer = crate::llm::env_guard();
    reset_cost_state();
    let _budget = install_llm_token_budget(10);

    // First call within budget — admits.
    let first = accumulate_llm_usage("claude-sonnet-4-20250514", 5, 0, 0.0);
    assert!(first.is_ok());

    // Second call pushes over — raises BudgetExceeded.
    let second = accumulate_llm_usage("claude-sonnet-4-20250514", 8, 0, 0.0);
    match second {
        Err(VmError::CategorizedError { category, message }) => {
            assert_eq!(category, ErrorCategory::BudgetExceeded);
            assert!(message.contains("token budget"), "got: {message}");
        }
        other => panic!("expected BudgetExceeded, got {other:?}"),
    }

    reset_cost_state();
}

/// `gemini-2.5-pro` bills $1.25/$10 per MTok up to 200k input tokens and
/// $2.50/$15 beyond it. A cost surface that reads the base rate pair
/// under-reports a long-context call by the band multiplier, so the two
/// helpers must not be interchangeable — and this pins which is which.
#[test]
fn a_long_context_call_bills_at_the_input_token_band() {
    let _guard = crate::llm::env_guard();
    let (provider, model) = ("gemini", "gemini-2.5-pro");

    // Below the band: the two helpers agree.
    let below = pricing_aware_call_cost(provider, model, 100_000, 1_000).expect("priced");
    assert!(
        (below - (100_000.0 * 1.25 + 1_000.0 * 10.0) / 1_000_000.0).abs() < 1e-9,
        "below the band must bill at the base rate, got {below}"
    );

    // Above it, the band applies: input x2, output x1.5.
    let above = pricing_aware_call_cost(provider, model, 300_000, 1_000).expect("priced");
    assert!(
        (above - (300_000.0 * 2.50 + 1_000.0 * 15.0) / 1_000_000.0).abs() < 1e-9,
        "above the band must bill at the banded rate, got {above}"
    );

    // The band-unaware pair is what the buggy surfaces used. Keep the gap
    // visible so nobody "simplifies" a usage-aware caller back onto it.
    let (base_in, base_out) = pricing_per_1k_for(provider, model).expect("priced");
    let unbanded = (300_000.0 * base_in + 1_000.0 * base_out) / 1000.0;
    assert!(
        above > unbanded,
        "the banded price must exceed the base-rate price: {above} vs {unbanded}"
    );
}

#[test]
fn mock_provider_has_an_authoritative_zero_cost() {
    assert_eq!(
        pricing_aware_call_cost("mock", "any-fixture", 10, 20),
        Some(0.0)
    );
    assert_eq!(
        pricing_aware_call_cost_with_cache("mock", "any-fixture", 10, 20, 5, 2),
        Some(0.0)
    );
}
