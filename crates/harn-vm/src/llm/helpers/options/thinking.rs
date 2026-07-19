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
    let Some(raw) = options.and_then(|o| o.get("effort")) else {
        return Ok(None);
    };
    match raw {
        VmValue::Nil | VmValue::Bool(false) => Ok(None),
        VmValue::String(level) => parse_reasoning_effort_field("effort", level).map(Some),
        other => Err(thinking_error(format!(
            "effort: expected \"none\" | \"minimal\" | \"low\" | \"medium\" | \"high\" | \"xhigh\" | \"max\", got {}",
            other.type_name()
        ))),
    }
}

#[derive(Clone, Copy)]
enum ThinkingSource {
    Effort,
    ReasoningPolicy,
    Thinking,
}

impl ThinkingSource {
    fn option_name(self) -> &'static str {
        match self {
            Self::Effort => "effort",
            Self::ReasoningPolicy => "reasoning_policy",
            Self::Thinking => "thinking",
        }
    }
}

fn default_reasoning_effort(
    model_defaults: &std::collections::BTreeMap<String, toml::Value>,
) -> Result<Option<crate::llm::api::ReasoningEffort>, VmError> {
    let Some(raw) = model_defaults.get("reasoning_effort") else {
        return Ok(None);
    };
    let Some(level) = raw.as_str() else {
        return Err(thinking_error(
            "model_defaults.reasoning_effort: expected a string",
        ));
    };
    parse_reasoning_effort_field("model_defaults.reasoning_effort", level).map(Some)
}

/// Resolve one effective provider-agnostic thinking shape.
///
/// Explicit call options and reasoning policy own the decision. Catalog
/// defaults apply only when neither surface made a choice, then pass through
/// the same capability and supported-level validation as explicit effort.
pub(crate) fn resolve_thinking_config(
    options: Option<&crate::value::DictMap>,
    model_defaults: &std::collections::BTreeMap<String, toml::Value>,
    provider: &str,
    model: &str,
    caps: &crate::llm::capabilities::Capabilities,
    enforce_capability_gates: bool,
) -> Result<crate::llm::api::ThinkingConfig, VmError> {
    let policy =
        crate::llm::reasoning_policy::resolve_for_llm_call(options, provider, model, caps)?;
    resolve_thinking_config_with_policy(
        options,
        model_defaults,
        provider,
        model,
        caps,
        enforce_capability_gates,
        policy,
    )
}

/// Resolve catalog defaults without inheriting ambient session policy.
pub(crate) fn resolve_catalog_thinking_config(
    model_defaults: &std::collections::BTreeMap<String, toml::Value>,
    provider: &str,
    model: &str,
    caps: &crate::llm::capabilities::Capabilities,
    enforce_capability_gates: bool,
) -> Result<crate::llm::api::ThinkingConfig, VmError> {
    resolve_thinking_config_with_policy(
        None,
        model_defaults,
        provider,
        model,
        caps,
        enforce_capability_gates,
        None,
    )
}

