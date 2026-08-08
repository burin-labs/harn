//! Per-dialect reasoning/thinking request shapes.
//!
//! OpenAI-compatible servers disagree on how a reasoning directive is spelled:
//! OpenRouter takes a `reasoning` object, MiniMax and Z.AI take `thinking`,
//! and several routes accept only a subset of the directives. The capability
//! row picks the dialect; these builders produce its body.

use crate::llm::api::ThinkingConfig;

/// True when the OpenRouter `reasoning` body explicitly disables reasoning
/// (`{"enabled": false}`). These are the directives that, on a model with no
/// reasoning support, cause OpenRouter to drop every endpoint under
/// `require_parameters: true`.
pub(super) fn is_openrouter_reasoning_disable(reasoning: &serde_json::Value) -> bool {
    reasoning
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
}

/// True when the (provider, model) capability row advertises ANY reasoning
/// support: a non-empty `thinking_modes` list, `reasoning_effort_supported`,
/// or `reasoning_none_supported`. When all three are absent the model declares
/// no reasoning capability and the `reasoning` request param must be omitted on
/// OpenRouter to avoid the empty-endpoint 404.
pub(super) fn model_declares_reasoning(caps: &crate::llm::capabilities::Capabilities) -> bool {
    !caps.thinking_modes.is_empty()
        || caps.reasoning_effort_supported
        || caps.reasoning_none_supported
}

pub(super) fn openrouter_reasoning_config(thinking: &ThinkingConfig) -> Option<serde_json::Value> {
    match thinking {
        // Explicitly disable on the wire. The previous `None` return left
        // the request silent, which on Qwen3 thinking variants caused the
        // model to fall through to its trained-default unbounded thinking
        // budget. OpenRouter universally honors `reasoning.enabled: false`
        // (verified empirically on qwen/qwen3.6-35b-a3b: 358ms with the
        // disable directive vs 1300ms+ without), so emit it.
        ThinkingConfig::Disabled => Some(serde_json::json!({
            "enabled": false
        })),
        ThinkingConfig::Enabled {
            budget_tokens: None,
        } => Some(serde_json::json!({
            "enabled": true
        })),
        ThinkingConfig::Enabled {
            budget_tokens: Some(max_tokens),
        } => Some(serde_json::json!({
            "max_tokens": max_tokens
        })),
        ThinkingConfig::Adaptive => Some(serde_json::json!({
            "enabled": true
        })),
        ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None,
        } => Some(serde_json::json!({
            "enabled": false
        })),
        ThinkingConfig::Effort { level } => Some(serde_json::json!({
            "effort": level.as_str()
        })),
    }
}

pub(super) fn enabled_reasoning_config(
    thinking: &ThinkingConfig,
    caps: &crate::llm::capabilities::Capabilities,
) -> Option<serde_json::Value> {
    let supports_enabled = caps.thinking_modes.iter().any(|mode| mode == "enabled");
    if !supports_enabled {
        return None;
    }
    match thinking {
        ThinkingConfig::Disabled
        | ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None,
        } => Some(serde_json::json!({ "enabled": false })),
        ThinkingConfig::Enabled { .. } | ThinkingConfig::Adaptive => {
            Some(serde_json::json!({ "enabled": true }))
        }
        ThinkingConfig::Effort { .. } => Some(serde_json::json!({ "enabled": true })),
    }
}

pub(super) fn minimax_thinking_config(thinking: &ThinkingConfig) -> Option<serde_json::Value> {
    match thinking {
        ThinkingConfig::Disabled
        | ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None,
        } => Some(serde_json::json!({ "type": "disabled" })),
        ThinkingConfig::Enabled { .. }
        | ThinkingConfig::Adaptive
        | ThinkingConfig::Effort { .. } => Some(serde_json::json!({ "type": "adaptive" })),
    }
}

pub(super) fn zai_thinking_config(thinking: &ThinkingConfig) -> Option<serde_json::Value> {
    match thinking {
        ThinkingConfig::Disabled
        | ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None,
        } => Some(serde_json::json!({ "type": "disabled" })),
        ThinkingConfig::Enabled { .. }
        | ThinkingConfig::Adaptive
        | ThinkingConfig::Effort { .. } => Some(serde_json::json!({ "type": "enabled" })),
    }
}
