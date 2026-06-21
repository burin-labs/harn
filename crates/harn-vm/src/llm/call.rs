use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::{json_to_vm_value, schema_result_value};
use crate::value::{VmError, VmValue};

use super::helpers::extract_llm_options;
use super::trace::{emit_agent_event, AgentTraceEvent};
use super::{
    agent_config, agent_observe, api,
    api::{parse_schema_stream_abort, schema_stream_aborted_result_value, SchemaStreamAbort},
    helpers, routing, structural_experiments,
};

thread_local! {
    static IN_FLIGHT_LLM_CALLS: RefCell<BTreeMap<String, InFlightLlmCall>> = const { RefCell::new(std::collections::BTreeMap::new()) };
}

#[derive(Clone, Debug)]
struct InFlightLlmCall {
    call_id: String,
    model: String,
    role: String,
    started_at_ms: i64,
}

pub(crate) struct InFlightLlmCallGuard {
    call_id: String,
}

impl InFlightLlmCallGuard {
    pub(crate) fn enter(opts: &api::LlmCallOptions) -> Self {
        let call_id = format!("llm_call_{}", uuid::Uuid::now_v7());
        let started_at_ms = crate::stdlib::clock::now_wall_ms();
        let role = opts
            .messages
            .last()
            .and_then(|message| message.get("role"))
            .and_then(|role| role.as_str())
            .unwrap_or("user")
            .to_string();
        let snapshot = InFlightLlmCall {
            call_id: call_id.clone(),
            model: opts.model.clone(),
            role,
            started_at_ms,
        };
        IN_FLIGHT_LLM_CALLS.with(|calls| {
            calls.borrow_mut().insert(call_id.clone(), snapshot);
        });
        Self { call_id }
    }
}

impl Drop for InFlightLlmCallGuard {
    fn drop(&mut self) {
        IN_FLIGHT_LLM_CALLS.with(|calls| {
            calls.borrow_mut().remove(&self.call_id);
        });
    }
}

pub(crate) fn snapshot_in_flight_llm_calls() -> Vec<serde_json::Value> {
    IN_FLIGHT_LLM_CALLS.with(|calls| {
        let calls = calls.borrow();
        if calls.is_empty() {
            return Vec::new();
        }
        let now_ms = crate::stdlib::clock::now_wall_ms();
        calls
            .values()
            .map(|call| {
                serde_json::json!({
                    "call_id": call.call_id.clone(),
                    "model": call.model.clone(),
                    "role": call.role.clone(),
                    "started_at_ms": call.started_at_ms,
                    "age_ms": now_ms.saturating_sub(call.started_at_ms).max(0),
                })
            })
            .collect()
    })
}

pub(crate) fn clear_in_flight_llm_calls() {
    IN_FLIGHT_LLM_CALLS.with(|calls| calls.borrow_mut().clear());
}

fn output_validation_mode(opts: &api::LlmCallOptions) -> &str {
    opts.output_validation.as_deref().unwrap_or("off")
}

fn schema_validation_errors(result: &VmValue) -> Vec<String> {
    match result {
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Err") => {
            enum_variant
                .fields
                .first()
                .and_then(|payload| payload.as_dict())
                .and_then(|payload| payload.get("errors"))
                .and_then(|errors| match errors {
                    VmValue::List(items) => Some(items.iter().map(|err| err.display()).collect()),
                    _ => None,
                })
                .unwrap_or_else(|| vec!["schema validation failed".to_string()])
        }
        _ => Vec::new(),
    }
}

/// Compute schema validation errors against `opts.output_schema` without
/// deciding disposition (warn vs error vs off). Returns an empty vec when
/// no schema is configured or the data validates. Used by the schema-retry
/// loop in `llm_call`.
pub(super) fn compute_validation_errors(data: &VmValue, opts: &api::LlmCallOptions) -> Vec<String> {
    let Some(schema_json) = &opts.output_schema else {
        return Vec::new();
    };
    let schema_vm = json_to_vm_value(schema_json);
    let validation = schema_result_value(data, &schema_vm, false);
    schema_validation_errors(&validation)
}

pub(crate) fn structured_output_errors(
    result: &VmValue,
    opts: &api::LlmCallOptions,
) -> Vec<String> {
    let Some(dict) = result.as_dict() else {
        return vec!["structured output result was not a dict".to_string()];
    };
    if let Some(data) = dict.get("data") {
        return compute_validation_errors(data, opts);
    }

    let mut errors = vec!["response did not contain parseable JSON".to_string()];
    if let Some(VmValue::List(violations)) = dict.get("protocol_violations") {
        let joined = violations
            .iter()
            .map(VmValue::display)
            .collect::<Vec<_>>()
            .join("; ");
        if !joined.is_empty() {
            errors.push(format!("protocol violations: {joined}"));
        }
    }
    if let Some(stop_reason) = dict.get("stop_reason").map(VmValue::display) {
        // Reuse the single canonical truncation classifier instead of a
        // partial, case-sensitive literal match. The hand-rolled
        // `matches!(.., "length" | "max_tokens")` missed Gemini/Vertex, which
        // report `MAX_TOKENS` (uppercase) — those native responses passed their
        // raw `finishReason` through unnormalized, so a truncated structured
        // output was misreported as "did not contain parseable JSON" rather
        // than a token-limit hit. `is_length_truncation` is case-insensitive
        // and covers `length`/`max_tokens`/`MAX_TOKENS`/etc. for free.
        if super::agent_session_host::is_length_truncation(Some(stop_reason.as_str())) {
            errors.push("response hit the token limit before producing complete JSON".to_string());
        }
    }
    errors
}

/// How `llm_call` should nudge the model when `output_schema` validation
/// fails and `schema_retries > 0`.
#[derive(Debug, Clone)]
pub(crate) enum SchemaNudge {
    /// Build a default corrective user message from the schema's top-level
    /// `required` / `properties` keys plus the validation errors. This is
    /// the default when `schema_retry_nudge` is unset or `true`.
    Auto,
    /// Use the caller's string verbatim (plus a short tail listing the
    /// validation errors).
    Verbatim(String),
    /// Retry without appending any corrective message (bare retry).
    /// Selected when `schema_retry_nudge: false`.
    Disabled,
}

