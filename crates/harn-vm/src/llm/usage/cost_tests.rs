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
fn terra_search_calls_settle_at_published_fees_in_standard_and_fast_service() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    let mut request = input();
    request.provider = "openai".into();
    request.model = "gpt-6.1-terra".into();
    request.cache_write_tokens = 0;
    request.prompt_cache_ttl = None;
    request.billing.hosted_tool_calls =
        [("web_search".into(), 1), ("file_search".into(), 1)].into();
    for (served_fast, token_cost) in [(false, 0.001), (true, 0.002)] {
        request.served_fast = served_fast;
        let result = settle_call_cost(&request).expect("embedded Terra card");
        assert!(result.unpriced.is_empty(), "{result:?}");
        assert!((result.cost_usd.expect("complete price") - token_cost - 0.0125).abs() < 1e-12);
        assert!((result.breakdown.unwrap().adjustments_usd - 0.0125).abs() < 1e-12);
    }
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
fn free_inference_keeps_hosted_gaps_separate_from_plain_and_zero_calls() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    let mut request = input();
    request.provider = "ollama".into();
    request.model = "local_cost_fixture".into();
    request.billing = BillingUsage::default();
    for count in [None, Some(0), Some(1)] {
        request.billing.hosted_tool_calls = count
            .map(|count| ("unpublished_tool".into(), count))
            .into_iter()
            .collect();
        let result = settle_call_cost(&request).unwrap();
        assert_eq!(result.known_cost_usd, 0.0);
        assert_eq!(result.pricing, None);
        if count == Some(1) {
            assert_eq!(result.cost_usd, None);
            assert_eq!(
                result.unpriced,
                [CallCostUnpriced::HostedTool {
                    name: "unpublished_tool".into(),
                }]
            );
        } else {
            assert_eq!(result.cost_usd, Some(0.0));
            assert!(result.unpriced.is_empty());
        }
    }
}

