//! Selector resolution: turn an alias or provider/model selector into the
//! complete `ResolvedModel` identity (provider, normalized id, tool format,
//! tier, family, lineage).
use serde::Serialize;

use super::*;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResolvedModel {
    pub id: String,
    pub provider: String,
    pub alias: Option<String>,
    pub tool_format: String,
    pub tier: String,
    pub family: String,
    pub lineage: String,
}

/// Resolve a model alias to (model_id, provider_name).
pub fn resolve_model(alias: &str) -> (String, Option<String>) {
    let config = effective_config();
    if let Some(a) = config.aliases.get(alias) {
        return (a.id.clone(), Some(a.provider.clone()));
    }
    (normalize_model_id(alias), None)
}

/// Strip host/provider selector prefixes that identify transport, not the
/// provider-native model id. This mirrors the host's existing normalization so
/// `ollama:qwen3:30b` reaches Ollama as `qwen3:30b` instead of an invalid
/// model named `ollama`. Cerebras follows the same convention but uses a
/// slash separator (`cerebras/gpt-oss-120b`) because its own /v1/models
/// endpoint returns bare names that overlap OpenAI's families.
pub fn normalize_model_id(raw: &str) -> String {
    for prefix in PROVIDER_SELECTOR_PREFIXES {
        if let Some(stripped) = raw.strip_prefix(prefix) {
            return stripped.to_string();
        }
    }
    raw.to_string()
}

const PROVIDER_SELECTOR_PREFIXES: &[&str] =
    &["ollama:", "local:", "huggingface:", "hf:", "cerebras/"];

/// Resolve an alias or selector into the complete catalog identity hosts need:
/// provider inference, prefix-normalized model id, default tool format, and tier.
pub fn resolve_model_info(selector: &str) -> ResolvedModel {
    let config = effective_config();
    if let Some(alias) = config.aliases.get(selector) {
        let id = alias.id.clone();
        let provider = alias.provider.clone();
        let requested = alias
            .tool_format
            .clone()
            .unwrap_or_else(|| default_tool_format_with_config(&config, &id, &provider));
        let tool_format = guard_tool_format(&provider, &id, &requested, Some(selector));
        return ResolvedModel {
            tier: model_tier_with_config(&config, &id),
            family: model_family_with_config(&config, &provider, &id),
            lineage: model_lineage_with_config(&config, &provider, &id),
            id,
            provider,
            alias: Some(selector.to_string()),
            tool_format,
        };
    }

    let id = normalize_model_id(selector);
    let inference = infer_provider_with_config(&config, selector);
    let source = inference.source;
    let provider = inference.provider;
    let requested = default_tool_format_with_config(&config, &id, &provider);
    let tool_format = guard_tool_format(&provider, &id, &requested, None);
    let tier = model_tier_with_config(&config, &id);
    let family = model_family_with_inference_source(&config, &provider, &id, source);
    let lineage = model_lineage_with_inference_source(&config, &provider, &id, source);
    ResolvedModel {
        id,
        provider,
        alias: None,
        tool_format,
        tier,
        family,
        lineage,
    }
}

/// Run the requested `tool_format` through the capability registry's
/// dialect-validity gate, returning the safe format to actually use. When the
/// registry auto-corrects a known-broken combo (e.g. a `native` pin on a
/// `native_unreliable` route that silently drops to unparsed DSML text), the
/// correction is logged once at resolution time so a harness developer sees
/// *why* their pinned format was not honored — never a silent vanishing.
fn guard_tool_format(provider: &str, model: &str, requested: &str, alias: Option<&str>) -> String {
    let decision = crate::llm::capabilities::validate_tool_format(provider, model, requested);
    if let Some(reason) = &decision.correction {
        tracing::warn!(
            target: "harn::llm::tool_format",
            alias = alias.unwrap_or(""),
            "{reason}"
        );
    }
    decision.effective
}

#[cfg(test)]
mod tests {
    use super::resolve_model_info;

    #[test]
    fn grok_code_aliases_resolve_through_live_resolver() {
        for selector in ["grok-code", "grok-code-fast", "grok-code-fast-1"] {
            let model = resolve_model_info(selector);
            assert_eq!(
                (
                    model.id.as_str(),
                    model.provider.as_str(),
                    model.alias.as_deref(),
                    model.tool_format.as_str(),
                ),
                ("grok-build-0.1", "xai", Some(selector), "native"),
                "selector: {selector}",
            );
        }
    }
}
