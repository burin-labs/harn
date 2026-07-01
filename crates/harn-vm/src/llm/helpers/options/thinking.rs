use super::output::*;
use super::*;

pub(super) fn thinking_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message.into())))
}

pub(super) fn parse_reasoning_effort_field(
    field: &str,
    raw: &str,
) -> Result<crate::llm::api::ReasoningEffort, VmError> {
    match raw {
        "none" => Ok(crate::llm::api::ReasoningEffort::None),
        "minimal" => Ok(crate::llm::api::ReasoningEffort::Minimal),
        "low" => Ok(crate::llm::api::ReasoningEffort::Low),
        "medium" => Ok(crate::llm::api::ReasoningEffort::Medium),
        "high" => Ok(crate::llm::api::ReasoningEffort::High),
        "xhigh" => Ok(crate::llm::api::ReasoningEffort::XHigh),
        "max" => Ok(crate::llm::api::ReasoningEffort::Max),
        other => Err(thinking_error(format!(
            "{field}: expected \"none\" | \"minimal\" | \"low\" | \"medium\" | \"high\" | \"xhigh\" | \"max\", got \"{other}\""
        ))),
    }
}

pub(super) fn parse_reasoning_effort(
    raw: &str,
) -> Result<crate::llm::api::ReasoningEffort, VmError> {
    parse_reasoning_effort_field("thinking.level", raw)
}

