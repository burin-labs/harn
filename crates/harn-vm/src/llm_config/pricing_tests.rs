use super::*;

#[test]
fn gpt_5_6_family_catalog_preserves_role_and_cache_economics() {
    for (model, tier, input, output, cache_read, cache_write) in [
        ("gpt-5.6-sol", "frontier", 5.0, 30.0, 0.5, 6.25),
        ("gpt-5.6-terra", "mid", 2.0, 12.0, 0.2, 2.5),
        ("gpt-5.6-luna", "small", 0.2, 1.2, 0.02, 0.25),
    ] {
        let entry = model_catalog_entry(model).unwrap_or_else(|| panic!("{model} catalog entry"));
        let pricing = entry.pricing.as_ref().expect("GPT-5.6 pricing");
        assert_eq!(entry.context_window, 1_050_000);
        assert_eq!(entry.tier.as_deref(), Some(tier));
        assert_eq!(pricing.input_per_mtok, input);
        assert_eq!(pricing.output_per_mtok, output);
        assert_eq!(pricing.cache_read_per_mtok, Some(cache_read));
        assert_eq!(pricing.cache_write_per_mtok, Some(cache_write));
        assert!(entry
            .capabilities
            .iter()
            .any(|capability| capability == "vision"));
    }

    let alias = resolve_model_info("gpt-5.6");
    assert_eq!(alias.provider, "openai");
    assert_eq!(alias.id, "gpt-5.6-sol");
    assert_eq!(
        qc_defaults().get("openai").map(String::as_str),
        Some("gpt-5.6-luna")
    );
}
