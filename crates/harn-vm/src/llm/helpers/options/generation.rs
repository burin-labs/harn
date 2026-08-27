//! Typed parsing and final-route validation for generation controls.

use super::*;

pub(super) fn generation_option_error(option: &str, detail: impl std::fmt::Display) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "llm_call: invalid `{option}`: {detail}"
    ))))
}

fn reject_unknown_record_fields(
    option: &str,
    record: &crate::value::DictMap,
    allowed: &[&str],
) -> Result<(), VmError> {
    if let Some(field) = record
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(generation_option_error(
            option,
            format!("unknown field `{field}`"),
        ));
    }
    Ok(())
}

pub(super) fn is_first_class_generation_wire_key(key: &str) -> bool {
    matches!(
        key,
        "max_tokens"
            | "max_completion_tokens"
            | "maxOutputTokens"
            | "num_predict"
            | "temperature"
            | "top_p"
            | "topP"
            | "top_k"
            | "topK"
            | "logprobs"
            | "responseLogprobs"
            | "response_logprobs"
            | "top_logprobs"
            | "logit_bias"
            | "min_p"
            | "repetition_penalty"
            | "repeat_penalty"
            | "prediction"
            | "verbosity"
            | "mirostat"
            | "mirostat_tau"
            | "mirostat_eta"
            | "stop"
            | "stopSequences"
            | "seed"
            | "frequency_penalty"
            | "frequencyPenalty"
            | "presence_penalty"
            | "presencePenalty"
            | "parallel_tool_calls"
            | "disable_parallel_tool_use"
    )
}

pub(super) fn first_class_generation_wire_path(
    overrides: &crate::value::DictMap,
) -> Option<String> {
    if let Some(key) = overrides
        .keys()
        .find(|key| is_first_class_generation_wire_key(key))
    {
        return Some(key.to_string());
    }
    for container in ["options", "generationConfig", "generation_config"] {
        let Some(fields) = overrides.get(container).and_then(VmValue::as_dict) else {
            continue;
        };
        if let Some(key) = fields
            .keys()
            .find(|key| is_first_class_generation_wire_key(key))
        {
            return Some(format!("{container}.{key}"));
        }
    }
    if overrides
        .get("text")
        .and_then(VmValue::as_dict)
        .is_some_and(|text| text.contains_key("verbosity"))
    {
        return Some("text.verbosity".to_string());
    }
    if overrides
        .get("tool_choice")
        .and_then(VmValue::as_dict)
        .is_some_and(|choice| choice.contains_key("disable_parallel_tool_use"))
    {
        return Some("tool_choice.disable_parallel_tool_use".to_string());
    }
    None
}