pub(crate) fn parse_schema_nudge(options: &Option<crate::value::DictMap>) -> SchemaNudge {
    let Some(opts) = options.as_ref() else {
        return SchemaNudge::Auto;
    };
    match opts.get("schema_retry_nudge") {
        None | Some(VmValue::Nil) => SchemaNudge::Auto,
        Some(VmValue::Bool(true)) => SchemaNudge::Auto,
        Some(VmValue::Bool(false)) => SchemaNudge::Disabled,
        Some(VmValue::String(s)) => SchemaNudge::Verbatim(s.to_string()),
        Some(other) => SchemaNudge::Verbatim(other.display()),
    }
}

/// Build the corrective user message appended before a schema-retry
/// attempt. Callers that want full control pass a string via
/// `schema_retry_nudge`; the `Auto` variant summarizes the schema shape
/// so small / local models can fix nested object mistakes without seeing
/// the whole JSON Schema again (see `docs/llm/harn-quickref.md` "Schema
/// retries").
pub(crate) fn build_schema_nudge(
    errors: &[String],
    schema: Option<&serde_json::Value>,
    mode: &SchemaNudge,
) -> String {
    let errors_line = if errors.is_empty() {
        String::from("(no detailed errors)")
    } else {
        errors.join("; ")
    };
    match mode {
        SchemaNudge::Disabled => String::new(),
        SchemaNudge::Verbatim(s) => {
            format!("{s}\n\nValidation errors: {errors_line}")
        }
        SchemaNudge::Auto => {
            let mut required_keys: Vec<String> = Vec::new();
            let mut property_keys: Vec<String> = Vec::new();
            let mut shape_lines: Vec<String> = Vec::new();
            if let Some(schema) = schema {
                if let Some(req) = schema.get("required").and_then(|v| v.as_array()) {
                    for r in req {
                        if let Some(k) = r.as_str() {
                            required_keys.push(k.to_string());
                        }
                    }
                }
                if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
                    for k in props.keys() {
                        property_keys.push(k.clone());
                    }
                }
                collect_schema_shape_lines(schema, "root", 0, &mut shape_lines);
            }
            let mut msg =
                String::from("Your previous response did not match the required JSON schema.");
            msg.push_str(&format!("\nValidation errors: {errors_line}."));
            if !required_keys.is_empty() {
                msg.push_str(&format!("\nRequired keys: {}.", required_keys.join(", ")));
            }
            if !property_keys.is_empty() {
                msg.push_str(&format!(
                    "\nAllowed top-level keys: {}.",
                    property_keys.join(", ")
                ));
            }
            if !shape_lines.is_empty() {
                msg.push_str("\nExpected JSON schema shape:");
                for line in shape_lines {
                    msg.push_str("\n- ");
                    msg.push_str(&line);
                }
            }
            msg.push_str(
                "\nRespond again with ONLY valid JSON conforming to the schema. No prose, no markdown fences.",
            );
            msg
        }
    }
}

const SCHEMA_NUDGE_MAX_DEPTH: usize = RuntimeLimits::DEFAULT.max_schema_nudge_depth;
const SCHEMA_NUDGE_MAX_LINES: usize = RuntimeLimits::DEFAULT.max_schema_nudge_lines;
const SCHEMA_NUDGE_MAX_KEYS: usize = RuntimeLimits::DEFAULT.max_schema_nudge_keys;

fn collect_schema_shape_lines(
    schema: &serde_json::Value,
    path: &str,
    depth: usize,
    lines: &mut Vec<String>,
) {
    if depth > SCHEMA_NUDGE_MAX_DEPTH || lines.len() >= SCHEMA_NUDGE_MAX_LINES {
        return;
    }

    let object_like = schema
        .get("type")
        .and_then(|value| value.as_str())
        .is_some_and(|kind| kind == "object")
        || schema.get("properties").is_some();
    if object_like {
        if let Some(props) = schema.get("properties").and_then(|value| value.as_object()) {
            let mut keys = props.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            if !keys.is_empty() {
                let mut line = format!("{path} object allowed keys: {}", join_limited_keys(&keys));
                let required = schema_required_keys(schema);
                if !required.is_empty() {
                    line.push_str(&format!(
                        "; required keys: {}",
                        join_limited_keys(&required)
                    ));
                }
                lines.push(line);
            }

            for key in keys {
                if lines.len() >= SCHEMA_NUDGE_MAX_LINES {
                    break;
                }
                if let Some(child_schema) = props.get(&key) {
                    let child_path = if path == "root" {
                        key
                    } else {
                        format!("{path}.{key}")
                    };
                    collect_schema_shape_lines(child_schema, &child_path, depth + 1, lines);
                }
            }
        }
    }

    let array_like = schema
        .get("type")
        .and_then(|value| value.as_str())
        .is_some_and(|kind| kind == "array")
        || schema.get("items").is_some();
    if array_like {
        if let Some(items) = schema.get("items") {
            collect_schema_shape_lines(items, &format!("{path}[]"), depth + 1, lines);
        }
    }
}

fn schema_required_keys(schema: &serde_json::Value) -> Vec<String> {
    let mut keys = schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    keys.sort();
    keys
}

fn join_limited_keys(keys: &[String]) -> String {
    if keys.len() <= SCHEMA_NUDGE_MAX_KEYS {
        return keys.join(", ");
    }
    format!(
        "{}, ... (+{} more)",
        keys[..SCHEMA_NUDGE_MAX_KEYS].join(", "),
        keys.len() - SCHEMA_NUDGE_MAX_KEYS
    )
}

