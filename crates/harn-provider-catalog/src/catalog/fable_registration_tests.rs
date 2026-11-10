use super::*;

#[test]
fn fable_52_registration_preserves_predecessors_and_measured_route_controls() {
    let config = llm_config::packaged_config();
    let view = CatalogView::new(config, crate::capabilities::builtin_file(), None);
    for (provider, id) in [
        ("anthropic", "claude-fable-5-2"),
        ("openrouter", "anthropic/claude-fable-5.2"),
    ] {
        let caps = view.capabilities(provider, id).capabilities();
        assert_eq!(caps.allowed_tool_choice_modes, vec!["auto", "none"]);
        assert!(!caps.reasoning_disable_supported);
        assert!(!caps.temperature_supported && !caps.top_p_supported && !caps.top_k_supported);
        assert!(
            !caps.seed_supported
                && !caps.frequency_penalty_supported
                && !caps.presence_penalty_supported
        );
        assert!(!caps.defer_loading && caps.tool_search.is_empty());
    }
    let previous = view
        .capabilities("anthropic", "claude-fable-5-1")
        .capabilities();
    let current = view
        .capabilities("anthropic", "claude-fable-5-2")
        .capabilities();
    assert!(previous.audio && !current.audio);
    assert_eq!(
        current.computer_use_style,
        Some(crate::capabilities::ComputerUseStyle::NativeAnthropicToolset)
    );
    assert_eq!(
        previous.computer_use_style,
        Some(crate::capabilities::ComputerUseStyle::NativeAnthropic)
    );
    let catalog = serde_json::to_value(artifact()).unwrap();
    let models = catalog["models"].as_array().unwrap();
    let model = |id: &str| models.iter().find(|row| row["id"] == id).unwrap();
    for id in ["claude-fable-5-2", "anthropic/claude-fable-5.2"] {
        let row = model(id);
        assert_eq!(row["context_window"], 1_000_000);
        assert_eq!(row["pricing"]["input_per_mtok"], 8.0);
        assert_eq!(row["pricing"]["output_per_mtok"], 40.0);
        assert_eq!(row["pricing"]["cache_read_per_mtok"], 0.2);
        assert_eq!(row["tool_support"]["native"], true);
        assert_eq!(row["structured_output"], "native");
        assert_eq!(row["reasoning"]["none_supported"], false);
        assert_eq!(
            row["reasoning"]["effort_levels"],
            serde_json::json!(["low", "medium", "high", "xhigh", "max"])
        );
        assert_eq!(
            row["modalities"]["input"],
            serde_json::json!(["text", "image"])
        );
        assert_eq!(
            row["format_preferences"]["supports_assistant_prefill"],
            false
        );
    }
    assert_eq!(model("claude-fable-5-1")["pricing"]["input_per_mtok"], 10.0);
    assert_eq!(
        model("claude-fable-5-2")["reasoning"]["modes"],
        serde_json::json!(["adaptive", "effort"])
    );
    assert_eq!(
        model("anthropic/claude-fable-5.2")["reasoning"]["modes"],
        serde_json::json!(["effort"])
    );
    assert!(!models
        .iter()
        .any(|row| row["id"] == "claude-fable-5-2-unserved"));
    let aliases = catalog["aliases"].as_array().unwrap();
    for (name, id) in [
        ("fable52", "claude-fable-5-2"),
        ("fable", "claude-fable-5-1"),
        ("fable51", "claude-fable-5-1"),
    ] {
        assert!(aliases
            .iter()
            .any(|alias| alias["name"] == name && alias["model_id"] == id));
    }
}