pub(super) fn parse_logprobs(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<crate::llm::api::LogprobsConfig>, VmError> {
    match options.and_then(|options| options.get("logprobs")) {
        None | Some(VmValue::Nil) | Some(VmValue::Bool(false)) => Ok(None),
        Some(VmValue::Bool(true)) => Ok(Some(crate::llm::api::LogprobsConfig { top: None })),
        Some(VmValue::Dict(config)) => {
            reject_unknown_record_fields("logprobs", config, &["top"])?;
            let top = match config.get("top") {
                None | Some(VmValue::Nil) => None,
                Some(VmValue::Int(value)) if (0..=20).contains(value) => Some(*value as u8),
                Some(VmValue::Int(_)) => {
                    return Err(generation_option_error("logprobs", "`top` must be 0..=20"));
                }
                Some(value) => {
                    return Err(generation_option_error(
                        "logprobs",
                        format!("`top` must be an int, got {}", value.type_name()),
                    ));
                }
            };
            Ok(Some(crate::llm::api::LogprobsConfig { top }))
        }
        Some(value) => Err(generation_option_error(
            "logprobs",
            format!("expected bool or {{top?: int}}, got {}", value.type_name()),
        )),
    }
}

pub(super) fn parse_logit_bias(
    options: Option<&crate::value::DictMap>,
    provider: &str,
    model: &str,
) -> Result<Vec<crate::llm::api::TokenBias>, VmError> {
    let Some(value) = options.and_then(|options| options.get("logit_bias")) else {
        return Ok(Vec::new());
    };
    let VmValue::List(entries) = value else {
        return Err(generation_option_error(
            "logit_bias",
            "expected a list of {token: TokenRef, bias: number}",
        ));
    };
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let expected_tokenizer = crate::llm::token_count::exact_tokenizer_identity_for_model(model)
        .map_err(|error| {
            generation_option_error(
                "logit_bias",
                format!("route `{provider}:{model}` cannot verify token IDs: {error}"),
            )
        })?;
    let mut seen = std::collections::BTreeSet::new();
    let mut parsed = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let VmValue::Dict(entry) = entry else {
            return Err(generation_option_error(
                "logit_bias",
                format!("entry {index} is not a TokenBias record"),
            ));
        };
        reject_unknown_record_fields("logit_bias", entry, &["token", "bias"])?;
        let Some(VmValue::Dict(token)) = entry.get("token") else {
            return Err(generation_option_error(
                "logit_bias",
                format!("entry {index} has no TokenRef in `token`"),
            ));
        };
        reject_unknown_record_fields(
            "logit_bias",
            token,
            &["_type", "id", "tokenizer", "bytes", "text"],
        )?;
        if !matches!(token.get("_type"), Some(VmValue::String(kind)) if kind.as_str() == "llm_token")
        {
            return Err(generation_option_error(
                "logit_bias",
                format!("entry {index} is not tagged as a TokenRef"),
            ));
        }
        let token_id = match token.get("id") {
            Some(VmValue::Int(id)) => u32::try_from(*id).map_err(|_| {
                generation_option_error(
                    "logit_bias",
                    format!("entry {index} has an out-of-range token id"),
                )
            })?,
            _ => {
                return Err(generation_option_error(
                    "logit_bias",
                    format!("entry {index} has no integer token id"),
                ));
            }
        };
        let tokenizer = match token.get("tokenizer") {
            Some(VmValue::String(tokenizer)) => tokenizer.to_string(),
            _ => {
                return Err(generation_option_error(
                    "logit_bias",
                    format!("entry {index} has no string tokenizer identity"),
                ));
            }
        };
        let Some(VmValue::List(bytes)) = token.get("bytes") else {
            return Err(generation_option_error(
                "logit_bias",
                format!("entry {index} has no token byte sequence"),
            ));
        };
        if bytes
            .iter()
            .any(|byte| !matches!(byte, VmValue::Int(value) if (0..=255).contains(value)))
        {
            return Err(generation_option_error(
                "logit_bias",
                format!("entry {index} token bytes must be integers within 0..=255"),
            ));
        }
        if !matches!(
            token.get("text"),
            Some(VmValue::String(_)) | Some(VmValue::Nil)
        ) {
            return Err(generation_option_error(
                "logit_bias",
                format!("entry {index} token text must be a string or nil"),
            ));
        }
        if tokenizer != expected_tokenizer {
            return Err(generation_option_error(
                "logit_bias",
                format!(
                    "entry {index} uses `{tokenizer}`, but route `{provider}:{model}` uses `{expected_tokenizer}`"
                ),
            ));
        }
        if !seen.insert(token_id) {
            return Err(generation_option_error(
                "logit_bias",
                format!("token id {token_id} appears more than once"),
            ));
        }
        let bias = match entry.get("bias") {
            Some(VmValue::Float(value)) => *value,
            Some(VmValue::Int(value)) => *value as f64,
            _ => {
                return Err(generation_option_error(
                    "logit_bias",
                    format!("entry {index} has no numeric bias"),
                ));
            }
        };
        if !bias.is_finite() || !(-100.0..=100.0).contains(&bias) {
            return Err(generation_option_error(
                "logit_bias",
                format!("entry {index} bias must be finite and within -100..=100"),
            ));
        }
        parsed.push(crate::llm::api::TokenBias {
            token_id,
            tokenizer,
            bias,
        });
    }
    Ok(parsed)
}