/// Shared implementation of `llm_call` / `llm_call_safe`. Runs the
/// full schema-retry loop; on success returns the LLM result dict, on
/// failure returns the underlying `VmError`. `llm_call` propagates the
/// error (re-wrapped as a thrown `{kind, reason, category, message,
/// retry_after_ms?, provider, model}` dict so catch blocks can dispatch
/// on the LLM error taxonomy);
/// `llm_call_safe` wraps it in a `{ok: false, error: …}` envelope with
/// the same fields.
pub(super) async fn llm_call_impl(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let options = args.get(2).and_then(|a| a.as_dict()).cloned();
    let opts = extract_llm_options(&args)?;
    let provider = opts.provider.clone();
    let model = opts.model.clone();
    // Publish the resolved provider/model/capabilities to templates
    // rendered while this `llm_call` frame is on the stack — e.g.
    // schema-retry prompts or middleware that re-renders a partial
    // for a corrective second pass.
    let _llm_render_guard = crate::stdlib::template::LlmRenderContextGuard::enter(
        crate::stdlib::template::LlmRenderContext::resolve(&provider, &model),
    );
    // `execute_llm_call` records the resolved provider/model into the
    // runtime-introspection snapshot — that's the single DRY point for
    // every llm_call path (plain, bridged, structured), so we don't
    // also record here.
    match execute_llm_call(ctx, opts, options, None).await {
        Ok(v) => Ok(v),
        Err(err) => Err(VmError::Thrown(build_llm_error_dict(
            &err, &provider, &model,
        ))),
    }
}

/// Build the `{kind, reason, category, message, retry_after_ms?, provider, model}`
/// dict that `llm_call` throws on failure. `retry_after_ms` is only
/// set when the underlying error carries a parseable `retry-after:`
/// hint, so callers can pattern-match on its presence:
///
/// ```harn
/// try { llm_call(prompt, nil, opts) } catch (e) {
///   if e.kind == "transient" && e.reason == "rate_limit" {
///     sleep(e.retry_after_ms ?? 1000)
///   }
/// }
/// ```
pub(crate) fn build_llm_error_dict(err: &VmError, provider: &str, model: &str) -> VmValue {
    let category = crate::value::error_to_category(err);
    let message = llm_error_message(err);
    let llm_error = api::classify_llm_error(category.clone(), &message);
    if let VmError::Thrown(VmValue::Dict(existing)) = err {
        let mut dict = existing.as_ref().clone();
        dict.entry(crate::value::intern_key("category"))
            .or_insert_with(|| VmValue::String(arcstr::ArcStr::from(category.as_str())));
        dict.entry(crate::value::intern_key("kind"))
            .or_insert_with(|| VmValue::String(arcstr::ArcStr::from(llm_error.kind.as_str())));
        dict.entry(crate::value::intern_key("reason"))
            .or_insert_with(|| VmValue::String(arcstr::ArcStr::from(llm_error.reason.as_str())));
        dict.entry(crate::value::intern_key("message"))
            .or_insert_with(|| VmValue::String(arcstr::ArcStr::from(message.as_str())));
        dict.put_str("provider", provider);
        dict.put_str("model", model);
        return VmValue::dict(dict);
    }
    let mut dict = std::collections::BTreeMap::new();
    dict.put_str("category", category.as_str());
    dict.put_str("kind", llm_error.kind.as_str());
    dict.put_str("reason", llm_error.reason.as_str());
    dict.put_str("message", message);
    if let Some(ms) = agent_observe::extract_retry_after_ms(err) {
        dict.insert("retry_after_ms".to_string(), VmValue::Int(ms as i64));
    }
    dict.put_str("provider", provider);
    dict.put_str("model", model);
    VmValue::dict(dict)
}

fn llm_error_message(err: &VmError) -> String {
    match err {
        VmError::CategorizedError { message, .. } => message.clone(),
        VmError::Thrown(VmValue::String(s)) => s.to_string(),
        VmError::Thrown(VmValue::Dict(d)) => d
            .get("message")
            .map(|v| v.display())
            .unwrap_or_else(|| err.to_string()),
        _ => err.to_string(),
    }
}

pub(crate) async fn execute_llm_call(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    opts: api::LlmCallOptions,
    options: Option<crate::value::DictMap>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
) -> Result<VmValue, VmError> {
    // Publish the resolved provider/model facts for the introspection
    // tool surface (current_model() / current_provider() / ...). All
    // llm_call code paths funnel through this function — the bridged
    // `llm_call_with_bridge`, structured variants, and the plain
    // `llm_call_impl` — so recording here is the single DRY point.
    super::introspection::record_resolved_llm_call(&opts.provider, &opts.model);
    let outcome = if let Some(policy) = opts.routing_policy.clone() {
        execute_routing_schema_retry_loop(ctx, policy, opts, options, bridge).await?
    } else {
        execute_schema_retry_loop(ctx, opts, options, bridge).await?
    };
    if outcome.errors.is_empty() {
        return Ok(outcome.vm_result);
    }
    // Schema retries exhausted — honor the caller's output_validation mode.
    let hint = if outcome.schema_retries_budget == 0 {
        " (hint: set `schema_retries: N` in the llm_call options to automatically re-prompt the model with a corrective nudge)"
    } else {
        " (hint: schema_retries budget exhausted — the model did not produce conforming output after the configured retries; consider raising `schema_retries` or relaxing the schema)"
    };
    let message = format!(
        "LLM output failed schema validation: {}{hint}",
        outcome.errors.join("; ")
    );
    match outcome.output_validation_mode.as_str() {
        "error" => Err(crate::value::VmError::CategorizedError {
            message,
            category: crate::value::ErrorCategory::SchemaValidation,
        }),
        "warn" => {
            crate::events::log_warn("llm", &message);
            Ok(outcome.vm_result)
        }
        _ => Ok(outcome.vm_result),
    }
}

