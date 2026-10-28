use super::*;

fn assert_observations(
    source: &str,
    expected_cells: usize,
    expected_supported: usize,
    expected_models: usize,
) {
    let receipt: serde_json::Value = serde_json::from_str(source).unwrap();
    let cells = receipt["cells"].as_array().unwrap();
    assert_eq!(cells.len(), expected_cells);
    let config = llm_config::packaged_config();
    let view = CatalogView::new(config, crate::capabilities::builtin_file(), None);
    let mut seen = BTreeSet::new();
    let mut supported = 0;
    for cell in cells {
        let model = cell["model"].as_str().unwrap();
        let option = cell["option"].as_str().unwrap();
        assert!(seen.insert((model, option)), "duplicate {model}/{option}");
        assert_eq!(config.models[model].provider, "openrouter");
        let expected = cell["supported"].as_bool().unwrap();
        supported += usize::from(expected);
        assert_eq!(cell["request_count"], if expected { 1 } else { 2 });
        assert_eq!(
            cell["verdict"],
            if expected { "accepted" } else { "rejected" }
        );
        let selected = if expected {
            "response_model"
        } else {
            "control_response_model"
        };
        assert_eq!(cell[selected], model);
        assert!(cell["input_tokens"].as_u64().unwrap() > 0);
        assert!(cell["output_tokens"].as_u64().unwrap() > 0);
        let caps = view.capabilities("openrouter", model).capabilities();
        let actual = match option {
            "stop" => caps.stop_supported,
            "presence_penalty" => caps.presence_penalty_supported,
            "frequency_penalty" => caps.frequency_penalty_supported,
            "top_p" => caps.top_p_supported,
            "top_k" => caps.top_k_supported,
            "seed" => caps.seed_supported,
            _ => panic!("unexpected measured option: {option}"),
        };
        assert_eq!(actual, expected, "{model}/{option}");
    }
    assert_eq!(supported, expected_supported);
    assert_eq!(
        seen.iter()
            .map(|(model, _)| model)
            .collect::<BTreeSet<_>>()
            .len(),
        expected_models
    );
}

#[test]
fn portable_options_match_strict_hosted_observations() {
    assert_observations(
        include_str!("portable_option_observations.json"),
        39,
        14,
        20,
    );
    let config = llm_config::packaged_config();
    let view = CatalogView::new(config, crate::capabilities::builtin_file(), None);
    // Shaped IDs resolve even without catalog membership. Neither a later
    // generation nor the unmeasured free variant may inherit these receipts.
    assert!(!config.models.contains_key("qwen/qwen3.9-flash"));
    assert!(
        !view
            .capabilities("openrouter", "qwen/qwen3.8-max-prime")
            .capabilities()
            .top_k_supported
    );
    assert!(
        !view
            .capabilities("openrouter", "nvidia/nemotron-3.5-super-vl:free")
            .capabilities()
            .top_k_supported
    );
    assert!(
        view.capabilities("openrouter", "meta/muse-glimmer-30b")
            .capabilities()
            .seed_supported
    );
}

#[test]
fn portable_options_match_october_26_hosted_observations() {
    assert_observations(
        include_str!("portable_option_observations_20261026.json"),
        13,
        3,
        6,
    );
}
