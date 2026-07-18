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

use crate::value::VmDictExt;
use std::sync::Arc;

use crate::value::{VmError, VmValue};

use super::helpers::{extract_llm_options, vm_value_to_json};
use super::{execute_schema_retry_loop, rewrite_structured_args, SchemaLoopOutcome};

/// Build the `{ok, data, raw_text, error, error_category, attempts,
/// repaired, extracted_json, usage, model, provider}` envelope. Never
/// throws on transport / schema failures — the caller dispatches on
/// `ok` / `error_category`.
pub(crate) async fn run_structured_envelope(
    args: Vec<VmValue>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
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
    let mut options_dict = rewritten.get(2).and_then(|a| a.as_dict()).cloned();
    let opts = match extract_llm_options(&rewritten) {
        Ok(opts) => opts,
        Err(err) if is_unsupported_structured_transport_error(&err) => {
            apply_prompt_mode_structured_transport(&mut rewritten);
            options_dict = rewritten.get(2).and_then(|a| a.as_dict()).cloned();
            match extract_llm_options(&rewritten) {
                Ok(opts) => opts,
                Err(fallback_err) => return Ok(envelope_from_arg_error(&fallback_err)),
            }
        }
        Err(err) => return Ok(envelope_from_arg_error(&err)),
    };
    let provider_hint = opts.provider.clone();
    let model_hint = opts.model.clone();

    let main_outcome = match execute_schema_retry_loop(
        None,
        opts,
        options_dict.clone(),
        bridge,
        None,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(err) => {
            // Outcome-based structured-output fallback. A multi-upstream
            // router (e.g. OpenRouter) can return a route-specific
            // `400 invalid_request` for a json_schema/json_object
            // `response_format` when it transiently routes to an upstream
            // that can't honor it — even for a model whose capability rule
            // declares native structured output. That is NOT a permanent
            // client error, but Harn classifies 400/invalid_request as a
            // Terminal (non-retryable) error, so the native mechanism alone
            // would surface a hard failure for a quirk Harn is meant to
            // abstract away. Degrade to the existing prompt-mode TEXT
            // transport (schema embedded in the prompt + post-hoc Harn
            // validation, which carries no `response_format` and so works on
            // any chat model/route) and retry once. The native path — and
            // the meter — are unchanged whenever the first call succeeds.
            if is_structured_output_rejection(&err)
                && structured_request_uses_response_format(&rewritten)
            {
                tracing::warn!(
                    provider = %provider_hint,
                    model = %model_hint,
                    "structured output got invalid_request; degrading to prompt-mode text transport and retrying"
                );
                apply_prompt_mode_structured_transport(&mut rewritten);
                let fallback_options = rewritten.get(2).and_then(|a| a.as_dict()).cloned();
                match extract_llm_options(&rewritten) {
                    Ok(fallback_opts) => {
                        match execute_schema_retry_loop(
                            None,
                            fallback_opts,
                            fallback_options,
                            bridge,
                            None,
                        )
                        .await
                        {
                            Ok(outcome) => outcome,
                            Err(fallback_err) => {
                                return Ok(envelope_from_transport_error(
                                    &fallback_err,
                                    &provider_hint,
                                    &model_hint,
                                ));
                            }
                        }
                    }
                    Err(fallback_err) => return Ok(envelope_from_arg_error(&fallback_err)),
                }
            } else {
                return Ok(envelope_from_transport_error(
                    &err,
                    &provider_hint,
                    &model_hint,
                ));
            }
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
            // from "repair was disabled". A token-limit truncation keeps its
            // own integrity category even after a failed repair: the root
            // cause is an under-budgeted call, and masking it as repair_failed
            // would hide a dead-judge truncation from the meter.
            let kind = if outcome_hit_token_limit(&main_outcome) {
                EnvelopeFailureKind::LengthTruncation
            } else {
                EnvelopeFailureKind::RepairFailed
            };
            return Ok(envelope_failure(&main_outcome, kind, false));
        }
    }

    Ok(envelope_failure(
        &main_outcome,
        classify_main_failure(&main_outcome),
        false,
    ))
}