#[test]
fn a_real_hosted_fee_card_prices_free_inference_without_token_or_audio_charges() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    let mut request = input();
    let config = crate::llm_config::embedded_config(None);
    let mut model = config.models[&request.model].clone();
    model.provider = "ollama".into();
    model.wire_model = None;
    let pricing = model.pricing.as_mut().unwrap();
    pricing.input_per_mtok = 99.0;
    pricing.output_per_mtok = 123.0;
    pricing.cache_write_1h_per_mtok = None;
    pricing.input_token_bands.clear();
    pricing.promotions.clear();
    pricing.schedules.clear();
    pricing.modality_rates = Some(crate::llm_config::ModalityRates {
        audio_input_per_mtok: Some(32.0),
        audio_output_per_mtok: Some(64.0),
        cached_audio_input_per_mtok: Some(0.4),
    });
    pricing.hosted_tool_fees = [(
        "web_search".into(),
        crate::llm_config::HostedToolFee {
            per_1k_calls: 10.0,
            free_per_month: Some(100),
            source_url: "https://example.invalid/fixture".into(),
        },
    )]
    .into();
    let mut provider = config.providers["ollama"].clone();
    provider.platform_fee_percent = Some(5.5);
    request.provider = "ollama".into();
    request.model = "local_hosted_fee_fixture".into();
    request.output_tokens = 100;
    request.cache_read_tokens = 100;
    request.cache_write_tokens = 200;
    request.billing.audio_input_tokens = Some(600);
    request.billing.audio_output_tokens = Some(80);
    request.billing.cached_audio_input_tokens = Some(100);
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.providers.insert("ollama".into(), provider);
    overlay.models.insert(request.model.clone(), model);
    for per_1k_calls in [10.0, 0.0] {
        overlay
            .models
            .get_mut(&request.model)
            .unwrap()
            .pricing
            .as_mut()
            .unwrap()
            .hosted_tool_fees
            .get_mut("web_search")
            .unwrap()
            .per_1k_calls = per_1k_calls;
        crate::llm_config::set_user_overrides(Some(overlay.clone()));
        let result = settle_call_cost(&request).unwrap();
        let expected = per_1k_calls / 1000.0 * 1.055;
        assert!((result.cost_usd.unwrap() - expected).abs() < 1e-12);
        assert!(result.unpriced.is_empty());
        let rates = result.rates.unwrap();
        assert_eq!(rates.input_per_mtok, 0.0);
        assert_eq!(rates.output_per_mtok, 0.0);
        assert_eq!(rates.cache_write_1h_per_mtok, Some(0.0));
        assert_eq!(rates.source, "provider_economics");
        let breakdown = result.breakdown.unwrap();
        assert_eq!(
            breakdown.input_usd
                + breakdown.output_usd
                + breakdown.cache_read_usd
                + breakdown.cache_write_usd,
            0.0
        );
        assert!((breakdown.adjustments_usd - expected).abs() < 1e-12);
        let facts = result
            .pricing
            .expect("a real card priced hosted units, including a quoted zero");
        assert_eq!(facts.rate_card, "base");
        assert!(!facts.cache_ttl_unpriced);
        assert_eq!(facts.monthly_allowance_unapplied, ["web_search"]);
        assert!((facts.platform_fee_estimate_usd - per_1k_calls / 1000.0 * 0.055).abs() < 1e-12);
        let native = crate::llm::api::LlmResult {
            provider: request.provider.clone(),
            model: request.model.clone(),
            input_tokens: request.input_tokens,
            output_tokens: request.output_tokens,
            cache_read_tokens: request.cache_read_tokens,
            cache_write_tokens: request.cache_write_tokens,
            telemetry: Box::new(crate::llm::api::ProviderTelemetry {
                started_at_ms: Some(request.started_at_ms),
                prompt_cache_ttl: Some("1h".into()),
                billing: Some(Box::new(request.billing.clone())),
                ..Default::default()
            }),
            ..Default::default()
        }
        .usage();
        let receipt = ProviderUsageReceipt {
            input_tokens: Some(request.input_tokens),
            output_tokens: Some(request.output_tokens),
            cache_read_tokens: request.cache_read_tokens,
            cache_write_tokens: request.cache_write_tokens,
            started_at_ms: Some(request.started_at_ms),
            prompt_cache_ttl: request.prompt_cache_ttl,
            billing: Some(Box::new(request.billing.clone())),
            ..Default::default()
        };
        for native in [
            native,
            LlmUsage::from_provider_receipt(&request.provider, &request.model, &receipt),
        ] {
            assert!((native.cost_usd.unwrap() - expected).abs() < 1e-12);
            assert_eq!(native.unpriced_calls, 0);
            assert_eq!(native.pricing.as_deref(), Some(facts.as_ref()));
            assert_eq!(native.cache_savings_usd, 0.0);
        }
    }
    crate::llm_config::clear_user_overrides();
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
    let selected = LlmUsage::settle_provider_receipt(&request.provider, &request.model, &receipt);
    assert!((selected.usage.known_cost_usd - 0.016).abs() < 1e-12);
    assert!(
        selected.pricing.is_some(),
        "a real selected rate card is required"
    );

    let mut model = crate::llm_config::embedded_config(None).models[&request.model].clone();
    model.pricing.as_mut().unwrap().cache_write_1h_per_mtok = Some(60.0);
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.models.insert(request.model.clone(), model);
    crate::llm_config::set_user_overrides(Some(overlay));
    let changed = settle_call_cost(&request).unwrap();
    assert!((changed.cost_usd.unwrap() - 0.07).abs() < 1e-12);
    assert_eq!(changed.rates.unwrap().cache_write_1h_per_mtok, Some(60.0));

    let retained = project_settlement(&request, selected).unwrap();
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
    request.billing = BillingUsage::default();
    let result = settle_call_cost(&request).unwrap();
    assert_eq!(result.cost_usd, None);
    assert_eq!(result.known_cost_usd, 0.0);
    assert_eq!(result.pricing, None);
    assert_eq!(result.unpriced, [CallCostUnpriced::PricingUnknown]);
    request.billing.hosted_tool_calls = [("unpublished_tool".into(), 1)].into();
    let result = settle_call_cost(&request).unwrap();
    assert_eq!(result.cost_usd, None);
    assert_eq!(result.known_cost_usd, 0.0);
    assert_eq!(
        result.unpriced,
        [
            CallCostUnpriced::HostedTool {
                name: "unpublished_tool".into()
            },
            CallCostUnpriced::PricingUnknown,
        ]
    );
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