/// Dispatch through the first-class routing policy executor. Each
/// chain link is tried in order with failover, optional latency-aware
/// racing, and per-call / session budget enforcement; the winning
/// link's result is wrapped in the standard `llm_call` envelope plus
/// a `routing` block summarizing every attempt.
async fn execute_routing_schema_retry_loop(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    policy: Arc<routing::RoutingPolicyConfig>,
    mut opts: api::LlmCallOptions,
    options: Option<crate::value::DictMap>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
) -> Result<SchemaLoopOutcome, VmError> {
    let _ = structural_experiments::apply_structural_experiment(ctx, &mut opts, None).await?;
    let schema_retries = helpers::opt_int(&options, "schema_retries")
        .unwrap_or(1)
        .max(0) as usize;
    let nudge_mode = parse_schema_nudge(&options);
    let output_validation_mode = output_validation_mode(&opts).to_string();
    let expects_structured = helpers::expects_structured_output(&opts);
    let original_messages = opts.messages.clone();

    for attempt in 0..=schema_retries {
        let (vm_result, raw_text, errors) =
            match routing::execute_with_routing(&policy, opts.clone(), bridge).await {
                Ok((result, trace)) => {
                    let raw_text = result.text.clone();
                    // Snap option metadata to the winning link so transcript / portal
                    // payloads describe the call that actually ran.
                    opts.provider = result.provider.clone();
                    opts.model = result.model.clone();
                    opts.routing_decision = Some(routing::trace_to_decision(&trace, &policy));
                    let envelope = attach_routing_block(
                        agent_config::build_llm_call_result(&result, &opts),
                        &trace,
                        &policy,
                    );
                    if !expects_structured {
                        return Ok(SchemaLoopOutcome {
                            vm_result: envelope,
                            raw_text,
                            errors: Vec::new(),
                            attempts: attempt + 1,
                            schema_retries_budget: schema_retries,
                            output_validation_mode,
                        });
                    }
                    let errors = structured_output_errors(&envelope, &opts);
                    (envelope, raw_text, errors)
                }
                Err(error) => match parse_schema_stream_abort(&error) {
                    Some(abort) => {
                        let errors = vec![schema_stream_abort_message(&abort)];
                        let vm_result = schema_stream_aborted_result_value(&abort);
                        (vm_result, String::new(), errors)
                    }
                    None => return Err(error),
                },
            };
        if errors.is_empty() {
            return Ok(SchemaLoopOutcome {
                vm_result,
                raw_text,
                errors,
                attempts: attempt + 1,
                schema_retries_budget: schema_retries,
                output_validation_mode,
            });
        }

        let more_attempts = attempt < schema_retries;
        if more_attempts {
            escalate_max_tokens_on_truncation(&mut opts, &errors);
            let nudge = build_schema_nudge(&errors, opts.output_schema.as_ref(), &nudge_mode);
            emit_agent_event(AgentTraceEvent::SchemaRetry {
                attempt: attempt + 1,
                errors: errors.clone(),
                nudge_used: !nudge.is_empty(),
                correction_prompt: nudge.clone(),
            });
            opts.messages = original_messages.clone();
            if !nudge.is_empty() {
                opts.messages.push(serde_json::json!({
                    "role": "user",
                    "content": nudge,
                }));
            }
            continue;
        }

        return Ok(SchemaLoopOutcome {
            vm_result,
            raw_text,
            errors,
            attempts: attempt + 1,
            schema_retries_budget: schema_retries,
            output_validation_mode,
        });
    }
    unreachable!("routing schema retry loop exited without returning");
}

fn attach_routing_block(
    envelope: VmValue,
    trace: &routing::RoutingTrace,
    policy: &routing::RoutingPolicyConfig,
) -> VmValue {
    let VmValue::Dict(dict) = envelope else {
        return envelope;
    };
    let mut dict = dict.as_ref().clone();
    let mut routing_dict = std::collections::BTreeMap::new();
    let label = if trace.label.is_empty() {
        policy.label.clone()
    } else {
        trace.label.clone()
    };
    routing_dict.put_str("policy", label);
    routing_dict.insert("attempts".to_string(), routing::trace_to_vm_attempts(trace));
    if let Some(selected) = trace.selected {
        routing_dict.insert("selected".to_string(), VmValue::Int(selected as i64));
    }
    routing_dict.insert(
        "session_cost_usd".to_string(),
        VmValue::Float(trace.session_cost_usd),
    );
    dict.insert(
        crate::value::intern_key("routing"),
        VmValue::dict(routing_dict),
    );
    VmValue::dict(dict)
}

/// Outcome of the schema-retry loop, exposing both the final attempt's
/// payload and the telemetry that envelope-shaped callers (e.g.
/// `llm_call_structured_result`) need to surface diagnostics. Transport
/// errors short-circuit the loop and propagate as `Err`; schema failures
/// after exhaustion return `Ok(...)` with `errors` populated so the
/// caller can decide between throwing, warning, or building a result
/// envelope.
pub(crate) struct SchemaLoopOutcome {
    /// Final attempt's vm_result dict (regardless of validation success).
    pub vm_result: VmValue,
    /// Final attempt's raw model text — preserved for diagnostics and
    /// repair input even when JSON couldn't be extracted.
    pub raw_text: String,
    /// Validation errors from the final attempt (empty = success).
    pub errors: Vec<String>,
    /// Number of model calls made (1-indexed; 1 = no retries used).
    pub attempts: usize,
    /// Configured `schema_retries` budget (0 means "no retries").
    pub schema_retries_budget: usize,
    /// `output_validation` mode the caller configured (off / warn / error).
    pub output_validation_mode: String,
}

