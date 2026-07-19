use serde_json::Value as JsonValue;

use crate::cli::EvalCodingAgentArgs;
use crate::commands::eval_model_selector::ModelSelector;

/// Translate the `--step-judge <preset>` CLI flag into a JSON object the
/// inner `coding_agent_suite.harn` script feeds to `agent_loop({step_judge: ...})`.
/// Returns `None` for `None` / `"none"` / `"off"` / empty.
///
/// Preset semantics (designed for the step-judge experiment in
/// experiments/step-judge/):
/// - `symmetric-cheap`: judge = generator model (cheap-judges-cheap)
/// - `asymmetric`: judge = `anthropic/claude-sonnet-4-6` via OpenRouter
/// - `symmetric-strong`: judge = generator model (caller expected to
///   pass --model anthropic/claude-sonnet-4-6 to make this meaningful)
/// - `custom:<json>`: literal JSON dict passed through verbatim
pub(super) fn resolve_step_judge_json(
    args: &EvalCodingAgentArgs,
    selector: &ModelSelector,
) -> Option<String> {
    let raw = args.step_judge.as_deref()?.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("none") || raw.eq_ignore_ascii_case("off") {
        return None;
    }
    let mut obj = serde_json::Map::new();
    if let Some(rest) = raw.strip_prefix("custom:") {
        match serde_json::from_str::<JsonValue>(rest) {
            Ok(JsonValue::Object(map)) => obj.extend(map),
            _ => {
                obj.insert(
                    "model".to_string(),
                    JsonValue::String("__invalid_custom_step_judge__".to_string()),
                );
            }
        }
    } else {
        match raw {
            "symmetric-cheap" | "symmetric-strong" => {
                obj.insert(
                    "model".to_string(),
                    JsonValue::String(selector.model.clone()),
                );
                obj.insert(
                    "provider".to_string(),
                    JsonValue::String(selector.provider.clone()),
                );
            }
            "asymmetric" => {
                obj.insert(
                    "model".to_string(),
                    JsonValue::String("anthropic/claude-sonnet-4-6".to_string()),
                );
                obj.insert(
                    "provider".to_string(),
                    JsonValue::String("openrouter".to_string()),
                );
            }
            _other => {
                obj.insert(
                    "model".to_string(),
                    JsonValue::String("__unknown_step_judge_preset__".to_string()),
                );
            }
        }
    }
    if let Some(on_veto) = args.step_judge_on_veto.as_deref() {
        obj.insert(
            "on_veto".to_string(),
            JsonValue::String(on_veto.to_string()),
        );
    }
    if args.step_judge_adversarial {
        obj.insert(
            "rubric".to_string(),
            JsonValue::String("adversarial".to_string()),
        );
    }
    Some(JsonValue::Object(obj).to_string())
}

pub(super) fn resolve_structural_validator_json(args: &EvalCodingAgentArgs) -> Option<String> {
    let raw = args.structural_validator.as_deref()?.trim();
    if raw.is_empty() {
        return None;
    }
    match raw {
        "on" | "default" => Some(
            serde_json::json!({
                "rules": [
                    "non_empty_when_writes_expected",
                    "no_phantom_completion",
                    "tool_calls_well_formed",
                    "output_token_cap_with_zero_calls"
                ]
            })
            .to_string(),
        ),
        "off" | "none" => Some(JsonValue::Bool(false).to_string()),
        _ => {
            if let Some(rest) = raw.strip_prefix("custom:") {
                return match serde_json::from_str::<JsonValue>(rest) {
                    Ok(JsonValue::Object(map)) => Some(JsonValue::Object(map).to_string()),
                    _ => Some(
                        serde_json::json!({
                            "on_failure": "__invalid_custom_structural_validator__"
                        })
                        .to_string(),
                    ),
                };
            }
            Some(
                serde_json::json!({
                    "on_failure": "__unknown_structural_validator_preset__"
                })
                .to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_step_judge_json, resolve_structural_validator_json};
    use crate::cli::EvalCodingAgentArgs;
    use crate::commands::eval_model_selector::ModelSelector;
    use serde_json::Value as JsonValue;

    fn default_args() -> EvalCodingAgentArgs {
        EvalCodingAgentArgs {
            fixtures: vec!["all".to_string()],
            models: vec!["mock:mock".to_string()],
            tool_formats: vec!["native".to_string()],
            output: None,
            env_files: Vec::new(),
            include_local: false,
            local_providers: Vec::new(),
            max_local_models: 2,
            keep_local_after_run: false,
            max_runs: None,
            replicates: 1,
            max_iterations: 8,
            python: "python3".to_string(),
            fail_on_unauthorized: false,
            json: false,
            step_judge: None,
            step_judge_on_veto: None,
            step_judge_adversarial: false,
            override_reason: None,
            structural_validator: None,
            run_label: String::new(),
            baseline_comparison_against: None,
        }
    }

    #[test]
    fn structural_validator_presets_translate_to_json() {
        let mut args = default_args();
        args.structural_validator = Some("on".to_string());

        let on = resolve_structural_validator_json(&args).expect("on preset");
        let on_value: JsonValue = serde_json::from_str(&on).expect("on preset parses");
        assert_eq!(
            on_value["rules"],
            serde_json::json!([
                "non_empty_when_writes_expected",
                "no_phantom_completion",
                "tool_calls_well_formed",
                "output_token_cap_with_zero_calls"
            ])
        );

        args.structural_validator = Some("off".to_string());
        assert_eq!(
            resolve_structural_validator_json(&args),
            Some("false".to_string())
        );

        args.structural_validator =
            Some("custom:{\"rules\":[\"tool_calls_well_formed\"]}".to_string());
        let custom = resolve_structural_validator_json(&args).expect("custom preset");
        let custom_value: JsonValue = serde_json::from_str(&custom).expect("custom preset parses");
        assert_eq!(
            custom_value["rules"],
            serde_json::json!(["tool_calls_well_formed"])
        );
    }

    #[test]
    fn step_judge_off_alias_disables_judge() {
        let mut args = default_args();
        args.step_judge = Some("off".to_string());
        let selector = ModelSelector {
            selector: "mock:mock".to_string(),
            provider: "mock".to_string(),
            model: "mock".to_string(),
        };

        assert_eq!(resolve_step_judge_json(&args, &selector), None);
    }
}
