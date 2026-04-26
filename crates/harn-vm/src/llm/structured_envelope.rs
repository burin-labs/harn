//! Result-envelope variant of `llm_call_structured`. Where
//! `llm_call_structured` throws on schema-retry exhaustion, this surface
//! always returns a `{ok, data, raw_text, error, error_category,
//! attempts, repaired, extracted_json, usage, model, provider}` dict so
//! production agent pipelines can preserve diagnostics, attempt counts,
//! and raw model text without hand-rolling parse/repair chains.
//!
//! Implemented as a thin wrapper over `execute_schema_retry_loop`, with
//! an optional repair pass that reissues a separate LLM call on
//! malformed JSON. Repair config:
//!
//! ```harn
//! let result = llm_call_structured_result(prompt, schema, {
//!   provider: "auto",
//!   schema_retries: 2,
//!   repair: {
//!     enabled: true,
//!     model: "cheapest_over_quality(low)",
//!     max_tokens: 600,
//!   },
//! })
//! ```
//!
//! The repair pass is only attempted on JSON-shaped failures
//! (`missing_json` / `schema_validation`); transport failures
//! (`auth`, `rate_limit`, `transient_network`, ...) skip repair
//! since there is no raw text to salvage.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::helpers::extract_llm_options;
use super::{execute_schema_retry_loop, rewrite_structured_args, SchemaLoopOutcome};

/// Build the `{ok, data, raw_text, error, error_category, attempts,
/// repaired, extracted_json, usage, model, provider}` envelope. Never
/// throws on transport / schema failures — the caller dispatches on
/// `ok` / `error_category`.
pub(crate) async fn run_structured_envelope(
    args: Vec<VmValue>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
) -> Result<VmValue, VmError> {
    let mut rewritten = match rewrite_structured_args(args) {
        Ok(v) => v,
        // Argument-shape errors surface as a `transport`-categorized
        // envelope so callers can branch on `ok` without try/catch.
        Err(err) => return Ok(envelope_from_arg_error(&err)),
    };
    // Pull the `repair` block out of the options dict before
    // `extract_llm_options` runs — repair is a result-envelope
    // configuration knob, not a pass-through provider option.
    let repair_config = take_repair_config(&mut rewritten);
    let options_dict = rewritten.get(2).and_then(|a| a.as_dict()).cloned();
    let opts = match extract_llm_options(&rewritten) {
        Ok(opts) => opts,
        Err(err) => return Ok(envelope_from_arg_error(&err)),
    };
    let provider_hint = opts.provider.clone();
    let model_hint = opts.model.clone();

    let main_outcome = match execute_schema_retry_loop(opts, options_dict.clone(), bridge).await {
        Ok(outcome) => outcome,
        Err(err) => {
            return Ok(envelope_from_transport_error(
                &err,
                &provider_hint,
                &model_hint,
            ));
        }
    };

    if main_outcome.errors.is_empty() {
        return Ok(envelope_success(&main_outcome, false));
    }

    // Schema/JSON failure — try repair if configured.
    if let Some(repair) = repair_config {
        if repair.enabled {
            if let Some(env) =
                run_repair_pass(&main_outcome, &repair, options_dict.as_ref(), bridge).await
            {
                return Ok(env);
            }
            // Repair didn't recover — fall through to the main-call
            // failure envelope, but mark the category as repair_failed
            // so callers can distinguish "tried repair, didn't help"
            // from "repair was disabled".
            return Ok(envelope_failure(
                &main_outcome,
                EnvelopeFailureKind::RepairFailed,
                false,
            ));
        }
    }

    Ok(envelope_failure(
        &main_outcome,
        classify_main_failure(&main_outcome),
        false,
    ))
}

fn classify_main_failure(outcome: &SchemaLoopOutcome) -> EnvelopeFailureKind {
    let has_data = outcome
        .vm_result
        .as_dict()
        .is_some_and(|d| d.contains_key("data"));
    if has_data {
        EnvelopeFailureKind::SchemaValidation
    } else {
        EnvelopeFailureKind::MissingJson
    }
}

/// Returned-`{enabled, ...overrides}`-dict-or-`nil` repair config.
struct RepairConfig {
    enabled: bool,
    overrides: BTreeMap<String, VmValue>,
}

fn take_repair_config(args: &mut [VmValue]) -> Option<RepairConfig> {
    let options = args.get_mut(2)?;
    let mut new_dict = options.as_dict()?.clone();
    let raw = new_dict.remove("repair")?;
    *options = VmValue::Dict(Rc::new(new_dict));
    parse_repair_value(&raw)
}