/// Whether a transport error looks like the provider/route REJECTING the
/// structured-output request itself (a json_schema/json_object `response_format`
/// an upstream can't honor), as opposed to an unrelated failure. Detected from
/// the error message because `error_to_category` collapses a provider 400 to the
/// generic bucket — the discriminating signal (`invalid_request`,
/// `400 bad request`, `response_format`, `json_schema`) only survives in the
/// message text relayed from the upstream/router.
fn is_structured_output_rejection(err: &VmError) -> bool {
    let message = err.to_string().to_lowercase();
    message.contains("invalid_request")
        || message.contains("response_format")
        || message.contains("json_schema")
        || (message.contains("400") && message.contains("bad request"))
}

/// Whether this structured request actually sent a `response_format`
/// (json_schema / json_object) to the provider, i.e. it is NOT already in
/// prompt-mode text transport. Only such requests can hit a provider-side
/// structured-output 400 and benefit from degrading to text transport. The
/// structured envelope defaults to json_schema when a schema arg is present, so
/// a missing options dict / missing output_format still counts as "yes".
fn structured_request_uses_response_format(args: &[VmValue]) -> bool {
    let Some(dict) = args.get(2).and_then(|a| a.as_dict()) else {
        return true;
    };
    let is_text = |value: &VmValue| match value {
        VmValue::String(text) => text.to_string() == "text",
        _ => false,
    };
    if dict.get("output_format").is_some_and(is_text)
        || dict.get("response_format").is_some_and(is_text)
    {
        return false;
    }
    true
}

fn is_unsupported_structured_transport_error(err: &VmError) -> bool {
    let message = err.to_string();
    message.contains("option `output_format` is not supported")
        || message.contains("unsupported structured_output strategy")
}

fn apply_prompt_mode_structured_transport(args: &mut [VmValue]) {
    let schema = args.get(1).cloned().unwrap_or(VmValue::Nil);
    if let Some(prompt) = args.get_mut(0).and_then(|value| match value {
        VmValue::String(text) => Some(text.to_string()),
        _ => None,
    }) {
        args[0] = VmValue::String(arcstr::ArcStr::from(prompt_with_schema_contract(
            &prompt, &schema,
        )));
    }
    if let Some(options) = args.get_mut(2) {
        let mut dict = options.as_dict().cloned().unwrap_or_default();
        dict.put_str("output_format", "text");
        dict.put_str("response_format", "text");
        *options = VmValue::dict(dict);
    }
}

fn prompt_with_schema_contract(prompt: &str, schema: &VmValue) -> String {
    let schema_json = serde_json::to_string_pretty(&vm_value_to_json(schema))
        .unwrap_or_else(|_| schema.display());
    let mut bindings = crate::value::DictMap::new();
    bindings.put_str("prompt", prompt);
    bindings.put_str("schema_json", schema_json);
    crate::stdlib::template::render_stdlib_prompt_asset(
        "llm/prompts/structured_envelope_schema_contract.harn.prompt",
        Some(&bindings),
    )
    .expect("structured_envelope_schema_contract.harn.prompt is embedded and must render")
}