pub(super) fn parse_reasoning_effort_option(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<crate::llm::api::ReasoningEffort>, VmError> {
    let Some(raw) = options.and_then(|o| o.get("reasoning_effort")) else {
        return Ok(None);
    };
    match raw {
        VmValue::Nil | VmValue::Bool(false) => Ok(None),
        VmValue::String(level) => parse_reasoning_effort_field("reasoning_effort", level).map(Some),
        other => Err(thinking_error(format!(
            "reasoning_effort: expected \"none\" | \"minimal\" | \"low\" | \"medium\" | \"high\" | \"xhigh\" | \"max\", got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn parse_thinking_budget(raw: Option<&VmValue>) -> Result<Option<u32>, VmError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if matches!(raw, VmValue::Nil) {
        return Ok(None);
    }
    let Some(value) = raw.as_int() else {
        return Err(thinking_error(
            "thinking.budget_tokens: expected a non-negative int",
        ));
    };
    u32::try_from(value)
        .map(Some)
        .map_err(|_| thinking_error("thinking.budget_tokens: expected a non-negative int"))
}

/// Parse the script-facing `thinking` option into a provider-agnostic shape.
///
/// New shape:
///   `{mode: "enabled", budget_tokens: 8000}`
///   `{mode: "adaptive"}`
///   `{mode: "effort", level: "high"}`
///
/// Legacy compatibility:
///   `true` => enabled with provider defaults
///   `{budget_tokens: N}` => enabled with a budget
///   `{enabled: false}` / `false` / `nil` => disabled
pub(super) fn parse_thinking_option(
    options: Option<&crate::value::DictMap>,
) -> Result<crate::llm::api::ThinkingConfig, VmError> {
    use crate::llm::api::ThinkingConfig;

    let Some(raw) = options.and_then(|o| o.get("thinking")) else {
        return Ok(ThinkingConfig::Disabled);
    };

    match raw {
        VmValue::Nil | VmValue::Bool(false) => Ok(ThinkingConfig::Disabled),
        VmValue::Bool(true) => Ok(ThinkingConfig::Enabled {
            budget_tokens: None,
        }),
        VmValue::String(s) => match s.as_str() {
            "disabled" | "off" | "none" => Ok(ThinkingConfig::Disabled),
            "enabled" | "on" | "true" => Ok(ThinkingConfig::Enabled {
                budget_tokens: None,
            }),
            "adaptive" => Ok(ThinkingConfig::Adaptive),
            "minimal" | "low" | "medium" | "high" | "xhigh" | "max" => Ok(ThinkingConfig::Effort {
                level: parse_reasoning_effort(s.as_str())?,
            }),
            other => Err(thinking_error(format!(
                "thinking: expected bool, dict, or one of \"enabled\" | \"adaptive\" | \"minimal\" | \"low\" | \"medium\" | \"high\" | \"xhigh\" | \"max\", got \"{other}\""
            ))),
        },
        VmValue::Dict(d) => {
            if d.get("enabled").is_some_and(|enabled| !enabled.is_truthy()) {
                return Ok(ThinkingConfig::Disabled);
            }

            let mode = d
                .get("mode")
                .and_then(|value| match value {
                    VmValue::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .unwrap_or("enabled");

            match mode {
                "disabled" | "off" | "none" => Ok(ThinkingConfig::Disabled),
                "enabled" => Ok(ThinkingConfig::Enabled {
                    budget_tokens: parse_thinking_budget(d.get("budget_tokens"))?,
                }),
                "adaptive" => Ok(ThinkingConfig::Adaptive),
                "effort" => {
                    let level = d
                        .get("level")
                        .and_then(|value| match value {
                            VmValue::String(s) => Some(s.as_str()),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            thinking_error(
                                "thinking.level is required when thinking.mode is \"effort\"",
                            )
                        })?;
                    Ok(ThinkingConfig::Effort {
                        level: parse_reasoning_effort(level)?,
                    })
                }
                other => Err(thinking_error(format!(
                    "thinking.mode: expected \"disabled\" | \"enabled\" | \"adaptive\" | \"effort\", got \"{other}\""
                ))),
            }
        }
        _ if raw.is_truthy() => Ok(ThinkingConfig::Enabled {
            budget_tokens: None,
        }),
        _ => Ok(ThinkingConfig::Disabled),
    }
}

pub(super) fn validate_thinking_supported(
    thinking: &crate::llm::api::ThinkingConfig,
    provider: &str,
    model: &str,
    supported_modes: &[String],
    option_name: &str,
) -> Result<(), VmError> {
    use crate::llm::api::ThinkingConfig;

    if thinking.is_disabled() {
        return Ok(());
    }
    let supports = |mode: &str| supported_modes.iter().any(|supported| supported == mode);
    let supported = match thinking {
        ThinkingConfig::Disabled => true,
        // `enabled` remains compatible with Anthropic Opus 4.7+ where
        // providers/anthropic.rs rewrites it to adaptive thinking.
        ThinkingConfig::Enabled { .. } => supports("enabled") || supports("adaptive"),
        ThinkingConfig::Adaptive => supports("adaptive"),
        ThinkingConfig::Effort { .. } => supports("effort"),
    };
    if supported {
        return Ok(());
    }
    Err(unsupported_option_error(option_name, provider, model))
}

pub(super) fn validate_reasoning_effort_level_supported(
    thinking: &crate::llm::api::ThinkingConfig,
    provider: &str,
    model: &str,
    caps: &crate::llm::capabilities::Capabilities,
    option_name: &str,
) -> Result<(), VmError> {
    let crate::llm::api::ThinkingConfig::Effort { level } = thinking else {
        return Ok(());
    };
    if caps.reasoning_effort_levels.is_empty() {
        return Ok(());
    }
    let raw = level.as_str();
    if caps
        .reasoning_effort_levels
        .iter()
        .any(|supported| supported == raw)
    {
        return Ok(());
    }
    let supported = caps.reasoning_effort_levels.join(", ");
    Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "option `{option_name}` level `{raw}` is not supported for provider `{provider}` model `{model}`; supported reasoning_effort values: {supported}"
    )))))
}

pub(super) fn parse_anthropic_beta_features_option(
    options: Option<&crate::value::DictMap>,
    thinking: &crate::llm::api::ThinkingConfig,
    provider: &str,
    model: &str,
    enforce_capability_gates: bool,
) -> Result<Vec<String>, VmError> {
    let mut features = Vec::new();
    if let Some(raw) = options.and_then(|o| o.get("anthropic_beta_features")) {
        match raw {
            VmValue::Nil | VmValue::Bool(false) => {}
            VmValue::String(feature) => {
                let feature = feature.as_str().trim();
                if !feature.is_empty() {
                    validate_anthropic_beta_feature_name(feature)?;
                    crate::llm::api::push_unique_anthropic_beta_feature(&mut features, feature);
                }
            }
            VmValue::List(list) => {
                for item in list.iter() {
                    match item {
                        VmValue::String(feature) => {
                            let feature = feature.as_str().trim();
                            if !feature.is_empty() {
                                validate_anthropic_beta_feature_name(feature)?;
                                crate::llm::api::push_unique_anthropic_beta_feature(
                                    &mut features,
                                    feature,
                                );
                            }
                        }
                        other => {
                            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                                format!(
                                    "anthropic_beta_features: expected list<string>, got {}",
                                    other.type_name()
                                ),
                            ))));
                        }
                    }
                }
            }
            other => {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                        "anthropic_beta_features: expected string or list<string>, got {}",
                        other.type_name()
                    ),
                ))));
            }
        }
    }

    let explicit_interleaved = options
        .and_then(|o| o.get("interleaved_thinking"))
        .is_some_and(|value| value.is_truthy());
    let caps = crate::llm::capabilities::lookup(provider, model);
    if enforce_capability_gates && explicit_interleaved && !caps.interleaved_thinking_supported {
        return Err(unsupported_option_error(
            "interleaved_thinking",
            provider,
            model,
        ));
    }
    if explicit_interleaved {
        crate::llm::api::push_unique_anthropic_beta_feature(
            &mut features,
            crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA,
        );
    }

    if matches!(
        thinking,
        crate::llm::api::ThinkingConfig::Enabled { .. } | crate::llm::api::ThinkingConfig::Adaptive
    ) && caps.interleaved_thinking_supported
    {
        crate::llm::api::push_unique_anthropic_beta_feature(
            &mut features,
            crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA,
        );
    }

    Ok(features)
}

pub(super) fn validate_anthropic_beta_feature_name(feature: &str) -> Result<(), VmError> {
    if feature
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Ok(());
    }
    Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "anthropic_beta_features: invalid beta feature name `{feature}`; expected ASCII letters, digits, '-' or '_'"
    )))))
}