fn parse_repair_value(raw: &VmValue) -> Option<RepairConfig> {
    match raw {
        VmValue::Nil => None,
        VmValue::Bool(b) => Some(RepairConfig {
            enabled: *b,
            overrides: BTreeMap::new(),
        }),
        VmValue::Dict(d) => {
            let enabled = match d.get("enabled") {
                None => true, // Presence of the dict implies opt-in.
                Some(VmValue::Bool(false)) => false,
                Some(VmValue::Nil) => true,
                Some(VmValue::Bool(true)) => true,
                Some(_) => true, // Tolerant: any truthy value enables it.
            };
            let mut overrides: BTreeMap<String, VmValue> = (**d).clone();
            overrides.remove("enabled");
            Some(RepairConfig { enabled, overrides })
        }
        _ => None,
    }
}

/// Run the repair pass: build a corrective prompt, call the LLM with
/// repair-config overrides applied, validate, return a success envelope
/// on success or `None` on failure (caller falls back to repair_failed).
async fn run_repair_pass(
    main_outcome: &SchemaLoopOutcome,
    repair: &RepairConfig,
    base_options: Option<&BTreeMap<String, VmValue>>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
) -> Option<VmValue> {
    let prompt = build_repair_prompt(&main_outcome.raw_text, &main_outcome.errors);
    let merged_options = merge_repair_options(base_options, &repair.overrides);
    let merged_dict = Some(merged_options.clone());
    // Repair runs as a single-shot structured call with no further
    // schema retries — the budget already burned on the main call. The
    // `extract_llm_options` path reads the same dict we hand to
    // `execute_schema_retry_loop`, so the repair pass picks up the
    // caller's `output_schema` (already lifted from the `schema`
    // positional arg by `rewrite_structured_args`).
    let args = vec![
        VmValue::String(Rc::from(prompt.as_str())),
        // System slot — the prompt carries instructions inline.
        VmValue::Nil,
        VmValue::Dict(Rc::new(merged_options)),
    ];
    let opts = extract_llm_options(&args).ok()?;
    let outcome = execute_schema_retry_loop(opts, merged_dict, bridge)
        .await
        .ok()?;
    if outcome.errors.is_empty() {
        Some(envelope_success(&outcome, true))
    } else {
        None
    }
}

fn build_repair_prompt(raw_text: &str, errors: &[String]) -> String {
    let errors_line = if errors.is_empty() {
        String::from("(no detailed errors)")
    } else {
        errors.join("; ")
    };
    let mut s = String::from(
        "The following text was supposed to be JSON conforming to the configured schema, but it failed validation. \
Repair it and respond with ONLY the corrected JSON — no prose, no markdown fences, no commentary.\n\n",
    );
    s.push_str("Validation errors: ");
    s.push_str(&errors_line);
    s.push_str("\n\nOriginal text:\n");
    s.push_str(raw_text);
    s.push_str("\n\nReply with valid JSON only.");
    s
}

fn merge_repair_options(
    base: Option<&BTreeMap<String, VmValue>>,
    overrides: &BTreeMap<String, VmValue>,
) -> BTreeMap<String, VmValue> {
    let mut merged = base.cloned().unwrap_or_default();
    // The repair pass runs a single shot: do not multiply schema
    // retries from the main call (cost amplification) and do not let
    // the main call's transient retry budget propagate either —
    // repair is best-effort and should fail fast.
    merged.insert("schema_retries".to_string(), VmValue::Int(0));
    // Drop any nested `repair` key from the base options so a repair
    // call cannot recursively trigger another repair pass.
    merged.remove("repair");
    for (k, v) in overrides {
        merged.insert(k.clone(), v.clone());
    }
    merged
}

#[derive(Clone, Copy)]
enum EnvelopeFailureKind {
    /// Model returned text but no parseable JSON could be extracted.
    MissingJson,
    /// JSON parsed but failed schema validation.
    SchemaValidation,
    /// Repair pass was attempted and also failed.
    RepairFailed,
}

impl EnvelopeFailureKind {
    fn category(self) -> &'static str {
        match self {
            EnvelopeFailureKind::MissingJson => "missing_json",
            EnvelopeFailureKind::SchemaValidation => "schema_validation",
            EnvelopeFailureKind::RepairFailed => "repair_failed",
        }
    }
}