fn resolve_thinking_config_with_policy(
    options: Option<&crate::value::DictMap>,
    model_defaults: &std::collections::BTreeMap<String, toml::Value>,
    provider: &str,
    model: &str,
    caps: &crate::llm::capabilities::Capabilities,
    enforce_capability_gates: bool,
    policy: Option<crate::llm::reasoning_policy::ReasoningPolicyApplication>,
) -> Result<crate::llm::api::ThinkingConfig, VmError> {
    let explicit_effort = parse_reasoning_effort_option(options)?;
    let has_effort_option = options.is_some_and(|opts| opts.contains_key("effort"));
    let has_thinking_option = options.is_some_and(|opts| opts.contains_key("thinking"));
    let catalog_effort = if explicit_effort.is_none()
        && !has_effort_option
        && !has_thinking_option
        && policy.is_none()
    {
        default_reasoning_effort(model_defaults)?
    } else {
        None
    };
    let effort = explicit_effort.or(catalog_effort);
    let (thinking, source) = if let Some(level) = effort {
        if options
            .and_then(|opts| opts.get("thinking"))
            .is_some_and(|value| value.is_truthy())
        {
            return Err(thinking_error(
                "effort cannot be combined with a non-disabled thinking option",
            ));
        }
        (
            crate::llm::api::ThinkingConfig::Effort { level },
            ThinkingSource::Effort,
        )
    } else if let Some(application) = policy {
        (application.thinking, ThinkingSource::ReasoningPolicy)
    } else {
        (parse_thinking_option(options)?, ThinkingSource::Thinking)
    };

    let effort_requires_provider_support = matches!(
        thinking,
        crate::llm::api::ThinkingConfig::Effort { level }
            if level != crate::llm::api::ReasoningEffort::None
    );
    if enforce_capability_gates
        && matches!(source, ThinkingSource::Effort)
        && effort_requires_provider_support
        && !caps.reasoning_effort_supported
    {
        return Err(unsupported_option_error(
            source.option_name(),
            provider,
            model,
        ));
    }
    if enforce_capability_gates {
        validate_thinking_supported(
            &thinking,
            provider,
            model,
            &caps.thinking_modes,
            source.option_name(),
        )?;
        validate_reasoning_effort_level_supported(
            &thinking,
            provider,
            model,
            caps,
            source.option_name(),
        )?;
    }
    Ok(thinking)
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
/// Author-facing grammar (one spelling per concept — reasoning *level* lives on
/// the sibling `effort` option, never inside `thinking`):
///   `true` / `false`         => enabled with provider defaults / disabled
///   `"adaptive"`             => provider-managed adaptive thinking
///   `{budget_tokens: N}`     => enabled with an explicit token budget
///
/// Internal shape: [`crate::llm::reasoning_policy`] lowers a resolved policy to
/// the tagged `{mode: "disabled" | "enabled" | "adaptive" | "effort", ...}`
/// dict and re-feeds it here, so the `mode`-tagged dict is still accepted. It is
/// the round-trip encoding of a [`ThinkingConfig`], not an author surface.
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
            "adaptive" => Ok(ThinkingConfig::Adaptive),
            other => Err(thinking_error(format!(
                "thinking: string value \"{other}\" is not accepted — use `thinking: true`/`false`, \
                 `thinking: \"adaptive\"`, or `thinking: {{budget_tokens: N}}`; for a reasoning-effort \
                 level use the `effort` option (e.g. `effort: \"high\"`)"
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

#[cfg(test)]
mod thinking_value_grammar_tests {
    use super::*;

    fn thinking_options(value: VmValue) -> crate::value::DictMap {
        let mut opts = crate::value::DictMap::new();
        opts.insert(crate::value::intern_key("thinking"), value);
        opts
    }

    fn thrown_message(err: VmError) -> String {
        match err {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => panic!("expected a thrown string error, got {other:?}"),
        }
    }

    #[test]
    fn thinking_effort_level_string_is_rejected_with_actionable_message() {
        // The reasoning *level* moved to the sibling `effort` option; a bare
        // effort-level string on `thinking` is no longer a silent synonym.
        let opts = thinking_options(VmValue::String(arcstr::ArcStr::from("high")));
        let message = thrown_message(
            parse_thinking_option(Some(&opts)).expect_err("effort-level string must be rejected"),
        );
        assert!(
            message.contains("string value \"high\" is not accepted"),
            "{message}"
        );
        assert!(message.contains("thinking: \"adaptive\""), "{message}");
        assert!(message.contains("effort: \"high\""), "{message}");
    }

    #[test]
    fn thinking_on_off_synonym_strings_are_rejected() {
        for raw in ["on", "off", "enabled", "disabled", "none", "true", "false"] {
            let opts = thinking_options(VmValue::String(arcstr::ArcStr::from(raw)));
            assert!(
                parse_thinking_option(Some(&opts)).is_err(),
                "thinking string {raw:?} should be rejected"
            );
        }
    }

    #[test]
    fn thinking_still_accepts_adaptive_bool_and_budget_forms() {
        let adaptive = thinking_options(VmValue::String(arcstr::ArcStr::from("adaptive")));
        assert_eq!(
            parse_thinking_option(Some(&adaptive)).expect("adaptive accepted"),
            crate::llm::api::ThinkingConfig::Adaptive
        );

        let enabled = thinking_options(VmValue::Bool(true));
        assert_eq!(
            parse_thinking_option(Some(&enabled)).expect("true accepted"),
            crate::llm::api::ThinkingConfig::Enabled {
                budget_tokens: None
            }
        );

        let disabled = thinking_options(VmValue::Bool(false));
        assert_eq!(
            parse_thinking_option(Some(&disabled)).expect("false accepted"),
            crate::llm::api::ThinkingConfig::Disabled
        );

        let budget = thinking_options(VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("budget_tokens"),
            VmValue::Int(8000),
        )])));
        assert_eq!(
            parse_thinking_option(Some(&budget)).expect("budget accepted"),
            crate::llm::api::ThinkingConfig::Enabled {
                budget_tokens: Some(8000)
            }
        );
    }
}
