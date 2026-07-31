use super::*;

#[test]
fn gpt_5_6_catalog_exports_current_openai_and_openrouter_economics() {
    llm_config::clear_user_overrides();
    let catalog = artifact();

    for (
        model_id,
        input,
        output,
        cache_read,
        cache_write,
        fast_input,
        fast_output,
        fast_cache_read,
        fast_cache_write,
        flex_input,
        flex_output,
        flex_cache_read,
        flex_cache_write,
    ) in [
        (
            "gpt-5.6-terra",
            2.0,
            12.0,
            0.2,
            2.5,
            4.0,
            24.0,
            0.4,
            5.0,
            1.0,
            6.0,
            0.1,
            1.25,
        ),
        (
            "gpt-5.6-luna",
            0.2,
            1.2,
            0.02,
            0.25,
            0.4,
            2.4,
            0.04,
            0.5,
            0.1,
            0.6,
            0.01,
            0.125,
        ),
    ] {
        let model = catalog
            .models
            .iter()
            .find(|model| model.provider == "openai" && model.id == model_id)
            .unwrap_or_else(|| panic!("missing OpenAI {model_id} catalog row"));
        let pricing = model.pricing.as_ref().expect("standard pricing");
        assert_eq!(pricing.input_per_mtok, input);
        assert_eq!(pricing.output_per_mtok, output);
        assert_eq!(pricing.cache_read_per_mtok, Some(cache_read));
        assert_eq!(pricing.cache_write_per_mtok, Some(cache_write));
        assert_eq!(pricing.input_token_bands[0].minimum_input_tokens, 272_001);
        assert_eq!(pricing.input_token_bands[0].input_multiplier, 2.0);
        assert_eq!(pricing.input_token_bands[0].output_multiplier, 1.5);

        let fast = model
            .serving_tiers
            .iter()
            .find(|tier| tier.id == "fast")
            .expect("Fast mode tier");
        let request = fast.request.as_ref().expect("Fast mode request knob");
        assert_eq!(request.param, "service_tier");
        assert_eq!(request.value, "fast");
        assert_eq!(request.response_values, ["fast", "priority"]);
        let fast_pricing = fast.pricing.as_ref().expect("Fast mode pricing");
        assert_eq!(fast_pricing.input_per_mtok, fast_input);
        assert_eq!(fast_pricing.output_per_mtok, fast_output);
        assert_eq!(fast_pricing.cache_read_per_mtok, Some(fast_cache_read));
        assert_eq!(fast_pricing.cache_write_per_mtok, Some(fast_cache_write));

        let flex = model
            .serving_tiers
            .iter()
            .find(|tier| tier.id == "flex")
            .expect("Flex processing tier");
        assert_eq!(flex.status.as_deref(), Some("beta"));
        assert_eq!(flex.discount_percent, Some(50));
        let flex_pricing = flex.pricing.as_ref().expect("Flex pricing");
        assert_eq!(flex_pricing.input_per_mtok, flex_input);
        assert_eq!(flex_pricing.output_per_mtok, flex_output);
        assert_eq!(flex_pricing.cache_read_per_mtok, Some(flex_cache_read));
        assert_eq!(flex_pricing.cache_write_per_mtok, Some(flex_cache_write));
    }

    for (model_id, input, output) in [
        ("openai/gpt-5.6-terra", 2.0, 12.0),
        ("openai/gpt-5.6-terra-pro", 2.0, 12.0),
        ("openai/gpt-5.6-luna", 0.2, 1.2),
        ("openai/gpt-5.6-luna-pro", 0.2, 1.2),
    ] {
        let model = catalog
            .models
            .iter()
            .find(|model| model.provider == "openrouter" && model.id == model_id)
            .unwrap_or_else(|| panic!("missing OpenRouter {model_id} catalog row"));
        let pricing = model.pricing.as_ref().expect("OpenRouter pricing");
        assert_eq!(pricing.input_per_mtok, input);
        assert_eq!(pricing.output_per_mtok, output);
    }
}

#[test]
fn serving_tier_response_values_surface_in_generated_contracts() {
    let schema = schema_value();
    assert_eq!(
        schema["$defs"]["serving_tier_request"]["properties"]["response_values"]["uniqueItems"],
        true
    );

    let typescript = typescript_declarations();
    assert!(typescript.contains("response_values?: string[]"));

    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("public let responseValues: [String]?"));
}
