use super::*;

fn input() -> CallCostInput {
    CallCostInput {
        provider: "anthropic".into(),
        model: "claude-sonnet-4-20250514".into(),
        started_at_ms: 1_792_756_800_000,
        input_tokens: 1_000,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 1_000,
        prompt_cache_ttl: Some(PromptCacheTtl::OneHour),
        served_fast: false,
        billing: BillingUsage {
            hosted_tool_calls: [("web_search".into(), 1)].into(),
            ..BillingUsage::default()
        },
    }
}

#[test]
fn public_settlement_prices_long_lived_cache_and_hosted_calls_without_spending() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    let before = crate::llm::peek_total_cost();
    let result = settle_call_cost(&input()).unwrap();
    assert!((result.cost_usd.expect("complete price") - 0.016).abs() < 1e-12);
    assert_eq!(result.known_cost_usd, result.cost_usd.unwrap());
    let breakdown = result.breakdown.as_ref().unwrap();
    assert!((breakdown.cache_write_usd - 0.006).abs() < 1e-12);
    assert!((breakdown.adjustments_usd - 0.01).abs() < 1e-12);
    assert!((breakdown.total() - result.known_cost_usd).abs() < 1e-12);
    assert!(result.unpriced.is_empty());
    assert_eq!(result.pricing.unwrap().rate_card, "base");
    assert_eq!(crate::llm::peek_total_cost(), before);
}

#[test]
fn missing_tool_price_keeps_priced_cache_and_refuses_a_complete_amount() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    let mut request = input();
    request.billing.hosted_tool_calls = [("unpublished_tool".into(), 1)].into();
    let result = settle_call_cost(&request).unwrap();
    assert_eq!(result.cost_usd, None);
    assert!((result.known_cost_usd - 0.006).abs() < 1e-12);
    assert_eq!(
        result.unpriced,
        [CallCostUnpriced::HostedTool {
            name: "unpublished_tool".into()
        }]
    );
}

#[test]
fn projection_retains_the_rate_card_selected_before_a_catalog_change() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    let request = input();
    let receipt = ProviderUsageReceipt {
        input_tokens: Some(request.input_tokens),
        reported_input_tokens: Some(request.input_tokens),
        output_tokens: Some(request.output_tokens),
        cache_write_tokens: request.cache_write_tokens,
        started_at_ms: Some(request.started_at_ms),
        prompt_cache_ttl: request.prompt_cache_ttl,
        billing: Some(Box::new(request.billing.clone())),
        ..ProviderUsageReceipt::default()
    };
    let (usage, selected) =
        LlmUsage::from_provider_receipt_with_pricing(&request.provider, &request.model, &receipt);
    assert!((usage.known_cost_usd - 0.016).abs() < 1e-12);
    assert!(selected.is_some(), "a real selected rate card is required");

    let mut model = crate::llm_config::embedded_config(None).models[&request.model].clone();
    model.pricing.as_mut().unwrap().cache_write_1h_per_mtok = Some(60.0);
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.models.insert(request.model.clone(), model);
    crate::llm_config::set_user_overrides(Some(overlay));
    let changed = settle_call_cost(&request).unwrap();
    assert!((changed.cost_usd.unwrap() - 0.07).abs() < 1e-12);
    assert_eq!(changed.rates.unwrap().cache_write_1h_per_mtok, Some(60.0));

    let retained = project_settlement(&request, usage, selected).unwrap();
    crate::llm_config::clear_user_overrides();
    assert!((retained.cost_usd.unwrap() - 0.016).abs() < 1e-12);
    assert_eq!(retained.rates.unwrap().cache_write_1h_per_mtok, Some(6.0));
    let breakdown = retained.breakdown.unwrap();
    assert!((breakdown.cache_write_usd - 0.006).abs() < 1e-12);
    assert!((breakdown.adjustments_usd - 0.01).abs() < 1e-12);
}

#[test]
fn unknown_route_is_not_a_measured_free_call() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    let mut request = input();
    request.provider = "unpriced_fixture".into();
    request.model = "unpriced_fixture/model".into();
    let result = settle_call_cost(&request).unwrap();
    assert_eq!(result.cost_usd, None);
    assert_eq!(result.known_cost_usd, 0.0);
    assert_eq!(result.pricing, None);
    assert_eq!(result.unpriced, [CallCostUnpriced::PricingUnknown]);
}

#[test]
fn invalid_counts_fail_before_pricing() {
    let mut request = input();
    request.cache_read_tokens = 1;
    assert!(settle_call_cost(&request)
        .unwrap_err()
        .to_string()
        .contains("exceed"));
    request.cache_read_tokens = 0;
    request.output_tokens = -1;
    assert!(settle_call_cost(&request)
        .unwrap_err()
        .to_string()
        .contains("output_tokens"));
    request.output_tokens = 0;
    request.billing.audio_output_tokens = Some(1);
    assert!(settle_call_cost(&request)
        .unwrap_err()
        .to_string()
        .contains("audio"));
}