fn envelope_success(outcome: &SchemaLoopOutcome, repaired: bool) -> VmValue {
    let data = match outcome.vm_result.as_dict() {
        Some(d) => d.get("data").cloned().unwrap_or(VmValue::Nil),
        None => VmValue::Nil,
    };
    let extracted_json = detect_extracted_json(outcome);
    let usage = build_usage_dict(outcome);
    let (model, provider) = result_model_provider(outcome);

    let mut env = BTreeMap::new();
    env.insert("ok".to_string(), VmValue::Bool(true));
    env.insert("data".to_string(), data);
    env.insert(
        "raw_text".to_string(),
        VmValue::String(Rc::from(outcome.raw_text.as_str())),
    );
    env.insert("error".to_string(), VmValue::String(Rc::from("")));
    env.insert("error_category".to_string(), VmValue::Nil);
    env.insert(
        "attempts".to_string(),
        VmValue::Int(outcome.attempts as i64),
    );
    env.insert("repaired".to_string(), VmValue::Bool(repaired));
    env.insert("extracted_json".to_string(), VmValue::Bool(extracted_json));
    env.insert("usage".to_string(), usage);
    env.insert(
        "model".to_string(),
        VmValue::String(Rc::from(model.as_str())),
    );
    env.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(provider.as_str())),
    );
    VmValue::Dict(Rc::new(env))
}

fn envelope_failure(
    outcome: &SchemaLoopOutcome,
    kind: EnvelopeFailureKind,
    repaired: bool,
) -> VmValue {
    let extracted_json = detect_extracted_json(outcome);
    let usage = build_usage_dict(outcome);
    let (model, provider) = result_model_provider(outcome);
    let message = if outcome.errors.is_empty() {
        "structured call failed without specific errors".to_string()
    } else {
        outcome.errors.join("; ")
    };

    let mut env = BTreeMap::new();
    env.insert("ok".to_string(), VmValue::Bool(false));
    env.insert("data".to_string(), VmValue::Nil);
    env.insert(
        "raw_text".to_string(),
        VmValue::String(Rc::from(outcome.raw_text.as_str())),
    );
    env.insert(
        "error".to_string(),
        VmValue::String(Rc::from(message.as_str())),
    );
    env.insert(
        "error_category".to_string(),
        VmValue::String(Rc::from(kind.category())),
    );
    env.insert(
        "attempts".to_string(),
        VmValue::Int(outcome.attempts as i64),
    );
    env.insert("repaired".to_string(), VmValue::Bool(repaired));
    env.insert("extracted_json".to_string(), VmValue::Bool(extracted_json));
    env.insert("usage".to_string(), usage);
    env.insert(
        "model".to_string(),
        VmValue::String(Rc::from(model.as_str())),
    );
    env.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(provider.as_str())),
    );
    VmValue::Dict(Rc::new(env))
}

fn envelope_from_transport_error(err: &VmError, provider: &str, model: &str) -> VmValue {
    let category = crate::value::error_to_category(err);
    let message = match err {
        VmError::CategorizedError { message, .. } => message.clone(),
        VmError::Thrown(VmValue::String(s)) => s.to_string(),
        VmError::Thrown(VmValue::Dict(d)) => d
            .get("message")
            .map(|v| v.display())
            .unwrap_or_else(|| err.to_string()),
        _ => err.to_string(),
    };
    let mut env = BTreeMap::new();
    env.insert("ok".to_string(), VmValue::Bool(false));
    env.insert("data".to_string(), VmValue::Nil);
    env.insert("raw_text".to_string(), VmValue::String(Rc::from("")));
    env.insert(
        "error".to_string(),
        VmValue::String(Rc::from(message.as_str())),
    );
    env.insert(
        "error_category".to_string(),
        VmValue::String(Rc::from(category.as_str())),
    );
    env.insert("attempts".to_string(), VmValue::Int(0));
    env.insert("repaired".to_string(), VmValue::Bool(false));
    env.insert("extracted_json".to_string(), VmValue::Bool(false));
    env.insert(
        "usage".to_string(),
        VmValue::Dict(Rc::new(empty_usage_dict())),
    );
    env.insert("model".to_string(), VmValue::String(Rc::from(model)));
    env.insert("provider".to_string(), VmValue::String(Rc::from(provider)));
    VmValue::Dict(Rc::new(env))
}

fn envelope_from_arg_error(err: &VmError) -> VmValue {
    // Argument-shape failures don't have provider/model context yet —
    // surface them as `generic`-categorized via the standard transport
    // path so callers can branch on `ok` without try/catch.
    envelope_from_transport_error(err, "", "")
}

