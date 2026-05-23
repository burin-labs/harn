use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ModelSelector {
    pub(crate) selector: String,
    pub(crate) provider: String,
    pub(crate) model: String,
}

pub(crate) fn resolve_selector(raw: &str) -> ModelSelector {
    let trimmed = raw.trim();
    if let Some((provider, model)) = parse_provider_model_kv(trimmed) {
        return ModelSelector {
            selector: trimmed.to_string(),
            provider,
            model,
        };
    }
    if let Some((provider, model)) = trimmed.split_once(':') {
        if !provider.is_empty() && !model.is_empty() {
            return ModelSelector {
                selector: trimmed.to_string(),
                provider: provider.to_string(),
                model: model.to_string(),
            };
        }
    }
    let resolved = harn_vm::llm_config::resolve_model_info(trimmed);
    ModelSelector {
        selector: trimmed.to_string(),
        provider: resolved.provider,
        model: resolved.id,
    }
}

fn parse_provider_model_kv(raw: &str) -> Option<(String, String)> {
    let mut provider = None;
    let mut model = None;
    for part in raw.split(',') {
        let (key, value) = part.split_once('=')?;
        match key.trim() {
            "provider" => provider = Some(value.trim().to_string()),
            "model" => model = Some(value.trim().to_string()),
            _ => {}
        }
    }
    match (provider, model) {
        (Some(provider), Some(model)) if !provider.is_empty() && !model.is_empty() => {
            Some((provider, model))
        }
        _ => None,
    }
}

pub(crate) fn selector_label(selector: &ModelSelector) -> String {
    format!("{}:{}", selector.provider, selector.model)
}

pub(crate) fn selector_is_local(selector: &ModelSelector) -> bool {
    matches!(
        selector.provider.as_str(),
        "ollama" | "llamacpp" | "mlx" | "local" | "vllm" | "tgi"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_accepts_key_value_and_colon_forms() {
        let kv = resolve_selector("provider=openrouter,model=google/gemma");
        assert_eq!(kv.provider, "openrouter");
        assert_eq!(kv.model, "google/gemma");

        let colon = resolve_selector("mock:mock");
        assert_eq!(colon.provider, "mock");
        assert_eq!(colon.model, "mock");
    }
}