#[test]
fn audio_must_fit_the_cache_bucket_whose_text_charge_it_replaces() {
    let mut request = input();
    request.input_tokens = 100;
    request.cache_write_tokens = 0;
    request.cache_read_tokens = 100;
    request.billing = BillingUsage {
        audio_input_tokens: Some(100),
        ..BillingUsage::default()
    };
    assert!(settle_call_cost(&request)
        .unwrap_err()
        .to_string()
        .contains("audio"));
    request.billing.cached_audio_input_tokens = Some(100);
    assert!(
        settle_call_cost(&request).is_ok(),
        "all audio explicitly belongs to cache reads"
    );
    request.cache_read_tokens = 40;
    request.cache_write_tokens = 10;
    request.billing.cached_audio_input_tokens = Some(40);
    assert!(
        settle_call_cost(&request).is_err(),
        "uncached audio does not fit regular input"
    );
    request.billing.audio_input_tokens = Some(90);
    assert!(
        settle_call_cost(&request).is_ok(),
        "50 uncached audio tokens fit 50 regular input tokens"
    );
}

fn instant(value: &str) -> i64 {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .unwrap()
        .unix_timestamp()
        * 1_000
}

#[test]
fn public_settlement_applies_every_catalog_extension_and_names_monthly_allowances() {
    let _guard = crate::llm::env_guard();
    let mut model = crate::llm_config::embedded_config(None).models["gpt-4o-mini"].clone();
    model.pricing = Some(serde_json::from_value(serde_json::json!({
        "input_per_mtok": 10.0, "output_per_mtok": 20.0,
        "cache_read_per_mtok": 1.0, "cache_write_per_mtok": 12.5,
        "cache_write_1h_per_mtok": 20.0,
        "input_token_bands": [{"minimum_input_tokens": 1000, "input_multiplier": 2.0, "output_multiplier": 3.0}],
        "promotions": [{
            "id": "launch", "starts_on": "2026-10-23", "ends_on": "2026-10-23",
            "source_url": "https://example.com/pricing",
            "input_per_mtok": 4.0, "output_per_mtok": 8.0,
            "cache_read_per_mtok": 0.4, "cache_write_per_mtok": 5.0,
            "cache_write_1h_per_mtok": 8.0
        }],
        "schedules": [{
            "id": "midday", "days": ["fri"], "start": "12:00", "end": "13:00",
            "input_multiplier": 0.5, "output_multiplier": 0.5,
            "source_url": "https://example.com/pricing"
        }],
        "hosted_tool_fees": {"search": {"per_1k_calls": 10.0, "free_per_month": 100, "source_url": "https://example.com/pricing"}},
        "modality_rates": {"audio_input_per_mtok": 30.0, "audio_output_per_mtok": 60.0, "cached_audio_input_per_mtok": 3.0}
    })).unwrap());
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay
        .models
        .insert("test/full-cost-settlement".into(), model);
    crate::llm_config::set_user_overrides(Some(overlay));
    let mut request = CallCostInput {
        provider: "openai".into(),
        model: "test/full-cost-settlement".into(),
        started_at_ms: instant("2026-10-23T12:00:00Z"),
        input_tokens: 1_000,
        output_tokens: 100,
        cache_read_tokens: 200,
        cache_write_tokens: 400,
        prompt_cache_ttl: Some(PromptCacheTtl::OneHour),
        served_fast: false,
        billing: BillingUsage {
            hosted_tool_calls: [("search".into(), 2)].into(),
            audio_input_tokens: Some(100),
            audio_output_tokens: Some(80),
            cached_audio_input_tokens: Some(50),
            invalid_modality_counts: false,
        },
    };
    let result = settle_call_cost(&request).unwrap();
    assert!((result.cost_usd.unwrap() - 0.03375).abs() < 1e-12);
    assert!((result.breakdown.as_ref().unwrap().total() - result.known_cost_usd).abs() < 1e-12);
    assert_eq!(
        result.rates.as_ref().unwrap().cache_write_1h_per_mtok,
        Some(8.0)
    );
    assert!(result.unpriced.is_empty());
    let facts = result.pricing.unwrap();
    assert_eq!(facts.rate_card, "schedule:midday");
    assert_eq!(facts.input_band_minimum, Some(1_000));
    assert_eq!(facts.monthly_allowance_unapplied, ["search"]);

    request.input_tokens = 999;
    let below = settle_call_cost(&request).unwrap();
    assert!((below.cost_usd.unwrap() - 0.025633).abs() < 1e-12);
    assert_eq!(below.pricing.unwrap().input_band_minimum, None);

    request.input_tokens = 1_000;
    request.started_at_ms = instant("2026-10-23T13:00:00Z");
    let promoted = settle_call_cost(&request).unwrap();
    assert!((promoted.cost_usd.unwrap() - 0.0475).abs() < 1e-12);
    assert_eq!(promoted.pricing.unwrap().rate_card, "promotion:launch");

    request.started_at_ms = instant("2026-10-24T12:00:00Z");
    let base = settle_call_cost(&request).unwrap();
    assert!((base.cost_usd.unwrap() - 0.0622).abs() < 1e-12);
    assert_eq!(base.pricing.unwrap().rate_card, "base");
    crate::llm_config::clear_user_overrides();
}
