use super::*;

#[test]
fn every_configured_provider_prefix_is_inferred_before_model_shape() {
    assert_eq!(infer_provider("local:gemma-4-e4b-it"), "ollama");
    assert_eq!(infer_provider("ollama:qwen3:30b-a3b"), "ollama");
    assert_eq!(infer_provider("local:owner/model"), "ollama");
    assert_eq!(infer_provider("hf:Qwen/Qwen3.6-35B-A3B"), "huggingface");

    for provider in provider_names() {
        let selector = format!("{provider}:catalog-native-model");
        let expected = if provider == "local" {
            "ollama"
        } else {
            provider.as_str()
        };
        assert_eq!(
            infer_provider(&selector),
            expected,
            "configured provider prefix must bypass generic model-shape inference"
        );
    }
}

#[test]
fn every_configured_provider_prefix_is_removed_during_resolution() {
    for (selector, model_id) in [
        ("local:gemma-4-e4b-it", "gemma-4-e4b-it"),
        ("ollama:qwen3:30b-a3b", "qwen3:30b-a3b"),
        ("ollama/gemma4:26b", "gemma4:26b"),
    ] {
        let model = resolve_model_info(selector);
        assert_eq!(model.id, model_id);
        assert_eq!(model.provider, "ollama");
    }

    let hf = resolve_model_info("hf:Qwen/Qwen3.6-35B-A3B");
    assert_eq!(hf.id, "Qwen/Qwen3.6-35B-A3B");
    assert_eq!(hf.provider, "huggingface");

    for selector in ["cerebras/gpt-oss-120b", "cerebras/zai-glm-4.7"] {
        let model = resolve_model_info(selector);
        assert_eq!(model.id, selector.trim_start_matches("cerebras/"));
        assert_eq!(model.provider, "cerebras");
    }

    for provider in provider_names() {
        let model = resolve_model_info(&format!("{provider}:catalog-native-model"));
        assert_eq!(model.id, "catalog-native-model");
        let expected = if provider == "local" {
            "ollama"
        } else {
            provider.as_str()
        };
        assert_eq!(model.provider, expected);
    }
}
