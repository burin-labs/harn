use super::*;

pub(super) struct RequestResolutionInputs<'a> {
    pub(super) options: Option<&'a crate::value::DictMap>,
    pub(super) model_defaults: &'a std::collections::BTreeMap<String, toml::Value>,
    pub(super) max_tokens: i64,
    pub(super) tool_format: &'a str,
    pub(super) tool_format_steer: Option<String>,
    pub(super) tools_declared: bool,
    pub(super) native_tool_count: usize,
    pub(super) thinking: &'a crate::llm::api::ThinkingConfig,
    pub(super) thinking_source: &'static str,
    pub(super) cache: bool,
    pub(super) stream: bool,
}

/// The per-call resolution receipt: each applied setting and the layer that
/// decided it (see [`crate::llm::api::ResolvedSetting`]).
pub(super) fn request_resolution(
    inputs: RequestResolutionInputs<'_>,
) -> Vec<crate::llm::api::ResolvedSetting> {
    use crate::llm::api::{ResolvedSetting, ThinkingConfig};
    let requested = |key: &str| {
        inputs
            .options
            .and_then(|options| options.get(key))
            .filter(|value| !matches!(value, VmValue::Nil))
            .map(VmValue::display)
    };
    let layer = |key: &str, caller: &'static str, catalog: &'static str| {
        if requested(key).is_some() {
            caller
        } else if inputs.model_defaults.contains_key(key) {
            catalog
        } else {
            "default"
        }
    };
    let mut rows = vec![ResolvedSetting {
        setting: "max_tokens",
        requested: requested("max_tokens"),
        applied: inputs.max_tokens.to_string(),
        source: layer(
            "max_tokens",
            "caller.max_tokens",
            "catalog.model_defaults.max_tokens",
        ),
        note: None,
    }];
    if inputs.tools_declared {
        let requested_format = requested("tool_format");
        rows.push(ResolvedSetting {
            setting: "tool_format",
            requested: requested_format.clone(),
            applied: inputs.tool_format.to_string(),
            source: if inputs.tool_format_steer.is_some() {
                "catalog.steer"
            } else if requested_format.is_some() {
                "caller.tool_format"
            } else {
                "catalog.preferred_tool_format"
            },
            note: inputs.tool_format_steer,
        });
        rows.push(ResolvedSetting {
            setting: "tool_wire",
            requested: None,
            applied: if inputs.native_tool_count > 0 {
                format!("native schemas: {}", inputs.native_tool_count)
            } else {
                "text contract in the system prompt".to_string()
            },
            source: "derived",
            note: None,
        });
    }
    rows.push(ResolvedSetting {
        setting: "reasoning",
        requested: requested("effort").or_else(|| requested("thinking")),
        applied: match inputs.thinking {
            ThinkingConfig::Disabled => "off".to_string(),
            ThinkingConfig::Enabled { budget_tokens } => match budget_tokens {
                Some(budget) => format!("enabled, budget {budget}"),
                None => "enabled, provider budget".to_string(),
            },
            ThinkingConfig::Adaptive => "adaptive".to_string(),
            ThinkingConfig::Effort { level } => format!("effort {}", level.as_str()),
        },
        source: inputs.thinking_source,
        note: None,
    });
    rows.push(ResolvedSetting {
        setting: "cache",
        requested: requested("cache"),
        applied: inputs.cache.to_string(),
        source: if requested("cache").is_some() {
            "caller.cache"
        } else {
            "catalog.prompt_caching"
        },
        note: None,
    });
    rows.push(ResolvedSetting {
        setting: "stream",
        requested: requested("stream"),
        applied: inputs.stream.to_string(),
        source: if requested("stream").is_some() {
            "caller.stream"
        } else {
            "default"
        },
        note: None,
    });
    rows
}