pub(crate) async fn execute_schema_retry_loop(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    mut opts: api::LlmCallOptions,
    options: Option<crate::value::DictMap>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
) -> Result<SchemaLoopOutcome, VmError> {
    let _ = structural_experiments::apply_structural_experiment(ctx, &mut opts, None).await?;
    let retry_config = agent_observe::LlmRetryConfig {
        retries: helpers::opt_int(&options, "llm_retries")
            .unwrap_or(agent_observe::DEFAULT_LLM_CALL_RETRIES as i64)
            .max(0) as usize,
        backoff_ms: helpers::opt_int(&options, "llm_backoff_ms")
            .unwrap_or(agent_observe::DEFAULT_LLM_CALL_BACKOFF_MS as i64)
            .max(0) as u64,
    };
    // Schema retry loop is orthogonal to transient retries. Each
    // schema retry gets a fresh transient budget. Small/local models
    // often need the corrective nudge to produce conforming JSON.
    let schema_retries = helpers::opt_int(&options, "schema_retries")
        .unwrap_or(1)
        .max(0) as usize;
    let nudge_mode = parse_schema_nudge(&options);

    let tool_format = helpers::opt_str(&options, "tool_format");
    let bridged = bridge.is_some();
    let user_visible = bridged && helpers::opt_bool(&options, "user_visible");
    let output_validation_mode = output_validation_mode(&opts).to_string();
    let expects_structured = helpers::expects_structured_output(&opts);
    // Snapshot the caller's original messages once. Each schema retry
    // replays this snapshot plus a single corrective user message, so
    // the invalid response never pollutes subsequent attempts — the
    // retry is a single-turn correction rather than a multi-turn
    // conversation.
    let original_messages = opts.messages.clone();
    for attempt in 0..=schema_retries {
        let call_result = agent_observe::observed_llm_call(
            &opts,
            tool_format.as_deref(),
            bridge,
            &retry_config,
            None,
            user_visible,
            bridged, // offthread=true on the bridge path, local set otherwise
            // Top-level `llm_call` host calls don't have a session in the
            // sense the streaming detector needs (no agent loop, no
            // session_id), so skip candidate detection here. The agent
            // loop's `run_llm_call` is the integration point that owns it.
            None,
        )
        .await;

        // A mid-stream schema abort short-circuits the provider call but
        // is otherwise equivalent to a normal schema-validation failure:
        // it consumes one `schema_retries` slot and feeds its
        // `path` + `reason` into the corrective nudge so the next attempt
        // gets a sharper prompt than a generic "stream failed".
        let (vm_result, raw_text, errors) = match call_result {
            Ok(result) => {
                let raw_text = result.text.clone();
                let vm_result = agent_config::build_llm_call_result(&result, &opts);
                if !expects_structured {
                    return Ok(SchemaLoopOutcome {
                        vm_result,
                        raw_text,
                        errors: Vec::new(),
                        attempts: attempt + 1,
                        schema_retries_budget: schema_retries,
                        output_validation_mode,
                    });
                }
                let errors = structured_output_errors(&vm_result, &opts);
                (vm_result, raw_text, errors)
            }
            Err(error) => match parse_schema_stream_abort(&error) {
                Some(abort) => {
                    let errors = vec![schema_stream_abort_message(&abort)];
                    let vm_result = schema_stream_aborted_result_value(&abort);
                    (vm_result, String::new(), errors)
                }
                None => return Err(error),
            },
        };

        if errors.is_empty() {
            return Ok(SchemaLoopOutcome {
                vm_result,
                raw_text,
                errors,
                attempts: attempt + 1,
                schema_retries_budget: schema_retries,
                output_validation_mode,
            });
        }

        let more_attempts = attempt < schema_retries;
        if more_attempts {
            escalate_max_tokens_on_truncation(&mut opts, &errors);
            let nudge = build_schema_nudge(&errors, opts.output_schema.as_ref(), &nudge_mode);
            emit_agent_event(AgentTraceEvent::SchemaRetry {
                attempt: attempt + 1,
                errors: errors.clone(),
                nudge_used: !nudge.is_empty(),
                correction_prompt: nudge.clone(),
            });
            // Replay the original messages with a single corrective
            // user turn appended. The invalid assistant response is
            // deliberately dropped — smaller / local models get
            // confused by a user→assistant(bad)→user(nudge)→assistant
            // shape, and the verbatim bad response otherwise sits in
            // context for every subsequent retry.
            opts.messages = original_messages.clone();
            if !nudge.is_empty() {
                opts.messages.push(serde_json::json!({
                    "role": "user",
                    "content": nudge,
                }));
            }
            continue;
        }

        // Attempts exhausted with errors. Surface the failure to the caller.
        return Ok(SchemaLoopOutcome {
            vm_result,
            raw_text,
            errors,
            attempts: attempt + 1,
            schema_retries_budget: schema_retries,
            output_validation_mode,
        });
    }
    unreachable!("schema retry loop exited without returning");
}

/// Hard ceiling for an auto-escalated structured-output retry budget. A
/// single doubling step is usually enough to clear a reasoning model's
/// hidden-channel spend; the ceiling stops a pathological loop from
/// requesting an unbounded completion when the model never converges.
const MAX_TOKENS_RETRY_CEILING: i64 = 32_768;

/// When a structured/schema attempt failed because the response ran out of
/// output-token budget mid-JSON (`is_length_truncation` -> the canonical
/// "hit the token limit" marker `structured_output_errors` appends), grow
/// `opts.max_tokens` before the retry so the next attempt has room to emit a
/// complete object.
///
/// This is the generalizing fix for reasoning models (gpt-oss/Harmony,
/// DeepSeek-R, o-series): their reasoning/analysis channel is billed against
/// the same output budget but is invisible in the parsed text, so a
/// `max_tokens` that comfortably fits a non-reasoning model's JSON is consumed
/// entirely by reasoning, truncating the visible JSON to empty. Replaying the
/// identical under-budget just re-truncates — burning a retry slot and, once
/// the slots are exhausted, returning a DEAD `length_truncation` envelope (an
/// empty judge verdict that silently falls through to the deterministic
/// grader). Returns `true` when the budget was grown so the caller can trace
/// the escalation.
fn escalate_max_tokens_on_truncation(opts: &mut api::LlmCallOptions, errors: &[String]) -> bool {
    let truncated = errors.iter().any(|e| e.contains("hit the token limit"));
    if !truncated || opts.max_tokens >= MAX_TOKENS_RETRY_CEILING {
        return false;
    }
    let before = opts.max_tokens;
    let grown = before
        .saturating_mul(2)
        .clamp(before + 1, MAX_TOKENS_RETRY_CEILING);
    opts.max_tokens = grown;
    crate::events::log_info(
        "llm",
        &format!(
            "structured retry: response truncated at max_tokens={before}; \
             escalating retry budget to max_tokens={grown} \
             (reasoning channel can consume the output budget)"
        ),
    );
    true
}

/// Render the schema-stream abort as a validation-style error string so
/// the existing nudge builder can fold it into its corrective prompt
/// alongside post-hoc validation errors.
fn schema_stream_abort_message(abort: &SchemaStreamAbort) -> String {
    format!(
        "streaming response aborted at {path}: {reason} (after {chunks} chunk{plural})",
        path = abort.path,
        reason = abort.reason,
        chunks = abort.chunks_consumed,
        plural = if abort.chunks_consumed == 1 { "" } else { "s" },
    )
}

pub(super) fn llm_safe_envelope_ok(response: VmValue) -> VmValue {
    let mut dict = std::collections::BTreeMap::new();
    dict.insert("ok".to_string(), VmValue::Bool(true));
    dict.insert("response".to_string(), response);
    dict.insert("error".to_string(), VmValue::Nil);
    VmValue::dict(dict)
}

