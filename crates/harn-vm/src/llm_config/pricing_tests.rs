use super::*;
use chrono::NaiveDate;

#[test]
fn promotional_pricing_resolves_at_injected_date_boundaries() {
    let pricing = ModelPricing {
        input_per_mtok: 10.0,
        output_per_mtok: 20.0,
        cache_read_per_mtok: Some(1.0),
        cache_write_per_mtok: None,
        input_token_bands: Vec::new(),
        promotions: vec![PromotionalPricing {
            id: "intro".to_string(),
            starts_on: "2026-08-01".to_string(),
            ends_on: Some("2026-08-31".to_string()),
            review_after: None,
            source_url: "https://provider.example/pricing".to_string(),
            input_per_mtok: 4.0,
            output_per_mtok: 8.0,
            cache_read_per_mtok: Some(0.4),
            cache_write_per_mtok: Some(5.0),
        }],
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
