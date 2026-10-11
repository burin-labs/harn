use super::*;
use chrono::NaiveDate;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[test]
fn openrouter_sol_pro_promotion_resolves_standard_prices_and_large_prompt_band() {
    let id = "openai/gpt-5.6-sol-pro";
    let current = OffsetDateTime::parse("2026-10-11T12:00:00Z", &Rfc3339).unwrap();
    let small = model_pricing_for_input_tokens(id, 271999, current).expect("catalogued Pro rate");
    assert_eq!(small.input_per_mtok, 2.0);
    assert_eq!(small.output_per_mtok, 10.0);
    assert_eq!(small.cache_read_per_mtok, Some(0.2));
    assert_eq!(small.cache_write_per_mtok, Some(2.5));
    let band = model_pricing_for_input_tokens(id, 272000, current).expect("large-prompt card");
    assert_eq!(band.input_per_mtok, 4.0);
    assert_eq!(band.output_per_mtok, 15.0);
    let before = OffsetDateTime::parse("2026-08-20T12:00:00Z", &Rfc3339).unwrap();
    let base = model_pricing_for_input_tokens(id, 100, before).expect("durable base card");
    assert_eq!(base.input_per_mtok, 5.0);
    assert_eq!(base.output_per_mtok, 30.0);
}

#[test]
fn promotional_pricing_resolves_at_injected_date_boundaries() {
    let pricing = ModelPricing {
        input_per_mtok: 10.0,
        output_per_mtok: 20.0,
        cache_read_per_mtok: Some(1.0),
        cache_write_per_mtok: None,
        cache_write_1h_per_mtok: None,
        input_token_bands: Vec::new(),
        promotions: vec![PromotionalPricing {
            id: "intro".to_string(),
            starts_on: "2026-08-01".to_string(),
            starts_at: None,
            ends_on: Some("2026-08-31".to_string()),
            ends_at: None,
            review_after: None,
            source_url: "https://provider.example/pricing".to_string(),
            input_per_mtok: 4.0,
            output_per_mtok: 8.0,
            cache_read_per_mtok: Some(0.4),
            cache_write_per_mtok: Some(5.0),
            cache_write_1h_per_mtok: None,
        }],
        schedules: Vec::new(),
        hosted_tool_fees: Default::default(),
        modality_rates: None,
    };

    let date = |value| NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap();
    assert_eq!(
        pricing.effective_on(date("2026-07-31")).input_per_mtok,
        10.0
    );
    assert_eq!(pricing.effective_on(date("2026-08-01")).input_per_mtok, 4.0);
    assert_eq!(
        pricing.effective_on(date("2026-08-31")).output_per_mtok,
        8.0
    );
    assert_eq!(
        pricing.effective_on(date("2026-09-01")).output_per_mtok,
        20.0
    );
}

#[test]
fn promotional_pricing_honors_exact_expiry_instants() {
    let pricing: ModelPricing = toml::from_str(
        r#"input_per_mtok = 0.15
output_per_mtok = 0.50
promotions = [{ id = "flash-launch", starts_on = "2026-08-26", ends_at = "2026-09-09T16:00:00Z", source_url = "https://provider.example/pricing", input_per_mtok = 0.075, output_per_mtok = 0.25 }]"#,
    )
    .unwrap();
    let at = |value| OffsetDateTime::parse(value, &Rfc3339).unwrap();

    assert_eq!(
        pricing
            .effective_at(at("2026-09-09T15:59:59.999999999Z"))
            .input_per_mtok,
        0.075
    );
    assert_eq!(
        pricing
            .effective_at(at("2026-09-09T16:00:00Z"))
            .input_per_mtok,
        0.15
    );
    assert_eq!(
        pricing
            .effective_on(NaiveDate::from_ymd_opt(9999, 12, 31).unwrap())
            .input_per_mtok,
        0.15
    );
}

#[test]
fn pricing_transforms_preserve_the_promotion_schedule() {
    let pricing: ModelPricing = toml::from_str(
        r#"input_per_mtok = 2.0
output_per_mtok = 6.0
promotions = [{ id = "p", starts_on = "2026-01-01", source_url = "https://example.test", input_per_mtok = 1.0, output_per_mtok = 3.0 }]"#,
    )
    .unwrap();

    let scaled = pricing.scaled(2.0);
    assert_eq!(scaled.promotions[0].input_per_mtok, 2.0);
    assert_eq!(scaled.promotions[0].output_per_mtok, 6.0);
    assert_eq!(pricing.for_input_tokens(1).promotions, pricing.promotions);
}