pub(super) fn llm_safe_envelope_err(err: &VmError) -> VmValue {
    // `llm_call_impl` pre-wraps its errors into a
    // `VmError::Thrown(VmValue::Dict{kind, reason, category, message,
    // retry_after_ms?, provider, model})`. Pass that dict through verbatim so
    // `llm_call_safe` callers see the same fields as `try/catch`
    // users — with `category` / `message` always populated.
    if let VmError::Thrown(VmValue::Dict(d)) = err {
        let mut dict = std::collections::BTreeMap::new();
        dict.insert("ok".to_string(), VmValue::Bool(false));
        dict.insert("response".to_string(), VmValue::Nil);
        dict.insert("error".to_string(), VmValue::Dict(d.clone()));
        return VmValue::dict(dict);
    }
    let category = crate::value::error_to_category(err);
    let message = llm_error_message(err);
    let llm_error = api::classify_llm_error(category.clone(), &message);
    let mut err_dict = std::collections::BTreeMap::new();
    err_dict.put_str("category", category.as_str());
    err_dict.put_str("kind", llm_error.kind.as_str());
    err_dict.put_str("reason", llm_error.reason.as_str());
    err_dict.put_str("message", message);
    let mut dict = std::collections::BTreeMap::new();
    dict.insert("ok".to_string(), VmValue::Bool(false));
    dict.insert("response".to_string(), VmValue::Nil);
    dict.insert("error".to_string(), VmValue::dict(err_dict));
    VmValue::dict(dict)
}

/// Rewrite `(prompt, schema, options?)` — the ergonomic
/// `llm_call_structured` argument shape — into the canonical
/// `(prompt, system, options)` arg list that `extract_llm_options` and
/// `llm_call_impl` expect. Schema is installed as `output_schema`; the
/// JSON-schema-validated-output defaults (`response_format: "json"`,
/// `output_validation: "error"`, `schema_retries: 3`) are applied
/// unless the caller already set them. The caller's `system` key
/// (when present) is lifted out of the options dict into the second
/// positional slot. Built as a standalone helper so the non-bridge
/// and bridge-aware paths share one definition.
pub(crate) fn rewrite_structured_args(args: Vec<VmValue>) -> Result<Vec<VmValue>, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "llm_call_structured: missing required `schema` argument (expected \
             (prompt, schema, options?))"
                .to_string(),
        ));
    }
    let prompt = args.first().cloned().unwrap_or(VmValue::Nil);
    let schema = match args.get(1) {
        Some(VmValue::Dict(_)) => args.get(1).cloned().unwrap(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "llm_call_structured: `schema` must be a dict (JSON Schema), got {}",
                other.type_name()
            )));
        }
        None => unreachable!("len check above guarantees arg index 1"),
    };
    let mut options = args
        .get(2)
        .and_then(|a| a.as_dict())
        .cloned()
        .unwrap_or_default();

    // Pull `system` out of the options dict (ergonomic alias — the
    // canonical llm_call path takes system as the second positional
    // arg). Nil values are treated as absence so `{system: nil, ...}`
    // still lets the default apply.
    let system = options
        .remove("system")
        .filter(|v| !matches!(v, VmValue::Nil));

    // Public ergonomic alias: `retries` maps to `schema_retries`. Honor
    // the long form if the caller passes it explicitly; otherwise
    // default to 3 (enough to recover from small-model JSON drift while
    // staying cheap on frontier models that rarely miss).
    let retries_alias = options.remove("retries").and_then(|v| v.as_int());
    if let Some(n) = retries_alias {
        options
            .entry(crate::value::intern_key("schema_retries"))
            .or_insert(VmValue::Int(n));
    } else {
        options
            .entry(crate::value::intern_key("schema_retries"))
            .or_insert(VmValue::Int(3));
    }

    options
        .entry(crate::value::intern_key("output_schema"))
        .or_insert(schema.clone());
    options
        .entry(crate::value::intern_key("json_schema"))
        .or_insert(schema.clone());
    options
        .entry(crate::value::intern_key("output_format"))
        .or_insert_with(|| {
            let mut fmt = std::collections::BTreeMap::new();
            fmt.put_str("kind", "json_schema");
            fmt.insert("schema".to_string(), schema);
            fmt.insert("strict".to_string(), VmValue::Bool(true));
            VmValue::dict(fmt)
        });
    options
        .entry(crate::value::intern_key("response_format"))
        .or_insert(VmValue::String(arcstr::ArcStr::from("json")));
    options
        .entry(crate::value::intern_key("output_validation"))
        .or_insert(VmValue::String(arcstr::ArcStr::from("error")));

    Ok(vec![
        prompt,
        system.unwrap_or(VmValue::Nil),
        VmValue::dict(options),
    ])
}

/// Extract the `.data` field from a canonical `llm_call` result dict.
/// Used by `llm_call_structured` to surface just the validated payload.
pub(crate) fn extract_structured_data(response: VmValue) -> VmValue {
    match response {
        VmValue::Dict(d) => d.get("data").cloned().unwrap_or(VmValue::Nil),
        other => other,
    }
}

/// Build the `{ok: true, data, error: nil}` envelope for
/// `llm_call_structured_safe`. Mirrors `llm_safe_envelope_ok` but keys
/// the payload on `data` (the validated schema-parsed value) instead
/// of `response` (the full result dict), matching the issue shape.
pub(crate) fn structured_safe_envelope_ok(data: VmValue) -> VmValue {
    let mut dict = std::collections::BTreeMap::new();
    dict.insert("ok".to_string(), VmValue::Bool(true));
    dict.insert("data".to_string(), data);
    dict.insert("error".to_string(), VmValue::Nil);
    VmValue::dict(dict)
}

pub(crate) fn structured_safe_envelope_err(err: &VmError) -> VmValue {
    if let VmError::Thrown(VmValue::Dict(d)) = err {
        let mut dict = std::collections::BTreeMap::new();
        dict.insert("ok".to_string(), VmValue::Bool(false));
        dict.insert("data".to_string(), VmValue::Nil);
        dict.insert("error".to_string(), VmValue::Dict(d.clone()));
        return VmValue::dict(dict);
    }
    let category = crate::value::error_to_category(err);
    let message = llm_error_message(err);
    let llm_error = api::classify_llm_error(category.clone(), &message);
    let mut err_dict = std::collections::BTreeMap::new();
    err_dict.put_str("category", category.as_str());
    err_dict.put_str("kind", llm_error.kind.as_str());
    err_dict.put_str("reason", llm_error.reason.as_str());
    err_dict.put_str("message", message);
    let mut dict = std::collections::BTreeMap::new();
    dict.insert("ok".to_string(), VmValue::Bool(false));
    dict.insert("data".to_string(), VmValue::Nil);
    dict.insert("error".to_string(), VmValue::dict(err_dict));
    VmValue::dict(dict)
}

