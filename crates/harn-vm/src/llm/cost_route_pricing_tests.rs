use super::cost::calculate_cost_for_provider_with_cache;
use super::cost::{calculate_cost_for_provider, pricing_detail_for, PricingSource};

fn install_banded_pricing_model() {
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    let mut model = crate::llm_config::embedded_config(None)
        .models
        .get("gpt-4o-mini")
        .expect("embedded fixture model")
        .clone();
    model.name = "Banded pricing fixture".to_string();
    model.pricing = Some(crate::llm_config::ModelPricing {
        input_per_mtok: 1.0,
        output_per_mtok: 10.0,
        cache_read_per_mtok: Some(0.1),
        cache_write_per_mtok: Some(1.25),
        input_token_bands: vec![crate::llm_config::InputTokenPricingBand {
            minimum_input_tokens: 1_000,
            input_multiplier: 2.0,
            output_multiplier: 1.5,
        }],
    });
    overlay
        .models
        .insert("test/banded-pricing".to_string(), model);
    crate::llm_config::set_user_overrides(Some(overlay));
}

#[test]
fn whole_request_pricing_band_drives_cost_and_cache_accounting() {
    let _guard = super::env_guard();
    install_banded_pricing_model();

    let below = calculate_cost_for_provider("openai", "test/banded-pricing", 999, 100);
    let at_band = calculate_cost_for_provider("openai", "test/banded-pricing", 1_000, 100);
    assert!((below - 0.001999).abs() < 1e-12);
    assert!((at_band - 0.0035).abs() < 1e-12);
    let cached =
        calculate_cost_for_provider_with_cache("openai", "test/banded-pricing", 1_000, 100, 800, 0);
    assert!((cached - 0.00206).abs() < 1e-12);
    crate::llm_config::clear_user_overrides();
}

#[test]
fn provider_wire_model_uses_collision_free_route_pricing() {
    let _guard = super::env_guard();
    crate::llm_config::clear_user_overrides();
    let detail = pricing_detail_for("vercel_ai_gateway", "anthropic/claude-haiku-4.5")
        .expect("Vercel wire model should resolve to its catalog route");
    assert_eq!(detail.source, PricingSource::CatalogModel);
    assert!((detail.input_per_1k - 0.001).abs() < f64::EPSILON);
    assert!((detail.output_per_1k - 0.005).abs() < f64::EPSILON);

    let cost =
        calculate_cost_for_provider("vercel_ai_gateway", "anthropic/claude-haiku-4.5", 696, 40);
    assert!((cost - 0.000896).abs() < 1e-12);
    crate::llm_config::clear_user_overrides();
}