fn validate_token_bias_route(
    entries: &[crate::llm::api::TokenBias],
    provider: &str,
    model: &str,
) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let expected = crate::llm::token_count::exact_tokenizer_identity_for_model(model)
        .map_err(|error| format!("route `{provider}:{model}` cannot verify token IDs: {error}"))?;
    if let Some(entry) = entries.iter().find(|entry| entry.tokenizer != expected) {
        return Err(format!(
            "token id {} uses `{}`, but route `{provider}:{model}` uses `{expected}`",
            entry.token_id, entry.tokenizer
        ));
    }
    Ok(())
}

pub(super) fn parse_prediction(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<String>, VmError> {
    let Some(value) = options.and_then(|options| options.get("prediction")) else {
        return Ok(None);
    };
    let Some(prediction) = value.as_dict() else {
        return Err(generation_option_error(
            "prediction",
            "expected {content: <non-empty string>}",
        ));
    };
    reject_unknown_record_fields("prediction", prediction, &["content"])?;
    match prediction.get("content") {
        Some(VmValue::String(content)) if !content.is_empty() => Ok(Some(content.to_string())),
        _ => Err(generation_option_error(
            "prediction",
            "expected {content: <non-empty string>}",
        )),
    }
}

pub(super) fn parse_verbosity(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<crate::llm::api::Verbosity>, VmError> {
    let Some(value) = options.and_then(|options| options.get("verbosity")) else {
        return Ok(None);
    };
    let VmValue::String(value) = value else {
        return Err(generation_option_error(
            "verbosity",
            "expected `low`, `medium`, or `high`",
        ));
    };
    match value.as_str() {
        "low" => Ok(Some(crate::llm::api::Verbosity::Low)),
        "medium" => Ok(Some(crate::llm::api::Verbosity::Medium)),
        "high" => Ok(Some(crate::llm::api::Verbosity::High)),
        _ => Err(generation_option_error(
            "verbosity",
            "expected `low`, `medium`, or `high`",
        )),
    }
}

pub(super) fn parse_mirostat(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<crate::llm::api::MirostatConfig>, VmError> {
    let Some(value) = options.and_then(|options| options.get("mirostat")) else {
        return Ok(None);
    };
    let Some(config) = value.as_dict() else {
        return Err(generation_option_error(
            "mirostat",
            "expected a config record",
        ));
    };
    reject_unknown_record_fields(
        "mirostat",
        config,
        &["version", "target_entropy", "learning_rate"],
    )?;
    let version = match config.get("version") {
        Some(VmValue::Int(1)) => 1,
        Some(VmValue::Int(2)) => 2,
        _ => {
            return Err(generation_option_error(
                "mirostat",
                "`version` must be 1 or 2",
            ));
        }
    };
    let number = |field: &str, default: f64| -> Result<f64, VmError> {
        match config.get(field) {
            None | Some(VmValue::Nil) => Ok(default),
            Some(VmValue::Float(value)) => Ok(*value),
            Some(VmValue::Int(value)) => Ok(*value as f64),
            Some(value) => Err(generation_option_error(
                "mirostat",
                format!("`{field}` must be numeric, got {}", value.type_name()),
            )),
        }
    };
    let target_entropy = number("target_entropy", 5.0)?;
    let learning_rate = number("learning_rate", 0.1)?;
    if !target_entropy.is_finite() || target_entropy <= 0.0 {
        return Err(generation_option_error(
            "mirostat",
            "`target_entropy` must be finite and positive",
        ));
    }
    if !learning_rate.is_finite() || !(0.0..=1.0).contains(&learning_rate) || learning_rate == 0.0 {
        return Err(generation_option_error(
            "mirostat",
            "`learning_rate` must be finite and within (0, 1]",
        ));
    }
    Ok(Some(crate::llm::api::MirostatConfig {
        version,
        target_entropy,
        learning_rate,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_generation_ranges(
    max_tokens: i64,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    min_p: Option<f64>,
    repetition_penalty: Option<f64>,
    frequency_penalty: Option<f64>,
    presence_penalty: Option<f64>,
) -> Result<(), VmError> {
    if max_tokens <= 0 {
        return Err(generation_option_error("max_tokens", "must be positive"));
    }
    for (name, value, range) in [
        ("temperature", temperature, 0.0..=2.0),
        ("top_p", top_p, 0.0..=1.0),
        ("min_p", min_p, 0.0..=1.0),
        ("frequency_penalty", frequency_penalty, -2.0..=2.0),
        ("presence_penalty", presence_penalty, -2.0..=2.0),
    ] {
        if value.is_some_and(|value| !value.is_finite() || !range.contains(&value)) {
            return Err(generation_option_error(
                name,
                format!(
                    "must be finite and within {}..={}",
                    range.start(),
                    range.end()
                ),
            ));
        }
    }
    if top_k.is_some_and(|value| value < 0) {
        return Err(generation_option_error("top_k", "must be non-negative"));
    }
    if repetition_penalty.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        return Err(generation_option_error(
            "repetition_penalty",
            "must be finite and positive",
        ));
    }
    Ok(())
}

/// Gemini Interactions currently rejects log probabilities on its streaming
/// transport. Preserve an explicit stream request as an error, while letting
/// `logprobs` select the unary transport when streaming was only the default.
pub(super) fn normalize_generation_stream(
    stream: bool,
    stream_explicit: bool,
    provider: &str,
    model: &str,
    logprobs: bool,
) -> Result<bool, VmError> {
    let interactions_logprobs = logprobs
        && provider.eq_ignore_ascii_case("gemini")
        && crate::llm::capabilities::lookup(provider, model).live_endpoint_family
            == Some(crate::llm::capabilities::LiveEndpointFamily::GeminiInteractions);
    if !interactions_logprobs {
        return Ok(stream);
    }
    if stream && stream_explicit {
        return Err(generation_option_error(
            "stream",
            "Gemini Interactions cannot combine `stream: true` with `logprobs`",
        ));
    }
    Ok(false)
}

/// Reject caller-selected controls that the final route cannot represent.
pub(crate) fn validate_options(opts: &crate::llm::api::LlmCallOptions) -> Result<(), VmError> {
    validate_token_bias_route(&opts.logit_bias, &opts.provider, &opts.model).map_err(|detail| {
        crate::llm::call::invalid_request_error(
            format!("option `logit_bias` is invalid for the resolved route: {detail}"),
            &opts.provider,
            &opts.model,
        )
    })?;
    if opts.provider.eq_ignore_ascii_case("cerebras")
        && opts.logprobs.is_some()
        && opts.prediction.is_some()
    {
        return Err(crate::llm::call::invalid_request_error(
            "Cerebras cannot combine `logprobs` with `prediction`; choose one",
            &opts.provider,
            &opts.model,
        ));
    }
    if opts.logprobs.is_some()
        && opts.stream
        && opts.provider.eq_ignore_ascii_case("gemini")
        && crate::llm::capabilities::lookup(&opts.provider, &opts.model).live_endpoint_family
            == Some(crate::llm::capabilities::LiveEndpointFamily::GeminiInteractions)
    {
        return Err(crate::llm::call::invalid_request_error(
            "Gemini Interactions cannot combine `stream: true` with `logprobs`; set `stream: false`",
            &opts.provider,
            &opts.model,
        ));
    }
    if opts.api_mode == crate::llm::api::LlmApiMode::Responses {
        for option in &opts.portable_option_intent {
            if matches!(
                option,
                crate::llm::capabilities::PortableOption::TopK
                    | crate::llm::capabilities::PortableOption::Seed
                    | crate::llm::capabilities::PortableOption::FrequencyPenalty
                    | crate::llm::capabilities::PortableOption::PresencePenalty
                    | crate::llm::capabilities::PortableOption::Stop
                    | crate::llm::capabilities::PortableOption::LogitBias
                    | crate::llm::capabilities::PortableOption::MinP
                    | crate::llm::capabilities::PortableOption::RepetitionPenalty
                    | crate::llm::capabilities::PortableOption::Prediction
                    | crate::llm::capabilities::PortableOption::Mirostat
            ) {
                return Err(crate::llm::call::invalid_request_error(
                    format!(
                        "option `{}` is not representable by the OpenAI Responses API; use `api_mode: \"chat_completions\"` or remove it",
                        option.name()
                    ),
                    &opts.provider,
                    &opts.model,
                ));
            }
        }
    }
    for option in &opts.portable_option_intent {
        let admitted = if *option == crate::llm::capabilities::PortableOption::PromptCacheTtl {
            let ttl = opts
                .prompt_cache_ttl
                .expect("prompt-cache TTL intent has a parsed value");
            crate::llm::capabilities::admit_prompt_cache_ttl(
                &opts.provider,
                &opts.model,
                ttl.as_str(),
            )
        } else {
            crate::llm::capabilities::admit_portable_option_for_thinking(
                &opts.provider,
                &opts.model,
                &opts.thinking,
                *option,
            )
        };
        admitted.map_err(|error| {
            crate::llm::call::invalid_request_error(error.to_string(), &opts.provider, &opts.model)
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        first_class_generation_wire_path, normalize_generation_stream, parse_logit_bias,
        parse_logprobs, validate_options,
    };
    use crate::value::{DictMap, VmDictExt, VmError, VmValue};

    fn token_ref(id: i64, tokenizer: &str) -> VmValue {
        let mut token = DictMap::new();
        token.put_str("_type", "llm_token");
        token.put("id", VmValue::Int(id));
        token.put_str("tokenizer", tokenizer);
        token.put("bytes", VmValue::List(std::sync::Arc::new(Vec::new())));
        token.put("text", VmValue::Nil);
        VmValue::dict(token)
    }

    fn bias(token: VmValue, value: f64) -> VmValue {
        let mut bias = DictMap::new();
        bias.put("token", token);
        bias.put("bias", VmValue::Float(value));
        VmValue::dict(bias)
    }

    fn message(error: VmError) -> String {
        match error {
            VmError::Thrown(VmValue::String(message)) => message.to_string(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn provider_overrides_cannot_hide_typed_controls_in_wire_containers() {
        let mut ollama_options = DictMap::new();
        ollama_options.put("repeat_penalty", VmValue::Float(1.2));
        let mut ollama = DictMap::new();
        ollama.put("options", VmValue::dict(ollama_options));
        assert_eq!(
            first_class_generation_wire_path(&ollama).as_deref(),
            Some("options.repeat_penalty")
        );

        let mut text = DictMap::new();
        text.put_str("verbosity", "high");
        let mut responses = DictMap::new();
        responses.put("text", VmValue::dict(text));
        assert_eq!(
            first_class_generation_wire_path(&responses).as_deref(),
            Some("text.verbosity")
        );
    }

    #[test]
    fn token_bias_requires_the_final_routes_exact_vocabulary() {
        let mut options = DictMap::new();
        options.put(
            "logit_bias",
            VmValue::List(std::sync::Arc::new(vec![bias(
                token_ref(15339, "tiktoken:cl100k_base"),
                -4.0,
            )])),
        );
        let error = parse_logit_bias(Some(&options), "openai", "gpt-4o")
            .expect_err("gpt-4o uses o200k_base");
        let message = message(error);
        assert!(message.contains("tiktoken:cl100k_base"));
        assert!(message.contains("tiktoken:o200k_base"));
    }

    #[test]
    fn token_bias_rejects_duplicate_ids_and_out_of_range_biases() {
        let token = token_ref(24912, "tiktoken:o200k_base");
        let mut duplicate = DictMap::new();
        duplicate.put(
            "logit_bias",
            VmValue::List(std::sync::Arc::new(vec![
                bias(token.clone(), -1.0),
                bias(token, 1.0),
            ])),
        );
        assert!(message(
            parse_logit_bias(Some(&duplicate), "openai", "gpt-4o")
                .expect_err("duplicate token IDs are ambiguous")
        )
        .contains("more than once"));

        let mut out_of_range = DictMap::new();
        out_of_range.put(
            "logit_bias",
            VmValue::List(std::sync::Arc::new(vec![bias(
                token_ref(24912, "tiktoken:o200k_base"),
                100.1,
            )])),
        );
        assert!(message(
            parse_logit_bias(Some(&out_of_range), "openai", "gpt-4o")
                .expect_err("bias range is provider bounded")
        )
        .contains("-100..=100"));
    }

    #[test]
    fn logprobs_config_bounds_top_alternatives() {
        let mut config = DictMap::new();
        config.put("top", VmValue::Int(21));
        let mut options = DictMap::new();
        options.put("logprobs", VmValue::dict(config));
        assert!(
            message(parse_logprobs(Some(&options)).expect_err("top is bounded")).contains("0..=20")
        );
    }

    #[test]
    fn fallback_route_revalidates_the_tokenizer_vocabulary() {
        let mut options = crate::llm::api::LlmCallOptions {
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            ..Default::default()
        };
        options.logit_bias.push(crate::llm::api::TokenBias {
            token_id: 24912,
            tokenizer: "tiktoken:o200k_base".to_string(),
            bias: -1.0,
        });
        options
            .portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::LogitBias);

        let error = validate_options(&options)
            .expect_err("gpt-4 uses cl100k_base after fallback selection");
        let rendered = format!("{error:?}");
        assert!(rendered.contains("tiktoken:o200k_base"));
        assert!(rendered.contains("tiktoken:cl100k_base"));
    }

    #[test]
    fn gemini_interactions_logprobs_select_unary_transport_unless_stream_is_explicit() {
        assert!(
            !normalize_generation_stream(true, false, "gemini", "gemini-3.5-flash-lite", true,)
                .expect("the default stream may normalize")
        );
        assert!(
            normalize_generation_stream(true, true, "gemini", "gemini-3.5-flash-lite", true,)
                .is_err()
        );
    }

    #[test]
    fn cerebras_rejects_logprobs_with_prediction_before_transport() {
        let mut options = crate::llm::api::LlmCallOptions {
            provider: "cerebras".to_string(),
            model: "gpt-oss-120b".to_string(),
            logprobs: Some(crate::llm::api::LogprobsConfig { top: Some(2) }),
            prediction: Some("OK".to_string()),
            ..Default::default()
        };
        options
            .portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::Logprobs);
        options
            .portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::Prediction);
        let error = validate_options(&options).expect_err("combination is provider-invalid");
        assert!(format!("{error:?}").contains("choose one"));
    }

    #[test]
    fn anthropic_rejects_forced_tool_choice_with_manual_thinking() {
        let options = crate::llm::api::LlmCallOptions {
            provider: "anthropic".to_string(),
            model: "claude-haiku-4-5-20251001".to_string(),
            thinking: crate::llm::api::ThinkingConfig::Enabled {
                budget_tokens: Some(1024),
            },
            tool_choice: Some(serde_json::json!("required")),
            ..Default::default()
        };

        let error = validate_options(&options).expect_err("combination is provider-invalid");
        assert!(format!("{error:?}").contains("manual thinking"));

        let mut automatic = options.clone();
        automatic.tool_choice = Some(serde_json::json!("auto"));
        validate_options(&automatic).expect("automatic tool choice remains valid");

        let mut adaptive = options;
        adaptive.thinking = crate::llm::api::ThinkingConfig::Adaptive;
        validate_options(&adaptive).expect("adaptive thinking permits forced tool choice");
    }
}