#[cfg(test)]
mod schema_stream_abort_retry_tests {
    //! Integration coverage for treating early streaming schema aborts as
    //! ordinary schema retry failures. The tests drive the retry loop through
    //! the in-process `FakeLlmProvider` so we can script:
    //!
    //! 1. an attempt that emits schema-violating tokens mid-stream
    //!    (triggers the abort, fires a `SchemaStreamAborted` event, and
    //!    consumes one retry budget slot), and
    //! 2. a follow-up attempt that emits a conforming JSON document
    //!    (the loop accepts it as the final answer).
    //!
    //! The corrective `SchemaRetry` event surfaces the abort path /
    //! reason verbatim, so callers see why the retry happened rather
    //! than a generic stream failure.

    use super::*;
    use crate::llm::fake::{
        install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn, FakeStopReason,
    };
    use crate::llm::trace::{peek_agent_trace, reset_agent_trace_state, AgentTraceEvent};

    fn options_with_retries(retries: i64) -> crate::value::DictMap {
        let mut opts = crate::value::DictMap::new();
        opts.insert(
            crate::value::intern_key("schema_retries"),
            VmValue::Int(retries),
        );
        opts
    }

    fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["age"],
            "properties": {"age": {"type": "integer"}}
        })
    }

    fn fake_opts_with_schema() -> api::LlmCallOptions {
        let mut opts = api::options::base_opts("fake");
        opts.model = "fake-stream".to_string();
        opts.output_schema = Some(schema());
        opts.output_format = api::OutputFormat::JsonSchema {
            schema: schema(),
            strict: true,
        };
        opts.json_schema = Some(schema());
        opts.response_format = Some("json".to_string());
        opts.output_validation = Some("error".to_string());
        opts.schema_stream_abort = true;
        opts.native_tools = None;
        opts.tools = None;
        opts.tool_choice = None;
        opts.provider_overrides = None;
        opts
    }

    fn fake_routing_policy() -> Arc<routing::RoutingPolicyConfig> {
        routing::clear_policy_registry();
        let chain = VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
            std::sync::Arc::new(crate::value::DictMap::from_iter([
                (
                    crate::value::intern_key("provider"),
                    VmValue::String(arcstr::ArcStr::from("fake")),
                ),
                (
                    crate::value::intern_key("model"),
                    VmValue::String(arcstr::ArcStr::from("fake-stream")),
                ),
            ])),
        )]));
        let tagged = routing::build_routing_policy(&crate::value::DictMap::from_iter([(
            crate::value::intern_key("chain"),
            chain,
        )]))
        .expect("routing policy validates");
        let options =
            crate::value::DictMap::from_iter([(crate::value::intern_key("routing"), tagged)]);
        routing::extract_routing_policy(Some(&options))
            .expect("routing policy extracts")
            .expect("routing policy present")
    }

    // Regression: a truncated structured-output response must be reported as a
    // token-limit hit regardless of provider stop_reason spelling. Gemini /
    // Vertex pass `MAX_TOKENS` (uppercase) through unnormalized; the previous
    // case-sensitive `matches!(.., "length" | "max_tokens")` missed it and
    // mislabeled the failure as "did not contain parseable JSON".
    #[test]
    fn structured_output_truncation_detected_case_insensitively() {
        let opts = api::options::base_opts("fake");
        for spelling in ["max_tokens", "MAX_TOKENS", "length", "LENGTH"] {
            let dict = VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("stop_reason"),
                VmValue::String(arcstr::ArcStr::from(spelling)),
            )]));
            let errors = structured_output_errors(&dict, &opts);
            assert!(
                errors.iter().any(|e| e.contains("hit the token limit")),
                "spelling {spelling:?} should be flagged as truncation, got: {errors:?}"
            );
        }
        // A non-truncation stop_reason must NOT add the token-limit error.
        let dict = VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("stop_reason"),
            VmValue::String(arcstr::ArcStr::from("stop")),
        )]));
        let errors = structured_output_errors(&dict, &opts);
        assert!(
            !errors.iter().any(|e| e.contains("hit the token limit")),
            "non-truncation stop must not add token-limit error, got: {errors:?}"
        );
    }

    #[test]
    fn in_flight_llm_guard_snapshots_and_clears() {
        clear_in_flight_llm_calls();
        let mut opts = fake_opts_with_schema();
        opts.messages = vec![serde_json::json!({"role": "assistant", "content": "thinking"})];

        let guard = InFlightLlmCallGuard::enter(&opts);
        let calls = snapshot_in_flight_llm_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["model"], "fake-stream");
        assert_eq!(calls[0]["role"], "assistant");
        assert!(
            calls[0]["age_ms"].as_i64().unwrap_or(-1) >= 0,
            "age must be a non-negative duration"
        );

        drop(guard);
        assert!(snapshot_in_flight_llm_calls().is_empty());
    }

    #[test]
    fn mid_stream_abort_consumes_one_retry_then_recovers() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            reset_agent_trace_state();

            // Turn 1: stream a partial doc that violates `age: int`.
            // Turn 2: stream a valid doc.
            let _script_guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token("{\"age\": ".into()),
                        FakeLlmEvent::Token("\"twenty".into()),
                        // Done isn't reached — the abort returns Err before
                        // the validator sees this chunk.
                        FakeLlmEvent::Token("\"}".into()),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ]))
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token("{\"age\": 20}".into()),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ])),
            );

            let opts = fake_opts_with_schema();
            let outcome =
                execute_schema_retry_loop(None, opts, Some(options_with_retries(2)), None)
                    .await
                    .expect("retry loop runs cleanly");

            assert_eq!(outcome.attempts, 2, "expected the recovery to run twice");
            assert!(
                outcome.errors.is_empty(),
                "final attempt must validate cleanly; got {:?}",
                outcome.errors
            );

            // The result envelope carries the validated data on the second
            // turn (post-loop, dict-shaped).
            match &outcome.vm_result {
                VmValue::Dict(d) => {
                    let data = d.get("data").cloned().unwrap_or(VmValue::Nil);
                    match data {
                        VmValue::Dict(inner) => match inner.get("age") {
                            Some(VmValue::Int(n)) => assert_eq!(*n, 20),
                            other => panic!("expected age=20; got {other:?}"),
                        },
                        other => panic!("expected validated dict; got {other:?}"),
                    }
                }
                other => panic!("expected dict result; got {other:?}"),
            }

            // Transcript events: exactly one SchemaStreamAborted, exactly one
            // SchemaRetry whose `errors` includes the abort path.
            let events = peek_agent_trace();
            let aborts: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    AgentTraceEvent::SchemaStreamAborted { path, reason, .. } => {
                        Some((path.clone(), reason.clone()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                aborts.len(),
                1,
                "expected one SchemaStreamAborted; got {events:#?}"
            );
            assert_eq!(aborts[0].0, "$.age");

            let retries: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    AgentTraceEvent::SchemaRetry { errors, .. } => Some(errors.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(retries.len(), 1, "expected one SchemaRetry event");
            assert!(
                retries[0].iter().any(|err| err.contains("$.age")),
                "retry nudge should cite the abort path; got {:?}",
                retries[0]
            );

            reset_agent_trace_state();
        });
    }

    #[test]
    fn routed_call_uses_schema_retry_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            reset_agent_trace_state();

            let _script_guard = install_fake_llm_script(
                FakeLlmScript::new()
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token("{\"age\":\"twenty\"}".into()),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ]))
                    .push(FakeLlmTurn::stream(vec![
                        FakeLlmEvent::Token("{\"age\":20}".into()),
                        FakeLlmEvent::Done(FakeStopReason::EndTurn),
                    ])),
            );

            let mut opts = fake_opts_with_schema();
            opts.routing_policy = Some(fake_routing_policy());
            let result = execute_llm_call(None, opts, Some(options_with_retries(1)), None)
                .await
                .expect("routed schema retry should recover");

            let dict = result.as_dict().expect("result dict");
            let data = dict.get("data").expect("validated data");
            let data = data.as_dict().expect("validated data dict");
            match data.get("age") {
                Some(VmValue::Int(age)) => assert_eq!(*age, 20),
                other => panic!("expected age=20, got {other:?}"),
            }
            assert!(
                dict.contains_key("routing"),
                "routed result should preserve routing diagnostics"
            );
            let retries = peek_agent_trace()
                .iter()
                .filter(|event| matches!(event, AgentTraceEvent::SchemaRetry { .. }))
                .count();
            assert_eq!(retries, 1);

            reset_agent_trace_state();
        });
    }

    #[test]
    fn opt_out_lets_invalid_stream_run_to_completion() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            reset_agent_trace_state();

            let _script_guard = install_fake_llm_script(FakeLlmScript::streaming(vec![
                FakeLlmEvent::Token("{\"age\":".into()),
                FakeLlmEvent::Token("\"twenty\"}".into()),
                FakeLlmEvent::Done(FakeStopReason::EndTurn),
            ]));

            let mut opts = fake_opts_with_schema();
            opts.schema_stream_abort = false;
            let outcome =
                execute_schema_retry_loop(None, opts, Some(options_with_retries(0)), None)
                    .await
                    .expect("retry loop completes");

            // No mid-stream abort fired; the stream ran to completion and
            // the schema validator caught the failure post-hoc instead.
            let events = peek_agent_trace();
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, AgentTraceEvent::SchemaStreamAborted { .. })),
                "abort must not fire when opted out; got {events:#?}"
            );
            assert!(
                !outcome.errors.is_empty(),
                "post-hoc validation should still flag the malformed response"
            );

            reset_agent_trace_state();
        });
    }

    // A structured retry after a token-limit truncation must grow the
    // output-token budget so a reasoning model (whose analysis channel is
    // billed against the same budget but invisible in parsed text) gets room
    // to emit complete JSON instead of re-truncating to empty.
    #[test]
    fn truncation_retry_escalates_max_tokens() {
        let mut opts = api::options::base_opts("fake");
        opts.max_tokens = 640;
        let errors =
            vec!["response hit the token limit before producing complete JSON".to_string()];
        let grew = escalate_max_tokens_on_truncation(&mut opts, &errors);
        assert!(grew, "truncation marker should escalate the budget");
        assert_eq!(opts.max_tokens, 1280, "640 should double to 1280");
    }

    // A non-truncation failure (e.g. a schema-validation miss) must NOT touch
    // the budget — escalation is reserved for the under-budget root cause.
    #[test]
    fn non_truncation_failure_leaves_max_tokens_unchanged() {
        let mut opts = api::options::base_opts("fake");
        opts.max_tokens = 640;
        let errors = vec!["data.age: expected integer, got string".to_string()];
        let grew = escalate_max_tokens_on_truncation(&mut opts, &errors);
        assert!(!grew, "non-truncation failure must not escalate");
        assert_eq!(opts.max_tokens, 640);
    }

    // The escalation is clamped at the retry ceiling so a pathological
    // never-converging loop can't request an unbounded completion.
    #[test]
    fn truncation_retry_clamps_at_ceiling() {
        let mut opts = api::options::base_opts("fake");
        opts.max_tokens = MAX_TOKENS_RETRY_CEILING - 100;
        let errors =
            vec!["response hit the token limit before producing complete JSON".to_string()];
        let grew = escalate_max_tokens_on_truncation(&mut opts, &errors);
        assert!(grew, "below the ceiling, the budget should still grow");
        assert_eq!(opts.max_tokens, MAX_TOKENS_RETRY_CEILING);

        // Already at the ceiling: no further growth, no wasted retry signal.
        let grew_again = escalate_max_tokens_on_truncation(&mut opts, &errors);
        assert!(
            !grew_again,
            "at the ceiling the budget must not grow further"
        );
        assert_eq!(opts.max_tokens, MAX_TOKENS_RETRY_CEILING);
    }
}