fn empty_usage_dict() -> BTreeMap<String, VmValue> {
    let mut usage = BTreeMap::new();
    usage.insert("input_tokens".to_string(), VmValue::Int(0));
    usage.insert("output_tokens".to_string(), VmValue::Int(0));
    usage.insert("cache_read_tokens".to_string(), VmValue::Int(0));
    usage.insert("cache_write_tokens".to_string(), VmValue::Int(0));
    usage
}

fn build_usage_dict(outcome: &SchemaLoopOutcome) -> VmValue {
    let dict = match outcome.vm_result.as_dict() {
        Some(d) => d,
        None => return VmValue::Dict(Rc::new(empty_usage_dict())),
    };
    let mut usage = empty_usage_dict();
    for key in [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
    ] {
        if let Some(v) = dict.get(key) {
            usage.insert(key.to_string(), v.clone());
        }
    }
    VmValue::Dict(Rc::new(usage))
}

fn result_model_provider(outcome: &SchemaLoopOutcome) -> (String, String) {
    let dict = match outcome.vm_result.as_dict() {
        Some(d) => d,
        None => return (String::new(), String::new()),
    };
    let model = dict.get("model").map(VmValue::display).unwrap_or_default();
    let provider = dict
        .get("provider")
        .map(VmValue::display)
        .unwrap_or_default();
    (model, provider)
}

/// Heuristic: if the trimmed raw text doesn't directly parse as JSON
/// but the extracted candidate does, JSON was lifted out of prose or
/// fences. The non-bridge schema-retry loop is what populates
/// `vm_result.data`, so this only flags `true` on the path where data
/// came back successfully.
fn detect_extracted_json(outcome: &SchemaLoopOutcome) -> bool {
    let dict = match outcome.vm_result.as_dict() {
        Some(d) => d,
        None => return false,
    };
    if !dict.contains_key("data") {
        return false;
    }
    let trimmed = outcome.raw_text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return false;
    }
    let extracted = crate::stdlib::json::extract_json_from_text(&outcome.raw_text);
    extracted != trimmed && serde_json::from_str::<serde_json::Value>(&extracted).is_ok()
}

/// Used by [`crate::llm::register_llm_builtins`] for the non-bridge
/// path, and by [`crate::llm::agent_config::register_llm_call_structured_with_bridge`]
/// for the bridge path. Single entry point keeps both registrations
/// behavior-identical.
pub(crate) async fn llm_call_structured_result_impl(
    args: Vec<VmValue>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
) -> Result<VmValue, VmError> {
    run_structured_envelope(args, bridge).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_prompt_includes_raw_text_and_errors() {
        let prompt = build_repair_prompt(
            "{\"verdict\": 42}",
            &["expected string for verdict".to_string()],
        );
        assert!(prompt.contains("Validation errors: expected string for verdict"));
        assert!(prompt.contains("{\"verdict\": 42}"));
        assert!(prompt.contains("Reply with valid JSON only"));
    }

    #[test]
    fn merge_repair_caps_schema_retries_and_drops_nested_repair() {
        let mut base = BTreeMap::new();
        base.insert("provider".to_string(), VmValue::String(Rc::from("auto")));
        base.insert("schema_retries".to_string(), VmValue::Int(5));
        base.insert(
            "repair".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::new())),
        );
        let overrides = {
            let mut o = BTreeMap::new();
            o.insert("model".to_string(), VmValue::String(Rc::from("local:fix")));
            o
        };
        let merged = merge_repair_options(Some(&base), &overrides);
        assert_eq!(
            merged.get("schema_retries").and_then(VmValue::as_int),
            Some(0)
        );
        assert_eq!(
            merged.get("model").map(VmValue::display).as_deref(),
            Some("local:fix")
        );
        assert_eq!(
            merged.get("provider").map(VmValue::display).as_deref(),
            Some("auto")
        );
        assert!(!merged.contains_key("repair"));
    }

    #[test]
    fn parse_repair_value_handles_each_shape() {
        assert!(parse_repair_value(&VmValue::Nil).is_none());
        let bool_true = parse_repair_value(&VmValue::Bool(true)).unwrap();
        assert!(bool_true.enabled);
        let bool_false = parse_repair_value(&VmValue::Bool(false)).unwrap();
        assert!(!bool_false.enabled);
        let dict_no_enabled = parse_repair_value(&VmValue::Dict(Rc::new(BTreeMap::new()))).unwrap();
        assert!(dict_no_enabled.enabled);
        let mut disabled = BTreeMap::new();
        disabled.insert("enabled".to_string(), VmValue::Bool(false));
        let dict_disabled = parse_repair_value(&VmValue::Dict(Rc::new(disabled))).unwrap();
        assert!(!dict_disabled.enabled);
    }
}