fn classify_main_failure(outcome: &SchemaLoopOutcome) -> EnvelopeFailureKind {
    // A token-limit truncation is a MEASUREMENT-INTEGRITY signal, not an
    // ordinary "the model returned prose instead of JSON" miss: the request
    // ran out of `max_tokens` mid-object, so the JSON is unparseable purely
    // because it was cut off. `structured_output_errors` (the schema-retry
    // loop) already appends the canonical truncation marker derived from a
    // provider-agnostic `is_length_truncation(stop_reason)` check, so we key
    // off that marker here rather than re-inspecting the raw stop_reason.
    // Surfacing it as its own category lets judge/router callers distinguish a
    // DEAD (truncated) judge — which silently falls through to a deterministic
    // grader — from a model that genuinely could not produce a verdict.
    if outcome_hit_token_limit(outcome) {
        return EnvelopeFailureKind::LengthTruncation;
    }
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

/// Whether the structured failure was caused by the response running out of
/// output-token budget mid-JSON. Detected from the canonical truncation marker
/// `structured_output_errors` appends (see `call.rs::structured_output_errors`),
/// which is itself derived from the provider-agnostic
/// `is_length_truncation(stop_reason)` classifier — so this stays correct across
/// every provider's stop_reason spelling without a per-provider branch here.
fn outcome_hit_token_limit(outcome: &SchemaLoopOutcome) -> bool {
    outcome
        .errors
        .iter()
        .any(|e| e.contains("hit the token limit"))
}

/// Returned-`{enabled, ...overrides}`-dict-or-`nil` repair config.
struct RepairConfig {
    enabled: bool,
    overrides: crate::value::DictMap,
}

fn take_repair_config(args: &mut [VmValue]) -> Option<RepairConfig> {
    let options = args.get_mut(2)?;
    let mut new_dict = options.as_dict()?.clone();
    let raw = new_dict.remove("repair")?;
    *options = VmValue::dict(new_dict);
    parse_repair_value(&raw)
}

fn parse_repair_value(raw: &VmValue) -> Option<RepairConfig> {
    match raw {
        VmValue::Nil => None,
        VmValue::Bool(b) => Some(RepairConfig {
            enabled: *b,
            overrides: crate::value::DictMap::new(),
        }),
        VmValue::Dict(d) => {
            let enabled = match d.get("enabled") {
                None => true, // Presence of the dict implies opt-in.
                Some(VmValue::Bool(false)) => false,
                Some(VmValue::Nil) => true,
                Some(VmValue::Bool(true)) => true,
                Some(_) => true, // Tolerant: any truthy value enables it.
            };
            let mut overrides: crate::value::DictMap = (**d).clone();
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
    base_options: Option<&crate::value::DictMap>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
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
        VmValue::String(arcstr::ArcStr::from(prompt.as_str())),
        // System slot — the prompt carries instructions inline.
        VmValue::Nil,
        VmValue::dict(merged_options),
    ];
    let opts = extract_llm_options(&args).ok()?;
    let outcome = execute_schema_retry_loop(None, opts, merged_dict, bridge, None)
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
    let mut bindings = crate::value::DictMap::new();
    bindings.put_str("errors_line", errors_line);
    bindings.put_str("raw_text", raw_text);
    crate::stdlib::template::render_stdlib_prompt_asset(
        "llm/prompts/structured_envelope_repair.harn.prompt",
        Some(&bindings),
    )
    .expect("structured_envelope_repair.harn.prompt is embedded and must render")
}

fn merge_repair_options(
    base: Option<&crate::value::DictMap>,
    overrides: &crate::value::DictMap,
) -> crate::value::DictMap {
    let mut merged = base.cloned().unwrap_or_default();
    // The repair pass runs a single shot: do not multiply schema
    // retries from the main call (cost amplification) and do not let
    // the main call's transient retry budget propagate either —
    // repair is best-effort and should fail fast.
    merged.insert(crate::value::intern_key("schema_retries"), VmValue::Int(0));
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
    /// The response was truncated by the `max_tokens` budget before it could
    /// emit complete JSON — a measurement-integrity signal distinct from a
    /// model that returned no JSON at all. Callers (judges/routers) use this to
    /// detect a DEAD structured call instead of treating the truncation as an
    /// ordinary abstention.
    LengthTruncation,
}

impl EnvelopeFailureKind {
    fn category(self) -> &'static str {
        match self {
            EnvelopeFailureKind::MissingJson => "missing_json",
            EnvelopeFailureKind::SchemaValidation => "schema_validation",
            EnvelopeFailureKind::RepairFailed => "repair_failed",
            EnvelopeFailureKind::LengthTruncation => "length_truncation",
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

    let mut env = crate::value::DictMap::new();
    env.insert(crate::value::intern_key("ok"), VmValue::Bool(true));
    env.insert(crate::value::intern_key("data"), data);
    env.put_str("raw_text", outcome.raw_text.as_str());
    env.put_str("error", "");
    env.insert(crate::value::intern_key("error_category"), VmValue::Nil);
    env.insert(
        crate::value::intern_key("attempts"),
        VmValue::Int(outcome.attempts as i64),
    );
    env.insert(
        crate::value::intern_key("repaired"),
        VmValue::Bool(repaired),
    );
    env.insert(
        crate::value::intern_key("extracted_json"),
        VmValue::Bool(extracted_json),
    );
    env.insert(crate::value::intern_key("usage"), usage);
    env.put_str("model", model.as_str());
    env.put_str("provider", provider.as_str());
    VmValue::dict(env)
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

    let mut env = crate::value::DictMap::new();
    env.insert(crate::value::intern_key("ok"), VmValue::Bool(false));
    env.insert(crate::value::intern_key("data"), VmValue::Nil);
    env.put_str("raw_text", outcome.raw_text.as_str());
    env.put_str("error", message.as_str());
    env.put_str("error_category", kind.category());
    env.insert(
        crate::value::intern_key("attempts"),
        VmValue::Int(outcome.attempts as i64),
    );
    env.insert(
        crate::value::intern_key("repaired"),
        VmValue::Bool(repaired),
    );
    env.insert(
        crate::value::intern_key("extracted_json"),
        VmValue::Bool(extracted_json),
    );
    env.insert(crate::value::intern_key("usage"), usage);
    env.put_str("model", model.as_str());
    env.put_str("provider", provider.as_str());
    VmValue::dict(env)
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
    let mut env = crate::value::DictMap::new();
    env.insert(crate::value::intern_key("ok"), VmValue::Bool(false));
    env.insert(crate::value::intern_key("data"), VmValue::Nil);
    env.put_str("raw_text", "");
    env.put_str("error", message.as_str());
    env.put_str("error_category", category.as_str());
    env.insert(crate::value::intern_key("attempts"), VmValue::Int(0));
    env.insert(crate::value::intern_key("repaired"), VmValue::Bool(false));
    env.insert(
        crate::value::intern_key("extracted_json"),
        VmValue::Bool(false),
    );
    env.insert(
        crate::value::intern_key("usage"),
        VmValue::dict(empty_usage_dict()),
    );
    env.put_str("model", model);
    env.put_str("provider", provider);
    VmValue::dict(env)
}

fn envelope_from_arg_error(err: &VmError) -> VmValue {
    // Argument-shape failures don't have provider/model context yet —
    // surface them as `generic`-categorized via the standard transport
    // path so callers can branch on `ok` without try/catch.
    envelope_from_transport_error(err, "", "")
}

fn empty_usage_dict() -> crate::value::DictMap {
    let mut usage = crate::value::DictMap::new();
    usage.insert(crate::value::intern_key("input_tokens"), VmValue::Int(0));
    usage.insert(crate::value::intern_key("output_tokens"), VmValue::Int(0));
    usage.insert(
        crate::value::intern_key("cache_read_tokens"),
        VmValue::Int(0),
    );
    usage.insert(
        crate::value::intern_key("cache_write_tokens"),
        VmValue::Int(0),
    );
    usage.insert(
        crate::value::intern_key("cache_creation_input_tokens"),
        VmValue::Int(0),
    );
    usage.insert(
        crate::value::intern_key("cache_hit_ratio"),
        VmValue::Float(0.0),
    );
    usage.insert(
        crate::value::intern_key("cache_savings_usd"),
        VmValue::Float(0.0),
    );
    usage.insert(crate::value::intern_key("cost_usd"), VmValue::Nil);
    usage
}

fn build_usage_dict(outcome: &SchemaLoopOutcome) -> VmValue {
    let dict = match outcome.vm_result.as_dict() {
        Some(d) => d,
        None => return VmValue::dict(empty_usage_dict()),
    };
    if let Some(VmValue::Dict(usage)) = dict.get("usage") {
        return VmValue::Dict(usage.clone());
    }
    let mut usage = empty_usage_dict();
    for key in [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "cache_creation_input_tokens",
        "cache_hit_ratio",
        "cache_savings_usd",
        "cost_usd",
    ] {
        if let Some(v) = dict.get(key) {
            usage.insert(crate::value::intern_key(key), v.clone());
        }
    }
    VmValue::dict(usage)
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
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
) -> Result<VmValue, VmError> {
    run_structured_envelope(args, bridge).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmDictExt;

    fn outcome_with_errors(errors: Vec<&str>) -> SchemaLoopOutcome {
        SchemaLoopOutcome {
            vm_result: VmValue::dict(crate::value::DictMap::new()),
            raw_text: String::from("{\"verdict\":\"do"),
            errors: errors.into_iter().map(String::from).collect(),
            attempts: 1,
            schema_retries_budget: 2,
            output_validation_mode: String::from("error"),
        }
    }

    fn priced_outcome(errors: Vec<&str>) -> SchemaLoopOutcome {
        let result = crate::llm::api::LlmResult {
            text: "{\"decision\":\"wait\"}".to_string(),
            tool_calls: Vec::new(),
            raw_tool_calls: Vec::new(),
            input_tokens: 1_000,
            output_tokens: 1_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_supported: true,
            model: "claude-sonnet-4-20250514".to_string(),
            provider: "anthropic".to_string(),
            thinking: None,
            thinking_summary: None,
            stop_reason: Some("stop".to_string()),
            served_fast: false,
            blocks: Vec::new(),
            logprobs: Vec::new(),
            telemetry: crate::llm::api::ProviderTelemetry::default(),
        };
        SchemaLoopOutcome {
            vm_result: crate::llm::api::vm_build_llm_result(&result, None, None, None),
            raw_text: String::from("{\"decision\":\"wait\"}"),
            errors: errors.into_iter().map(String::from).collect(),
            attempts: 1,
            schema_retries_budget: 2,
            output_validation_mode: String::from("error"),
        }
    }

    fn envelope_usage(envelope: &VmValue) -> &crate::value::DictMap {
        let dict = envelope.as_dict().expect("envelope dict");
        let Some(VmValue::Dict(usage)) = dict.get("usage") else {
            panic!("missing usage dict: {dict:?}");
        };
        usage
    }

    #[test]
    fn structured_success_and_validation_failure_preserve_final_usage() {
        let _guard = crate::llm::env_guard();
        crate::llm_config::clear_user_overrides();
        let outcome = priced_outcome(vec!["decision must be merge"]);
        let canonical_usage = outcome
            .vm_result
            .as_dict()
            .and_then(|dict| dict.get("usage"))
            .expect("canonical usage");
        let expected_cost = crate::llm::cost::pricing_aware_call_cost(
            "anthropic",
            "claude-sonnet-4-20250514",
            1_000,
            1_000,
        )
        .expect("catalog-priced result");

        let success = envelope_success(&outcome, false);
        let failure = envelope_failure(&outcome, EnvelopeFailureKind::SchemaValidation, false);

        let canonical_usage = crate::llm::vm_value_to_json(canonical_usage);
        let success_usage =
            crate::llm::vm_value_to_json(&VmValue::Dict(envelope_usage(&success).clone()));
        let failure_usage =
            crate::llm::vm_value_to_json(&VmValue::Dict(envelope_usage(&failure).clone()));
        assert_eq!(success_usage, canonical_usage);
        assert_eq!(failure_usage, canonical_usage);
        assert_eq!(success_usage["cost_usd"], serde_json::json!(expected_cost));
        assert_eq!(failure_usage["cost_usd"], serde_json::json!(expected_cost));
    }

    #[test]
    fn empty_structured_usage_keeps_cost_unknown() {
        let transport = envelope_from_transport_error(
            &VmError::Runtime("offline".to_string()),
            "nonexistent_provider",
            "ghost-model",
        );
        let usage =
            crate::llm::vm_value_to_json(&VmValue::Dict(envelope_usage(&transport).clone()));
        assert_eq!(
            usage.as_object().and_then(|usage| usage.get("cost_usd")),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn token_limit_truncation_gets_its_own_integrity_category() {
        // The canonical marker `structured_output_errors` appends on a length
        // stop_reason must be classified as `length_truncation`, NOT lumped
        // into the generic `missing_json` bucket — otherwise a dead (truncated)
        // judge is invisible to the meter.
        let outcome = outcome_with_errors(vec![
            "response did not contain parseable JSON",
            "response hit the token limit before producing complete JSON",
        ]);
        assert!(outcome_hit_token_limit(&outcome));
        let kind = classify_main_failure(&outcome);
        assert_eq!(kind.category(), "length_truncation");
    }

    #[test]
    fn non_truncation_missing_json_stays_missing_json() {
        let outcome = outcome_with_errors(vec!["response did not contain parseable JSON"]);
        assert!(!outcome_hit_token_limit(&outcome));
        assert_eq!(classify_main_failure(&outcome).category(), "missing_json");
    }

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
    fn structured_output_rejection_detects_provider_400() {
        // The OpenRouter/upstream relay shape we degrade on.
        let rejected = VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "openrouter HTTP 400 Bad Request [invalid_request]: Provider returned error",
        )));
        assert!(is_structured_output_rejection(&rejected));
        // An unrelated transport failure must NOT trigger the structured fallback.
        let unrelated = VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "connection reset by peer",
        )));
        assert!(!is_structured_output_rejection(&unrelated));
    }

    #[test]
    fn response_format_guard_skips_text_mode_requests() {
        // No options dict: structured envelope defaults to json_schema -> counts.
        assert!(structured_request_uses_response_format(&[
            VmValue::Nil,
            VmValue::Nil
        ]));
        // Already prompt-mode text transport -> do NOT re-degrade.
        let mut text_opts = crate::value::DictMap::new();
        text_opts.put_str("output_format", "text");
        assert!(!structured_request_uses_response_format(&[
            VmValue::Nil,
            VmValue::Nil,
            VmValue::dict(text_opts),
        ]));
        // json_object still sends a response_format -> degradable.
        let mut json_object_opts = crate::value::DictMap::new();
        json_object_opts.put_str("output_format", "json_object");
        assert!(structured_request_uses_response_format(&[
            VmValue::Nil,
            VmValue::Nil,
            VmValue::dict(json_object_opts),
        ]));
    }

    #[test]
    fn merge_repair_caps_schema_retries_and_drops_nested_repair() {
        let mut base = crate::value::DictMap::new();
        base.put_str("provider", "auto");
        base.insert(crate::value::intern_key("schema_retries"), VmValue::Int(5));
        base.insert(
            crate::value::intern_key("repair"),
            VmValue::dict(crate::value::DictMap::new()),
        );
        let overrides = {
            let mut o = crate::value::DictMap::new();
            o.put_str("model", "local:fix");
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
        let dict_no_enabled =
            parse_repair_value(&VmValue::dict(crate::value::DictMap::new())).unwrap();
        assert!(dict_no_enabled.enabled);
        let mut disabled = crate::value::DictMap::new();
        disabled.insert(crate::value::intern_key("enabled"), VmValue::Bool(false));
        let dict_disabled = parse_repair_value(&VmValue::dict(disabled)).unwrap();
        assert!(!dict_disabled.enabled);
    }

    #[test]
    fn prompt_mode_structured_transport_keeps_harn_side_validation() {
        let schema = VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("type"),
                VmValue::String(arcstr::ArcStr::from("object")),
            ),
            (
                crate::value::intern_key("properties"),
                VmValue::dict(crate::value::DictMap::from_iter([(
                    crate::value::intern_key("pass"),
                    VmValue::dict(crate::value::DictMap::from_iter([(
                        crate::value::intern_key("type"),
                        VmValue::String(arcstr::ArcStr::from("boolean")),
                    )])),
                )])),
            ),
        ]));
        let options = VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("ollama")),
            ),
            (
                crate::value::intern_key("model"),
                // A capability rule with no structured_output transport, so native
                // structured transport is unsupported and the loop must fall back to
                // prompt-mode. (ollama gemma4/devstral now declare format_kw support.)
                VmValue::String(arcstr::ArcStr::from("llava:latest")),
            ),
        ]));
        let mut args = crate::llm::rewrite_structured_args(vec![
            VmValue::String(arcstr::ArcStr::from("Return a completion verdict.")),
            schema,
            options,
        ])
        .unwrap();

        let err = match extract_llm_options(&args) {
            Ok(_) => panic!("native structured transport should be unsupported"),
            Err(err) => err,
        };
        assert!(is_unsupported_structured_transport_error(&err));

        apply_prompt_mode_structured_transport(&mut args);
        let opts = extract_llm_options(&args).expect("prompt-mode structured call should parse");
        assert!(matches!(
            opts.output_format,
            crate::llm::api::OutputFormat::Text
        ));
        assert!(
            opts.output_schema.is_some(),
            "Harn must still validate the model's JSON after prompt-mode fallback"
        );
        assert!(opts.messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("Return only JSON that conforms to this JSON Schema"));
    }
}
