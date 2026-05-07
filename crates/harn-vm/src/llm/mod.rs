//! LLM integration: API calls, streaming, agent loops, tool handling, and tracing.
//!
//! This module is split into sub-modules for maintainability:
//! - `api`: Core LLM API call logic (request building, response parsing)
//! - `agent`: Agent loop implementations (basic and bridge-backed)
//! - `stream`: SSE streaming support
//! - `tools`: Tool schema resolution, text-based tool calling, argument normalization
//! - `mock`: Mock provider and fixture record/replay
//! - `trace`: LLM call tracing (thread-local trace log)
//! - `helpers`: Option extraction, provider/model/key resolution, JSON conversion
//! - `conversation`: Conversation management builtins
//! - `config_builtins`: Provider configuration query builtins

mod agent_config;
mod agent_observe;
mod agent_runtime;
mod agent_session_host;
mod agent_tools;
pub(crate) mod api;
pub(crate) mod autonomy_budget;
mod cache;
pub mod capabilities;
mod config_builtins;
pub(crate) mod content;
mod conversation;
pub(crate) mod cost;
pub(crate) mod cost_route;
pub(crate) mod daemon;
pub(crate) mod fake;
pub(crate) mod helpers;
pub(crate) mod mock;
pub(crate) mod permissions;
pub mod plan;
pub mod readiness;
mod rerank;
pub(crate) mod schema_recover;
pub(crate) mod skill_score;
pub(crate) mod structural_experiments;
pub(crate) mod structured_envelope;
mod tool_search_score;
mod transcript_stats;

use std::sync::OnceLock;

/// Streaming client: no overall request timeout (per-chunk idle timeout
/// handles stalls), connection pooling and TLS session reuse.
pub(crate) fn shared_streaming_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        client_builder_for_tests(
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .pool_max_idle_per_host(4),
        )
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Non-streaming client: 120s request timeout, connection pooling.
pub(crate) fn shared_blocking_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        client_builder_for_tests(
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .timeout(std::time::Duration::from_secs(120))
                .pool_max_idle_per_host(4),
        )
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Utility client for short-lived requests (healthchecks, context window
/// lookups). Shorter timeouts than the blocking client, shared connection pool.
pub(crate) fn shared_utility_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        client_builder_for_tests(
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(15))
                .pool_max_idle_per_host(2),
        )
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
    })
}

#[cfg(test)]
fn client_builder_for_tests(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    builder.danger_accept_invalid_certs(true)
}

#[cfg(not(test))]
fn client_builder_for_tests(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    builder
}

pub use api::{
    ollama_runtime_settings_from_env, warm_ollama_model, warm_ollama_model_with_settings,
    OllamaRuntimeSettings, HARN_OLLAMA_KEEP_ALIVE_ENV, HARN_OLLAMA_NUM_CTX_ENV,
    OLLAMA_DEFAULT_KEEP_ALIVE, OLLAMA_DEFAULT_NUM_CTX, OLLAMA_HOST_ENV,
};
pub use fake::{
    fake_llm_captured_calls, install_fake_llm_script, FakeLlmCall, FakeLlmError, FakeLlmEvent,
    FakeLlmGuard, FakeLlmScript, FakeLlmTurn, FakeStopReason,
};
pub use mock::drain_tool_recordings;
mod healthcheck;
pub(crate) mod provider;
pub(crate) mod providers;
pub(crate) mod rate_limit;
mod stream;
pub(crate) mod tools;
mod trace;
pub(crate) mod trigger_predicate;

/// Shared process-wide lock for tests that mutate LLM-related environment
/// variables (LOCAL_LLM_BASE_URL, LOCAL_LLM_MODEL, HARN_LLM_*). Any test that
/// sets or removes one of these MUST hold this lock for its whole duration,
/// including through any async LLM call, so concurrent tests from sibling
/// modules cannot clobber each other's env and leak stale values into a
/// streaming request. Previously each submodule had its own `env_lock()` and
/// races between `llm::helpers::tests` and `llm::api::tests` flaked the
/// streaming classification tests under parallel cargo execution.
#[cfg(test)]
pub(crate) fn env_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub const LLM_CALLS_DISABLED_ENV: &str = "HARN_LLM_CALLS_DISABLED";

pub(crate) fn llm_calls_disabled() -> bool {
    std::env::var(LLM_CALLS_DISABLED_ENV)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

pub(crate) fn ensure_real_llm_allowed(provider: &str) -> Result<(), crate::value::VmError> {
    if !llm_calls_disabled() || provider == "mock" || provider == "fake" {
        return Ok(());
    }
    Err(crate::value::VmError::Runtime(format!(
        "LLM calls are disabled by {LLM_CALLS_DISABLED_ENV}; provider `{provider}` would make a real LLM request"
    )))
}

use std::rc::Rc;
use std::sync::Arc;

use crate::stdlib::registration::{
    async_builtin, register_builtin_group, register_builtin_groups, AsyncBuiltin, BuiltinGroup,
    SyncBuiltin,
};
use crate::stdlib::{json_to_vm_value, schema_result_value};
use crate::value::{VmChannelHandle, VmError, VmStream, VmStreamCancel, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

use self::api::{vm_build_llm_result, vm_call_completion_full};
use self::helpers::{opt_int, opt_str};
use self::stream::vm_stream_llm;
use self::trace::emit_agent_event;
use self::trace::trace_llm_call;

pub use self::api::{
    normalize_ollama_keep_alive, ollama_readiness, OllamaReadinessOptions, OllamaReadinessResult,
    OllamaWarmupResult,
};

#[cfg(feature = "llm-bench-internals")]
#[doc(hidden)]
pub mod bench_internals {
    use super::*;

    pub fn llm_options_roundtrip_probe(
        args: &[VmValue],
        options: &Option<std::collections::BTreeMap<String, VmValue>>,
    ) -> Result<usize, VmError> {
        let resolved_provider = helpers::vm_resolve_provider(options);
        let extracted = extract_llm_options(args)?;
        let pricing_per_1k = cost::pricing_per_1k_for(&extracted.provider, &extracted.model);
        Ok(resolved_provider.len()
            + extracted.provider.len()
            + extracted.model.len()
            + usize::from(pricing_per_1k.is_some()))
    }
}

pub fn install_current_host_bridge(bridge: Rc<crate::bridge::HostBridge>) {
    agent_runtime::install_current_host_bridge(bridge);
}

pub fn clear_current_host_bridge() {
    agent_runtime::clear_current_host_bridge();
}

pub(crate) fn append_observability_sidecar_entry(
    event_type: &str,
    fields: serde_json::Map<String, serde_json::Value>,
) {
    agent_observe::append_llm_observability_entry(event_type, fields);
}

fn output_validation_mode(opts: &api::LlmCallOptions) -> &str {
    opts.output_validation.as_deref().unwrap_or("off")
}

fn schema_validation_errors(result: &VmValue) -> Vec<String> {
    match result {
        VmValue::EnumVariant {
            enum_name,
            variant,
            fields,
        } if enum_name.as_ref() == "Result" && variant.as_ref() == "Err" => fields
            .first()
            .and_then(|payload| payload.as_dict())
            .and_then(|payload| payload.get("errors"))
            .and_then(|errors| match errors {
                VmValue::List(items) => Some(items.iter().map(|err| err.display()).collect()),
                _ => None,
            })
            .unwrap_or_else(|| vec!["schema validation failed".to_string()]),
        _ => Vec::new(),
    }
}

/// Compute schema validation errors against `opts.output_schema` without
/// deciding disposition (warn vs error vs off). Returns an empty vec when
/// no schema is configured or the data validates. Used by the schema-retry
/// loop in `llm_call`.
fn compute_validation_errors(data: &VmValue, opts: &api::LlmCallOptions) -> Vec<String> {
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
        if matches!(stop_reason.as_str(), "length" | "max_tokens") {
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

pub(crate) fn parse_schema_nudge(
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
) -> SchemaNudge {
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

const SCHEMA_NUDGE_MAX_DEPTH: usize = 3;
const SCHEMA_NUDGE_MAX_LINES: usize = 8;
const SCHEMA_NUDGE_MAX_KEYS: usize = 16;

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

pub(crate) use self::agent_config::agent_loop_result_from_llm;
pub use self::agent_config::{
    register_agent_loop_with_bridge, register_llm_call_structured_with_bridge,
    register_llm_call_with_bridge,
};
pub use self::agent_runtime::{
    current_agent_session_id, drain_global_pending_feedback, push_pending_feedback_global,
    register_session_end_hook, wait_for_global_pending_feedback,
};
pub(crate) use self::agent_runtime::{
    current_host_bridge, emit_agent_event as emit_live_agent_event,
};
pub(crate) use self::api::vm_call_llm_full;
pub use self::api::{
    fetch_provider_max_context, probe_openai_compatible_model, selected_model_for_provider,
    supports_model_readiness_probe, ModelReadiness,
};
pub use self::cost::{calculate_cost_for_provider, peek_total_cost};
pub use self::healthcheck::{
    build_healthcheck_url, run_provider_healthcheck, run_provider_healthcheck_with_options,
    ProviderHealthcheckOptions, ProviderHealthcheckResult,
};
pub(crate) use self::helpers::extract_llm_options;
pub use self::helpers::resolve_api_key;
pub use self::helpers::vm_value_to_json;
pub use self::mock::{
    clear_cli_llm_mock_mode, enable_cli_llm_mock_recording, install_cli_llm_mocks, set_replay_mode,
    take_cli_llm_recordings, LlmMock, LlmReplayMode, MockError,
};
pub use self::trace::{
    agent_trace_summary, enable_tracing, peek_agent_trace, peek_trace, peek_trace_summary,
    take_agent_trace, take_trace, AgentTraceEvent, LlmTraceEntry,
};

/// Reset all thread-local LLM state (cost, trace, mock, rate limits). Call between test runs.
pub fn reset_llm_state() {
    cost::reset_cost_state();
    trace::reset_trace_state();
    trace::reset_agent_trace_state();
    provider::register_default_providers();
    rate_limit::reset_rate_limit_state();
    mock::reset_llm_mock_state();
    autonomy_budget::reset_autonomy_budget_state();
    agent_session_host::reset_agent_session_host_state();
    permissions::clear_dynamic_permission_state();
    trigger_predicate::reset_trigger_predicate_state();
    capabilities::clear_user_overrides();
    // Per-`@step` registry, active stack, and completed-step log are
    // thread-local; clear them between runs to prevent stale step
    // metadata from one program leaking into the next.
    crate::step_runtime::reset_thread_local_state();
}

/// Shared implementation of `llm_call` / `llm_call_safe`. Runs the
/// full schema-retry loop; on success returns the LLM result dict, on
/// failure returns the underlying `VmError`. `llm_call` propagates the
/// error (re-wrapped as a thrown `{kind, reason, category, message,
/// retry_after_ms?, provider, model}` dict so catch blocks can dispatch
/// on the LLM error taxonomy);
/// `llm_call_safe` wraps it in a `{ok: false, error: …}` envelope with
/// the same fields.
async fn llm_call_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let options = args.get(2).and_then(|a| a.as_dict()).cloned();
    let opts = extract_llm_options(&args)?;
    let provider = opts.provider.clone();
    let model = opts.model.clone();
    match execute_llm_call(opts, options, None).await {
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
        dict.entry("category".to_string())
            .or_insert_with(|| VmValue::String(Rc::from(category.as_str())));
        dict.entry("kind".to_string())
            .or_insert_with(|| VmValue::String(Rc::from(llm_error.kind.as_str())));
        dict.entry("reason".to_string())
            .or_insert_with(|| VmValue::String(Rc::from(llm_error.reason.as_str())));
        dict.entry("message".to_string())
            .or_insert_with(|| VmValue::String(Rc::from(message.as_str())));
        dict.insert("provider".to_string(), VmValue::String(Rc::from(provider)));
        dict.insert("model".to_string(), VmValue::String(Rc::from(model)));
        return VmValue::Dict(Rc::new(dict));
    }
    let mut dict = std::collections::BTreeMap::new();
    dict.insert(
        "category".to_string(),
        VmValue::String(Rc::from(category.as_str())),
    );
    dict.insert(
        "kind".to_string(),
        VmValue::String(Rc::from(llm_error.kind.as_str())),
    );
    dict.insert(
        "reason".to_string(),
        VmValue::String(Rc::from(llm_error.reason.as_str())),
    );
    dict.insert("message".to_string(), VmValue::String(Rc::from(message)));
    if let Some(ms) = agent_observe::extract_retry_after_ms(err) {
        dict.insert("retry_after_ms".to_string(), VmValue::Int(ms as i64));
    }
    dict.insert("provider".to_string(), VmValue::String(Rc::from(provider)));
    dict.insert("model".to_string(), VmValue::String(Rc::from(model)));
    VmValue::Dict(Rc::new(dict))
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
    opts: api::LlmCallOptions,
    options: Option<std::collections::BTreeMap<String, VmValue>>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
) -> Result<VmValue, VmError> {
    let outcome = execute_schema_retry_loop(opts, options, bridge).await?;
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
    mut opts: api::LlmCallOptions,
    options: Option<std::collections::BTreeMap<String, VmValue>>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
) -> Result<SchemaLoopOutcome, VmError> {
    let _ = structural_experiments::apply_structural_experiment(&mut opts, None).await?;
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
    // conversation. See issue #533.
    let original_messages = opts.messages.clone();
    for attempt in 0..=schema_retries {
        let result = agent_observe::observed_llm_call(
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
        .await?;

        let raw_text = result.text.clone();
        // Non-bridge path runs schema validation; bridge path
        // delegates validation to the host.
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

fn llm_safe_envelope_ok(response: VmValue) -> VmValue {
    let mut dict = std::collections::BTreeMap::new();
    dict.insert("ok".to_string(), VmValue::Bool(true));
    dict.insert("response".to_string(), response);
    dict.insert("error".to_string(), VmValue::Nil);
    VmValue::Dict(Rc::new(dict))
}

fn llm_safe_envelope_err(err: &VmError) -> VmValue {
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
        return VmValue::Dict(Rc::new(dict));
    }
    let category = crate::value::error_to_category(err);
    let message = llm_error_message(err);
    let llm_error = api::classify_llm_error(category.clone(), &message);
    let mut err_dict = std::collections::BTreeMap::new();
    err_dict.insert(
        "category".to_string(),
        VmValue::String(Rc::from(category.as_str())),
    );
    err_dict.insert(
        "kind".to_string(),
        VmValue::String(Rc::from(llm_error.kind.as_str())),
    );
    err_dict.insert(
        "reason".to_string(),
        VmValue::String(Rc::from(llm_error.reason.as_str())),
    );
    err_dict.insert("message".to_string(), VmValue::String(Rc::from(message)));
    let mut dict = std::collections::BTreeMap::new();
    dict.insert("ok".to_string(), VmValue::Bool(false));
    dict.insert("response".to_string(), VmValue::Nil);
    dict.insert("error".to_string(), VmValue::Dict(Rc::new(err_dict)));
    VmValue::Dict(Rc::new(dict))
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

    // Public ergonomic alias: `retries` in the issue's proposed shape
    // maps to `schema_retries`. Honor the long form if the caller
    // passes it explicitly; otherwise default to 3 (enough to recover
    // from small-model JSON drift while staying cheap on frontier
    // models that rarely miss).
    let retries_alias = options.remove("retries").and_then(|v| v.as_int());
    if let Some(n) = retries_alias {
        options
            .entry("schema_retries".to_string())
            .or_insert(VmValue::Int(n));
    } else {
        options
            .entry("schema_retries".to_string())
            .or_insert(VmValue::Int(3));
    }

    options
        .entry("output_schema".to_string())
        .or_insert(schema.clone());
    options
        .entry("json_schema".to_string())
        .or_insert(schema.clone());
    options
        .entry("output_format".to_string())
        .or_insert_with(|| {
            let mut fmt = std::collections::BTreeMap::new();
            fmt.insert("kind".to_string(), VmValue::String(Rc::from("json_schema")));
            fmt.insert("schema".to_string(), schema);
            fmt.insert("strict".to_string(), VmValue::Bool(true));
            VmValue::Dict(Rc::new(fmt))
        });
    options
        .entry("response_format".to_string())
        .or_insert(VmValue::String(Rc::from("json")));
    options
        .entry("output_validation".to_string())
        .or_insert(VmValue::String(Rc::from("error")));

    Ok(vec![
        prompt,
        system.unwrap_or(VmValue::Nil),
        VmValue::Dict(Rc::new(options)),
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
    VmValue::Dict(Rc::new(dict))
}

pub(crate) fn structured_safe_envelope_err(err: &VmError) -> VmValue {
    if let VmError::Thrown(VmValue::Dict(d)) = err {
        let mut dict = std::collections::BTreeMap::new();
        dict.insert("ok".to_string(), VmValue::Bool(false));
        dict.insert("data".to_string(), VmValue::Nil);
        dict.insert("error".to_string(), VmValue::Dict(d.clone()));
        return VmValue::Dict(Rc::new(dict));
    }
    let category = crate::value::error_to_category(err);
    let message = llm_error_message(err);
    let llm_error = api::classify_llm_error(category.clone(), &message);
    let mut err_dict = std::collections::BTreeMap::new();
    err_dict.insert(
        "category".to_string(),
        VmValue::String(Rc::from(category.as_str())),
    );
    err_dict.insert(
        "kind".to_string(),
        VmValue::String(Rc::from(llm_error.kind.as_str())),
    );
    err_dict.insert(
        "reason".to_string(),
        VmValue::String(Rc::from(llm_error.reason.as_str())),
    );
    err_dict.insert("message".to_string(), VmValue::String(Rc::from(message)));
    let mut dict = std::collections::BTreeMap::new();
    dict.insert("ok".to_string(), VmValue::Bool(false));
    dict.insert("data".to_string(), VmValue::Nil);
    dict.insert("error".to_string(), VmValue::Dict(Rc::new(err_dict)));
    VmValue::Dict(Rc::new(dict))
}

#[derive(Clone)]
struct CapturingAgentEventSink {
    session_id: String,
    events: Arc<std::sync::Mutex<Vec<crate::agent_events::AgentEvent>>>,
}

impl crate::agent_events::AgentEventSink for CapturingAgentEventSink {
    fn handle_event(&self, event: &crate::agent_events::AgentEvent) {
        if event.session_id() != self.session_id {
            return;
        }
        if let Ok(mut events) = self.events.lock() {
            events.push(event.clone());
        }
    }
}

async fn host_agent_capture_events_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(text)) if !text.is_empty() => text.to_string(),
        Some(VmValue::String(_)) => {
            return Err(VmError::Runtime(
                "__host_agent_capture_events(session_id, body): session_id must be non-empty"
                    .to_string(),
            ))
        }
        Some(other) => {
            let type_name = other.type_name();
            return Err(VmError::Runtime(format!(
                "__host_agent_capture_events(session_id, body): session_id must be a string; got {type_name}"
            )));
        }
        None => {
            return Err(VmError::Runtime(
                "__host_agent_capture_events(session_id, body): missing session_id".to_string(),
            ))
        }
    };
    let body = match args.get(1) {
        Some(VmValue::Closure(closure)) => closure.clone(),
        _ => {
            return Err(VmError::Runtime(
                "__host_agent_capture_events(session_id, body): body must be a closure".to_string(),
            ))
        }
    };

    let captured_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink: Arc<dyn crate::agent_events::AgentEventSink> = Arc::new(CapturingAgentEventSink {
        session_id,
        events: captured_events.clone(),
    });
    let _guard = agent_runtime::LoopSinkGuard::install(Some(sink));
    let mut child_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime(
            "__host_agent_capture_events requires an async builtin VM context".to_string(),
        )
    })?;
    let result = child_vm.call_closure_pub(&body, &[]).await;
    let output = child_vm.take_output();
    crate::vm::forward_child_output_to_parent(&output);
    let result = result?;
    let events = captured_events
        .lock()
        .map(|events| {
            events
                .iter()
                .map(|event| serde_json::to_value(event).unwrap_or(serde_json::Value::Null))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut envelope = std::collections::BTreeMap::new();
    envelope.insert("result".to_string(), result);
    envelope.insert(
        "events".to_string(),
        json_to_vm_value(&serde_json::Value::Array(events)),
    );
    Ok(VmValue::Dict(Rc::new(envelope)))
}

fn agent_primitive_tools_arg(
    args: &[VmValue],
    index: usize,
    label: &str,
) -> Result<Option<VmValue>, VmError> {
    match args.get(index) {
        Some(VmValue::Nil) | None => Ok(crate::stdlib::tools::current_tool_registry()),
        Some(VmValue::Dict(_)) => Ok(args.get(index).cloned()),
        Some(other) => Err(VmError::Runtime(format!(
            "{label}: tools must be a tool registry dict or nil; got {}",
            other.type_name()
        ))),
    }
}

fn agent_primitive_options_arg(
    args: &[VmValue],
    index: usize,
    label: &str,
) -> Result<std::collections::BTreeMap<String, VmValue>, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(options)) => Ok(options.as_ref().clone()),
        Some(VmValue::Nil) | None => Ok(std::collections::BTreeMap::new()),
        Some(other) => Err(VmError::Runtime(format!(
            "{label}: options must be a dict or nil; got {}",
            other.type_name()
        ))),
    }
}

fn agent_primitive_call_json(args: &[VmValue]) -> Result<serde_json::Value, VmError> {
    match args.first() {
        Some(VmValue::Dict(_)) => Ok(helpers::vm_value_to_json(args.first().unwrap())),
        Some(other) => Err(VmError::Runtime(format!(
            "__host_agent_dispatch_tool_call(call, tools?, options?): call must be a dict; got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(
            "__host_agent_dispatch_tool_call(call, tools?, options?): missing call".to_string(),
        )),
    }
}

/// Append a `PermissionGrant` / `PermissionDeny` / `PermissionEscalation`
/// event to the live transcript for the named session, when one exists.
/// Silent no-op for sessions that haven't been opened (e.g. raw
/// dispatcher calls outside an agent loop).
fn emit_permission_event(
    session_id: &str,
    kind: &str,
    tool_name: &str,
    tool_args: &serde_json::Value,
    reason: &str,
    escalated: bool,
) {
    if !crate::agent_sessions::exists(session_id) {
        return;
    }
    let event =
        permissions::permission_transcript_event(kind, tool_name, tool_args, reason, escalated);
    let _ = crate::agent_sessions::append_event(session_id, event);
}

fn agent_primitive_denied_tool(
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    reason: impl Into<String>,
    category: crate::agent_events::ToolCallErrorCategory,
) -> serde_json::Value {
    let reason = reason.into();
    let result = agent_tools::denied_tool_result(tool_name, reason.clone());
    serde_json::json!({
        "ok": false,
        "status": "error",
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "arguments": tool_args,
        "result": result,
        "rendered_result": agent_tools::render_tool_result(&result),
        "error": reason,
        "error_category": category.as_str(),
        "executor": null,
    })
}

fn agent_primitive_tool_name(call: &serde_json::Value) -> Result<String, VmError> {
    call.get("name")
        .and_then(|value| value.as_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            VmError::Runtime(
                "__host_agent_dispatch_tool_call: call.name must be a non-empty string".to_string(),
            )
        })
}

fn agent_primitive_call_name(call: &VmValue) -> Option<String> {
    let dict = call.as_dict()?;
    let name = dict.get("name")?.display();
    (!name.trim().is_empty()).then_some(name)
}

fn agent_primitive_call_is_read_only(call: &VmValue) -> bool {
    agent_primitive_call_name(call)
        .as_deref()
        .and_then(crate::orchestration::current_tool_annotations)
        .map(|annotations| annotations.kind.is_read_only())
        .unwrap_or(false)
}

async fn host_agent_parse_tool_calls_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let text = match args.first() {
        Some(VmValue::String(text)) => text.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_agent_parse_tool_calls(text, tools?): text must be a string; got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::Runtime(
                "__host_agent_parse_tool_calls(text, tools?): missing text".to_string(),
            ))
        }
    };
    let tools = agent_primitive_tools_arg(&args, 1, "__host_agent_parse_tool_calls")?;
    let parsed = tools::parse_text_tool_calls_with_tools(&text, tools.as_ref());
    Ok(json_to_vm_value(&serde_json::json!({
        "calls": parsed.calls,
        "tool_calls": parsed.calls,
        "tool_parse_errors": parsed.errors,
        "protocol_violations": parsed.violations,
        "prose": parsed.prose,
        "user_response": parsed.user_response,
        "done_marker": parsed.done_marker,
        "canonical_text": parsed.canonical,
    })))
}

async fn host_agent_dispatch_tool_batch_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let calls = match args.first() {
        Some(VmValue::List(calls)) => calls.as_ref().clone(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_agent_dispatch_tool_batch(calls, tools?, options?): calls must be a list; got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::Runtime(
                "__host_agent_dispatch_tool_batch(calls, tools?, options?): missing calls"
                    .to_string(),
            ))
        }
    };
    let tools = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let options = VmValue::Dict(Rc::new(agent_primitive_options_arg(
        &args,
        2,
        "__host_agent_dispatch_tool_batch",
    )?));
    let ro_prefix_len = calls
        .iter()
        .position(|call| !agent_primitive_call_is_read_only(call))
        .unwrap_or(calls.len());

    let mut results: Vec<Option<VmValue>> = vec![None; calls.len()];
    if ro_prefix_len >= 2 {
        let futures = calls[..ro_prefix_len].iter().cloned().map(|call| {
            host_agent_dispatch_tool_call_impl(vec![call, tools.clone(), options.clone()])
        });
        for (index, result) in futures::future::join_all(futures)
            .await
            .into_iter()
            .enumerate()
        {
            results[index] = Some(result?);
        }
    }

    for (index, call) in calls.into_iter().enumerate() {
        if results[index].is_some() {
            continue;
        }
        results[index] = Some(
            host_agent_dispatch_tool_call_impl(vec![call, tools.clone(), options.clone()]).await?,
        );
    }

    Ok(VmValue::List(Rc::new(
        results
            .into_iter()
            .map(|value| value.unwrap_or(VmValue::Nil))
            .collect(),
    )))
}

async fn host_agent_dispatch_tool_call_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let call = agent_primitive_call_json(&args)?;
    let tools = agent_primitive_tools_arg(&args, 1, "__host_agent_dispatch_tool_call")?;
    let options = agent_primitive_options_arg(&args, 2, "__host_agent_dispatch_tool_call")?;
    let options_ref = Some(options.clone());
    let tool_name = agent_primitive_tool_name(&call)?;
    let tool_id = call
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let mut tool_args = tools::normalize_tool_args(&tool_name, &call["arguments"]);
    let session_id = opt_str(&options_ref, "session_id")
        .or_else(current_agent_session_id)
        .unwrap_or_else(|| format!("agent_primitive_session_{}", uuid::Uuid::now_v7()));
    let tool_retries = opt_int(&options_ref, "tool_retries").unwrap_or(0).max(0) as usize;
    let tool_backoff_ms = opt_int(&options_ref, "tool_backoff_ms")
        .unwrap_or(1000)
        .max(1) as u64;
    let bridge = current_host_bridge();
    let _policy_guard = agent_session_host::install_session_policy_guard(&options)?;

    if let Err(error) =
        crate::orchestration::enforce_current_policy_for_tool(&tool_name).and_then(|_| {
            crate::orchestration::enforce_tool_arg_constraints(
                &crate::orchestration::current_execution_policy().unwrap_or_default(),
                &tool_name,
                &tool_args,
            )
        })
    {
        let reason = error.to_string();
        emit_permission_event(
            &session_id,
            "PermissionDeny",
            &tool_name,
            &tool_args,
            &reason,
            false,
        );
        return Ok(json_to_vm_value(&agent_primitive_denied_tool(
            &tool_name,
            &tool_id,
            &tool_args,
            reason,
            crate::agent_events::ToolCallErrorCategory::PermissionDenied,
        )));
    }

    let mut permission_grants = permissions::take_session_grants(&session_id);
    let permission_outcome = permissions::check_dynamic_permission(
        &mut permission_grants,
        &tool_name,
        &tool_args,
        &session_id,
    )
    .await?;
    permissions::store_session_grants(&session_id, permission_grants);
    if let Some(permission) = permission_outcome {
        match permission {
            permissions::PermissionCheck::Granted { reason, escalated } => {
                if escalated {
                    emit_permission_event(
                        &session_id,
                        "PermissionEscalation",
                        &tool_name,
                        &tool_args,
                        &reason,
                        true,
                    );
                }
                emit_permission_event(
                    &session_id,
                    "PermissionGrant",
                    &tool_name,
                    &tool_args,
                    &reason,
                    escalated,
                );
            }
            permissions::PermissionCheck::Denied { reason, escalated } => {
                if escalated {
                    emit_permission_event(
                        &session_id,
                        "PermissionEscalation",
                        &tool_name,
                        &tool_args,
                        &reason,
                        true,
                    );
                }
                emit_permission_event(
                    &session_id,
                    "PermissionDeny",
                    &tool_name,
                    &tool_args,
                    &reason,
                    escalated,
                );
                return Ok(json_to_vm_value(&agent_primitive_denied_tool(
                    &tool_name,
                    &tool_id,
                    &tool_args,
                    reason,
                    crate::agent_events::ToolCallErrorCategory::PermissionDenied,
                )));
            }
        }
    }

    let approval = crate::orchestration::current_approval_policy()
        .map(|policy| policy.evaluate(&tool_name, &tool_args));
    let mut approval_status = None;
    match approval {
        None | Some(crate::orchestration::ToolApprovalDecision::AutoApproved) => {}
        Some(crate::orchestration::ToolApprovalDecision::AutoDenied { reason }) => {
            emit_permission_event(
                &session_id,
                "PermissionDeny",
                &tool_name,
                &tool_args,
                &reason,
                false,
            );
            return Ok(json_to_vm_value(&agent_primitive_denied_tool(
                &tool_name,
                &tool_id,
                &tool_args,
                reason,
                crate::agent_events::ToolCallErrorCategory::PermissionDenied,
            )));
        }
        Some(crate::orchestration::ToolApprovalDecision::RequiresHostApproval) => {
            let Some(bridge) = bridge.as_ref() else {
                let reason = "approval required but no host bridge is available";
                emit_permission_event(
                    &session_id,
                    "PermissionDeny",
                    &tool_name,
                    &tool_args,
                    reason,
                    false,
                );
                return Ok(json_to_vm_value(&agent_primitive_denied_tool(
                    &tool_name,
                    &tool_id,
                    &tool_args,
                    reason,
                    crate::agent_events::ToolCallErrorCategory::PermissionDenied,
                )));
            };
            let approval_id = if tool_id.is_empty() {
                format!("tool_call_{}", uuid::Uuid::now_v7())
            } else {
                tool_id.clone()
            };
            let approval_request = crate::stdlib::hitl::approval_request_for_host_permission(
                approval_id.clone(),
                tool_name.clone(),
                tool_args.clone(),
                session_id.clone(),
                Vec::new(),
                serde_json::Value::Null,
                vec![format!("tool.{tool_name}")],
            );
            let response = bridge
                .call(
                    "session/request_permission",
                    serde_json::json!({
                        "sessionId": session_id,
                        "approvalRequest": approval_request,
                        "toolCall": {
                            "toolCallId": approval_id,
                            "toolName": tool_name,
                            "rawInput": tool_args,
                        },
                    }),
                )
                .await;
            match response {
                Ok(response) => {
                    let outcome = response
                        .get("outcome")
                        .and_then(|value| value.get("outcome"))
                        .and_then(|value| value.as_str())
                        .or_else(|| response.get("outcome").and_then(|value| value.as_str()))
                        .unwrap_or("");
                    let granted = matches!(outcome, "selected" | "allow")
                        || response
                            .get("granted")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false);
                    if granted {
                        if let Some(new_args) = response.get("args") {
                            tool_args = new_args.clone();
                        }
                        approval_status = Some("host_granted");
                        emit_permission_event(
                            &session_id,
                            "PermissionGrant",
                            &tool_name,
                            &tool_args,
                            "host approved tool call",
                            true,
                        );
                    } else {
                        let reason = response
                            .get("reason")
                            .and_then(|value| value.as_str())
                            .unwrap_or("host did not grant approval");
                        emit_permission_event(
                            &session_id,
                            "PermissionDeny",
                            &tool_name,
                            &tool_args,
                            reason,
                            true,
                        );
                        return Ok(json_to_vm_value(&agent_primitive_denied_tool(
                            &tool_name,
                            &tool_id,
                            &tool_args,
                            reason,
                            crate::agent_events::ToolCallErrorCategory::PermissionDenied,
                        )));
                    }
                }
                Err(_) => {
                    let reason =
                        "approval request failed or host does not implement session/request_permission";
                    emit_permission_event(
                        &session_id,
                        "PermissionDeny",
                        &tool_name,
                        &tool_args,
                        reason,
                        true,
                    );
                    return Ok(json_to_vm_value(&agent_primitive_denied_tool(
                        &tool_name,
                        &tool_id,
                        &tool_args,
                        reason,
                        crate::agent_events::ToolCallErrorCategory::PermissionDenied,
                    )));
                }
            }
        }
    }

    match crate::orchestration::run_pre_tool_hooks(&tool_name, &tool_args).await? {
        crate::orchestration::PreToolAction::Allow => {}
        crate::orchestration::PreToolAction::Deny(reason) => {
            return Ok(json_to_vm_value(&agent_primitive_denied_tool(
                &tool_name,
                &tool_id,
                &tool_args,
                reason,
                crate::agent_events::ToolCallErrorCategory::PermissionDenied,
            )));
        }
        crate::orchestration::PreToolAction::Modify(new_args) => {
            tool_args = new_args;
        }
    }

    let tool_schemas = tools::collect_tool_schemas(tools.as_ref(), None);
    if let Err(message) = tools::validate_tool_args(&tool_name, &tool_args, &tool_schemas) {
        return Ok(json_to_vm_value(&agent_primitive_denied_tool(
            &tool_name,
            &tool_id,
            &tool_args,
            message,
            crate::agent_events::ToolCallErrorCategory::SchemaValidation,
        )));
    }

    let started = std::time::Instant::now();
    // Session-scoped MCP clients (from opts.mcp_servers) bypass the bridge.
    let session_mcp = {
        use std::collections::BTreeMap;
        let mut clients: BTreeMap<String, crate::mcp::VmMcpClientHandle> = BTreeMap::new();
        if let Some(server_name) = agent_tools::mcp_server_for_tool(tools.as_ref(), &tool_name) {
            if let Some(handle) = agent_runtime::session_mcp_client(&session_id, &server_name) {
                clients.insert(server_name, handle);
            }
        }
        clients
    };
    let mcp_clients_ref = if session_mcp.is_empty() {
        None
    } else {
        Some(&session_mcp)
    };
    let outcome = agent_tools::dispatch_tool_execution_with_mcp(
        &tool_name,
        &tool_args,
        tools.as_ref(),
        mcp_clients_ref,
        bridge.as_ref(),
        tool_retries,
        tool_backoff_ms,
    )
    .await;
    let execution_duration_ms = started.elapsed().as_millis() as u64;
    let executor = outcome
        .executor
        .as_ref()
        .and_then(|executor| serde_json::to_value(executor).ok());

    match outcome.result {
        Ok(raw_result) => {
            let rendered = agent_tools::render_tool_result(&raw_result);
            let rendered =
                crate::orchestration::run_post_tool_hooks(&tool_name, &tool_args, &rendered)
                    .await?;
            let denied = agent_tools::is_denied_tool_result(&raw_result);
            let observation = format!(
                "[result of {name}]\n{result}\n[end of {name} result]\n",
                name = tool_name,
                result = rendered
            );
            let error = denied.then(|| rendered.clone());
            Ok(json_to_vm_value(&serde_json::json!({
                "ok": !denied,
                "status": if denied { "error" } else { "ok" },
                "tool_name": tool_name.clone(),
                "tool_call_id": tool_id,
                "arguments": tool_args,
                "result": raw_result,
                "rendered_result": rendered,
                "observation": observation,
                "error": error,
                "error_category": if denied { Some("tool_rejected") } else { None },
                "executor": executor,
                "approval": approval_status,
                "execution_duration_ms": execution_duration_ms,
            })))
        }
        Err(error) => {
            let category = crate::value::error_to_category(&error);
            let error_text = error.to_string();
            let observation = format!(
                "[error from {name}]\n{error}\n[end of {name} error]\n",
                name = tool_name,
                error = error_text
            );
            Ok(json_to_vm_value(&serde_json::json!({
                "ok": false,
                "status": "error",
                "tool_name": tool_name,
                "tool_call_id": tool_id,
                "arguments": tool_args,
                "result": null,
                "rendered_result": error_text,
                "observation": observation,
                "error": error_text,
                "error_category": category.as_str(),
                "executor": executor,
                "approval": approval_status,
                "execution_duration_ms": execution_duration_ms,
            })))
        }
    }
}

const LLM_TRACE_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("agent_trace", agent_trace_builtin)
        .signature("agent_trace()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Return captured agent trace events for the current process."),
    SyncBuiltin::new("agent_trace_summary", agent_trace_summary_builtin)
        .signature("agent_trace_summary()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Return a summarized view of captured agent trace events."),
    SyncBuiltin::new(
        "__host_typed_checkpoint_trace",
        host_typed_checkpoint_trace_builtin,
    )
    .signature("__host_typed_checkpoint_trace(checkpoint)")
    .arity(VmBuiltinArity::Exact(1))
    .doc("Record a typed-output checkpoint event in the current agent trace."),
];

async fn host_mcp_bootstrap_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    use std::collections::BTreeMap;

    let session_id = match args.first() {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_mcp_bootstrap(session_id, specs): session_id must be a string; got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::Runtime(
                "__host_mcp_bootstrap(session_id, specs): missing session_id".to_string(),
            ))
        }
    };

    let specs_val = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let specs_list: Vec<serde_json::Value> = match &specs_val {
        VmValue::List(list) => list.iter().map(crate::mcp::vm_value_to_serde).collect(),
        VmValue::Nil => Vec::new(),
        other => {
            return Err(VmError::Runtime(format!(
                "__host_mcp_bootstrap(session_id, specs): specs must be a list; got {}",
                other.type_name()
            )))
        }
    };

    let mut clients: BTreeMap<String, crate::mcp::VmMcpClientHandle> = BTreeMap::new();
    let mut tools_added: Vec<serde_json::Value> = Vec::new();
    let mut server_infos: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<serde_json::Value> = Vec::new();

    for spec in &specs_list {
        let server_name = spec
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if server_name.is_empty() {
            errors.push(serde_json::json!({"error": "mcp_servers entry missing 'name'"}));
            continue;
        }

        match crate::mcp::connect_mcp_server_from_json(spec).await {
            Err(e) => {
                errors.push(serde_json::json!({
                    "server": server_name,
                    "error": e.to_string(),
                }));
            }
            Ok(handle) => {
                let initialize = handle
                    .initialize_result
                    .lock()
                    .await
                    .clone()
                    .unwrap_or(serde_json::Value::Null);
                let instructions = initialize
                    .get("instructions")
                    .or_else(|| {
                        initialize
                            .get("serverInfo")
                            .and_then(|value| value.get("instructions"))
                    })
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                server_infos.push(serde_json::json!({
                    "name": server_name.clone(),
                    "initialize": initialize,
                    "instructions": instructions,
                }));
                let list_result = handle.call("tools/list", serde_json::json!({})).await;
                match list_result {
                    Err(e) => {
                        errors.push(serde_json::json!({
                            "server": server_name,
                            "error": format!("tools/list failed: {e}"),
                        }));
                    }
                    Ok(result) => {
                        let raw_tools = result
                            .get("tools")
                            .and_then(|t| t.as_array())
                            .cloned()
                            .unwrap_or_default();
                        for mut tool in raw_tools {
                            if let Some(obj) = tool.as_object_mut() {
                                let original_name = obj
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let prefixed_name = format!("{server_name}__{original_name}");
                                // Namespace to avoid conflicts between servers.
                                obj.insert("name".into(), serde_json::Value::String(prefixed_name));
                                obj.insert(
                                    "executor".into(),
                                    serde_json::Value::String("mcp_server".into()),
                                );
                                obj.insert(
                                    "mcp_server".into(),
                                    serde_json::Value::String(server_name.clone()),
                                );
                                obj.insert(
                                    "_mcp_server".into(),
                                    serde_json::Value::String(server_name.clone()),
                                );
                                obj.insert(
                                    "_mcp_tool_name".into(),
                                    serde_json::Value::String(original_name),
                                );
                            }
                            tools_added.push(tool);
                        }
                        clients.insert(server_name, handle);
                    }
                }
            }
        }
    }

    agent_runtime::install_session_mcp_clients(&session_id, clients);

    Ok(json_to_vm_value(&serde_json::json!({
        "tools_added": tools_added,
        "server_info": server_infos,
        "errors": errors,
    })))
}

async fn host_mcp_disconnect_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_mcp_disconnect(session_id): session_id must be a string; got {}",
                other.type_name()
            )))
        }
        None => String::new(),
    };

    if !session_id.is_empty() {
        if let Some(clients) = agent_runtime::take_session_mcp_clients(&session_id) {
            for handle in clients.values() {
                let _ = handle.disconnect().await;
            }
        }
    }

    Ok(VmValue::Bool(true))
}

const AGENT_HOST_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[
    async_builtin!(
        "__host_agent_capture_events",
        host_agent_capture_events_impl
    )
    .signature("__host_agent_capture_events(session_id, body)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Capture agent events emitted while executing a Harn closure."),
    async_builtin!(
        "__host_agent_parse_tool_calls",
        host_agent_parse_tool_calls_impl
    )
    .signature("__host_agent_parse_tool_calls(text, tools?)")
    .arity(VmBuiltinArity::Range { min: 1, max: 2 })
    .doc("Parse model text into normalized agent tool-call records."),
    async_builtin!(
        "__host_agent_dispatch_tool_call",
        host_agent_dispatch_tool_call_impl
    )
    .signature("__host_agent_dispatch_tool_call(call, tools?, options?)")
    .arity(VmBuiltinArity::Range { min: 1, max: 3 })
    .doc("Dispatch one normalized agent tool call through the host tool runtime."),
    async_builtin!(
        "__host_agent_dispatch_tool_batch",
        host_agent_dispatch_tool_batch_impl
    )
    .signature("__host_agent_dispatch_tool_batch(calls, tools?, options?)")
    .arity(VmBuiltinArity::Range { min: 1, max: 3 })
    .doc("Dispatch a batch of normalized agent tool calls through the host tool runtime."),
    async_builtin!("__host_mcp_bootstrap", host_mcp_bootstrap_impl)
        .signature("__host_mcp_bootstrap(session_id, specs)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc(
            "Connect to each MCP server in specs, list their tools (prefixed with \
             server_name__), store handles keyed by session_id, and return \
             {tools_added, errors}.",
        ),
    async_builtin!("__host_mcp_disconnect", host_mcp_disconnect_impl)
        .signature("__host_mcp_disconnect(session_id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc(
            "Disconnect all MCP clients installed for session_id and remove them \
             from the session registry.",
        ),
];

const LLM_HOST_CORE_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[
    async_builtin!("__cost_route", cost_route::cost_route_impl)
        .signature("__cost_route(options)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Route an LLM request by cost and capability metadata."),
    async_builtin!("llm_call", llm_call_impl)
        .signature("llm_call(prompt, system?, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .doc("Execute one LLM call and return the normalized Harn result dict."),
    async_builtin!("llm_stream_call", llm_stream_call_impl)
        .signature("llm_stream_call(prompt, system?, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .doc("Execute one streaming LLM call and return the normalized Harn result dict."),
    async_builtin!("llm_call_safe", llm_call_safe_builtin)
        .signature("llm_call_safe(prompt, system?, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .doc("Execute one LLM call and return a non-throwing safe envelope."),
];

const LLM_STRUCTURED_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[
    async_builtin!("llm_call_structured", llm_call_structured_builtin)
        .signature("llm_call_structured(prompt, schema, options?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .doc("Call an LLM for JSON data and return parsed schema-valid data."),
    async_builtin!("llm_call_structured_safe", llm_call_structured_safe_builtin)
        .signature("llm_call_structured_safe(prompt, schema, options?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .doc("Call an LLM for JSON data and return a non-throwing schema envelope."),
    async_builtin!(
        "llm_call_structured_result",
        llm_call_structured_result_builtin
    )
    .signature("llm_call_structured_result(prompt, schema, options?)")
    .arity(VmBuiltinArity::Range { min: 2, max: 3 })
    .doc("Call an LLM for JSON data and return a diagnostic structured-output envelope."),
];

const SCHEMA_RECOVERY_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[async_builtin!(
    "schema_recover",
    schema_recover_builtin
)
.signature("schema_recover(text, schema, options?)")
.arity(VmBuiltinArity::Range { min: 2, max: 3 })
.doc("Recover malformed JSON text against a schema using deterministic and optional LLM repair.")];

const LLM_RATE_LIMIT_ASYNC_PRIMITIVES: &[AsyncBuiltin] =
    &[async_builtin!("with_rate_limit", with_rate_limit_builtin)
        .signature("with_rate_limit(provider, callback, options?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .doc("Run a closure behind the provider rate limiter with retryable-error backoff.")];

const LLM_HOST_COMPLETION_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[
    async_builtin!("llm_completion", llm_completion_builtin)
        .signature("llm_completion(prefix, suffix?, system?, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 4 })
        .doc("Execute a fill-in-the-middle LLM completion request."),
    async_builtin!("llm_stream", llm_stream_builtin)
        .signature("llm_stream(prompt, system?, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .doc("Execute a legacy channel-based streaming LLM request."),
];

const LLM_MOCK_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("llm_mock", llm_mock_builtin)
        .signature("llm_mock(config)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Register a deterministic LLM mock response for tests."),
    SyncBuiltin::new("llm_mock_calls", llm_mock_calls_builtin)
        .signature("llm_mock_calls()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Return recorded LLM mock calls."),
    SyncBuiltin::new("llm_mock_clear", llm_mock_clear_builtin)
        .signature("llm_mock_clear()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Clear deterministic LLM mocks and recorded calls."),
    SyncBuiltin::new("llm_mock_push_scope", llm_mock_push_scope_builtin)
        .signature("llm_mock_push_scope()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Push an isolated LLM mock scope."),
    SyncBuiltin::new("llm_mock_pop_scope", llm_mock_pop_scope_builtin)
        .signature("llm_mock_pop_scope()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Pop the current isolated LLM mock scope."),
];

fn host_tool_search_score_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let query = match args.first() {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_tool_search_score(query, registry, opts): query must be a string; got {}",
                other.type_name()
            )))
        }
        None => String::new(),
    };
    let registry = args
        .get(1)
        .map(helpers::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let opts = args
        .get(2)
        .map(helpers::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let ranked = tool_search_score::score_tools(&query, &registry, &opts);
    Ok(crate::stdlib::json_to_vm_value(&serde_json::Value::Array(
        ranked
            .into_iter()
            .map(|item| {
                serde_json::json!({
                    "tool_name": item.tool_name,
                    "score": item.score,
                    "snippet": item.snippet,
                })
            })
            .collect(),
    )))
}

const LLM_TOOL_SEARCH_SYNC_PRIMITIVES: &[SyncBuiltin] =
    &[
        SyncBuiltin::new("__host_tool_search_score", host_tool_search_score_builtin)
            .signature("__host_tool_search_score(query, registry, opts)")
            .arity(VmBuiltinArity::Range { min: 2, max: 3 })
            .doc("Rank a tool registry for Harn-managed client-mode tool search."),
    ];

const LLM_RUNTIME_PRIMITIVE_GROUPS: &[BuiltinGroup<'static>] = &[
    BuiltinGroup::new()
        .category("agent.trace")
        .sync(LLM_TRACE_SYNC_PRIMITIVES),
    BuiltinGroup::new()
        .category("agent.host")
        .async_(AGENT_HOST_ASYNC_PRIMITIVES),
    BuiltinGroup::new()
        .category("llm.host")
        .async_(LLM_HOST_CORE_ASYNC_PRIMITIVES),
    BuiltinGroup::new()
        .category("llm.structured")
        .async_(LLM_STRUCTURED_ASYNC_PRIMITIVES),
    BuiltinGroup::new()
        .category("schema.recovery")
        .async_(SCHEMA_RECOVERY_ASYNC_PRIMITIVES),
    BuiltinGroup::new()
        .category("llm.rate_limit")
        .async_(LLM_RATE_LIMIT_ASYNC_PRIMITIVES),
    BuiltinGroup::new()
        .category("llm.host")
        .async_(LLM_HOST_COMPLETION_ASYNC_PRIMITIVES),
    BuiltinGroup::new()
        .category("agent.host")
        .sync(LLM_TOOL_SEARCH_SYNC_PRIMITIVES),
];

const LLM_MOCK_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("llm.mock")
    .sync(LLM_MOCK_SYNC_PRIMITIVES);

async fn llm_call_safe_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    match llm_call_impl(args).await {
        Ok(response) => Ok(llm_safe_envelope_ok(response)),
        Err(err) => Ok(llm_safe_envelope_err(&err)),
    }
}

async fn llm_call_structured_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let rewritten = rewrite_structured_args(args)?;
    let response = llm_call_impl(rewritten).await?;
    Ok(extract_structured_data(response))
}

async fn llm_call_structured_safe_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let rewritten = match rewrite_structured_args(args) {
        Ok(v) => v,
        Err(err) => return Ok(structured_safe_envelope_err(&err)),
    };
    match llm_call_impl(rewritten).await {
        Ok(response) => Ok(structured_safe_envelope_ok(extract_structured_data(
            response,
        ))),
        Err(err) => Ok(structured_safe_envelope_err(&err)),
    }
}

async fn llm_call_structured_result_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    structured_envelope::llm_call_structured_result_impl(args, None).await
}

async fn schema_recover_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    schema_recover::schema_recover_impl(args, None).await
}

async fn with_rate_limit_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let provider = args.first().map(|a| a.display()).unwrap_or_default();
    if provider.is_empty() {
        return Err(VmError::Runtime(
            "with_rate_limit: provider name is required".to_string(),
        ));
    }
    let closure = match args.get(1) {
        Some(VmValue::Closure(c)) => c.clone(),
        _ => {
            return Err(VmError::Runtime(
                "with_rate_limit: second argument must be a closure".to_string(),
            ))
        }
    };
    let opts = args.get(2).and_then(|a| a.as_dict()).cloned();
    let max_retries = helpers::opt_int(&opts, "max_retries").unwrap_or(5).max(0) as usize;
    let mut backoff_ms = helpers::opt_int(&opts, "backoff_ms").unwrap_or(1000).max(1) as u64;

    let mut attempt: usize = 0;
    loop {
        rate_limit::acquire_permit(&provider).await;
        let mut child_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
            VmError::Runtime("with_rate_limit requires an async builtin VM context".to_string())
        })?;
        match child_vm.call_closure_pub(&closure, &[]).await {
            Ok(v) => return Ok(v),
            Err(err) => {
                let cat = crate::value::error_to_category(&err);
                let retryable = matches!(
                    cat,
                    crate::value::ErrorCategory::RateLimit
                        | crate::value::ErrorCategory::Overloaded
                        | crate::value::ErrorCategory::TransientNetwork
                        | crate::value::ErrorCategory::Timeout
                );
                if !retryable || attempt >= max_retries {
                    return Err(err);
                }
                crate::events::log_debug(
                    "llm.with_rate_limit",
                    &format!(
                        "retrying after {cat:?} (attempt {}/{max_retries}) in {backoff_ms}ms",
                        attempt + 1
                    ),
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = backoff_ms.saturating_mul(2).min(30_000);
                attempt += 1;
            }
        }
    }
}

async fn llm_completion_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let prefix = args.first().map(|a| a.display()).unwrap_or_default();
    let suffix = args.get(1).and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    let opts = extract_llm_options(&[
        VmValue::String(Rc::from(prefix.clone())),
        args.get(2).cloned().unwrap_or(VmValue::Nil),
        args.get(3).cloned().unwrap_or(VmValue::Nil),
    ])?;
    if let Some(span_id) = crate::tracing::current_span_id() {
        crate::tracing::span_set_metadata(span_id, "model", serde_json::json!(opts.model.clone()));
        crate::tracing::span_set_metadata(
            span_id,
            "provider",
            serde_json::json!(opts.provider.clone()),
        );
    }

    let start = std::time::Instant::now();
    let result = vm_call_completion_full(&opts, &prefix, suffix.as_deref()).await?;
    trace_llm_call(LlmTraceEntry {
        model: result.model.clone(),
        input_tokens: result.input_tokens,
        output_tokens: result.output_tokens,
        duration_ms: start.elapsed().as_millis() as u64,
    });
    if let Some(span_id) = crate::tracing::current_span_id() {
        crate::tracing::span_set_metadata(span_id, "status", serde_json::json!("ok"));
        crate::tracing::span_set_metadata(
            span_id,
            "input_tokens",
            serde_json::json!(result.input_tokens),
        );
        crate::tracing::span_set_metadata(
            span_id,
            "output_tokens",
            serde_json::json!(result.output_tokens),
        );
    }
    Ok(vm_build_llm_result(&result, None, None, None))
}

fn agent_trace_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let events = trace::peek_agent_trace();
    let list: Vec<VmValue> = events
        .iter()
        .filter_map(|e| serde_json::to_value(e).ok())
        .map(|v| json_to_vm_value(&v))
        .collect();
    Ok(VmValue::List(Rc::new(list)))
}

fn agent_trace_summary_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let summary = trace::agent_trace_summary();
    Ok(json_to_vm_value(&summary))
}

fn host_typed_checkpoint_trace_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let checkpoint = args.first().cloned().unwrap_or(VmValue::Nil);
    let checkpoint_json = helpers::vm_value_to_json(&checkpoint);
    let object = checkpoint_json.as_object();
    let string_field = |key: &str| -> String {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string()
    };
    let opt_string_field = |key: &str| -> Option<String> {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty())
    };
    let usize_field = |key: &str| -> usize {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as usize
    };
    let bool_field = |key: &str| -> bool {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    };
    let list_strings = |key: &str| -> Vec<String> {
        object
            .and_then(|obj| obj.get(key))
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| item.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut errors = list_strings("validation_errors");
    if errors.is_empty() {
        errors = list_strings("validator_failures");
    }
    if errors.is_empty() {
        errors = list_strings("schema_failures");
    }
    if errors.is_empty() {
        errors = list_strings("parse_failures");
    }

    trace::emit_agent_event(trace::AgentTraceEvent::TypedCheckpoint {
        name: string_field("name"),
        status: string_field("status"),
        checkpoint_attempts: usize_field("checkpoint_attempts"),
        llm_attempts: usize_field("attempts"),
        error_category: opt_string_field("error_category"),
        errors,
        repaired: bool_field("repaired"),
        final_accepted: bool_field("final_accepted"),
        raw_text: string_field("raw_text"),
    });
    Ok(VmValue::Nil)
}

/// Register LLM builtins on a VM.
pub fn register_llm_builtins(vm: &mut Vm) {
    rate_limit::init_from_config();
    agent_config::register_agent_control_primitives(vm);
    register_builtin_groups(vm, LLM_RUNTIME_PRIMITIVE_GROUPS);
    agent_config::register_agent_loop(vm);
    agent_session_host::register_agent_session_host_primitives(vm);

    conversation::register_conversation_builtins(vm);
    cache::register_cache_builtins(vm);
    config_builtins::register_config_builtins(vm);
    cost::register_cost_builtins(vm);
    rerank::register_rerank_builtins(vm);
    register_llm_mock_builtins(vm);
    transcript_stats::register_transcript_builtins(vm);
}

/// Register llm_mock / llm_mock_calls / llm_mock_clear builtins.
fn register_llm_mock_builtins(vm: &mut Vm) {
    register_builtin_group(vm, LLM_MOCK_PRIMITIVES);
}

fn llm_mock_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config = match args.first() {
        Some(VmValue::Dict(d)) => d,
        _ => {
            return Err(VmError::Runtime(
                "llm_mock: expected a dict argument".to_string(),
            ))
        }
    };

    let text = config.get("text").map(|v| v.display()).unwrap_or_default();

    let tool_calls = match config.get("tool_calls") {
        Some(VmValue::List(list)) => list
            .iter()
            .map(helpers::vm_value_to_json)
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let logprobs = match config.get("logprobs") {
        Some(VmValue::List(list)) => list
            .iter()
            .map(helpers::vm_value_to_json)
            .collect::<Vec<_>>(),
        Some(VmValue::Nil) | None => Vec::new(),
        _ => {
            return Err(VmError::Runtime(
                "llm_mock: logprobs must be a list of token logprob dicts".to_string(),
            ))
        }
    };

    let match_pattern = config.get("match").and_then(|v| {
        if matches!(v, VmValue::Nil) {
            None
        } else {
            Some(v.display())
        }
    });
    let consume_on_match = matches!(config.get("consume_match"), Some(VmValue::Bool(true)));

    let input_tokens = config.get("input_tokens").and_then(|v| v.as_int());
    let output_tokens = config.get("output_tokens").and_then(|v| v.as_int());
    let cache_read_tokens = config.get("cache_read_tokens").and_then(|v| v.as_int());
    let cache_write_tokens = config
        .get("cache_write_tokens")
        .and_then(|v| v.as_int())
        .or_else(|| {
            config
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_int())
        });
    let thinking = config.get("thinking").and_then(|v| {
        if matches!(v, VmValue::Nil) {
            None
        } else {
            Some(v.display())
        }
    });
    let thinking_summary = config.get("thinking_summary").and_then(|v| {
        if matches!(v, VmValue::Nil) {
            None
        } else {
            Some(v.display())
        }
    });
    let stop_reason = config.get("stop_reason").and_then(|v| {
        if matches!(v, VmValue::Nil) {
            None
        } else {
            Some(v.display())
        }
    });
    let model = config
        .get("model")
        .map(|v| v.display())
        .unwrap_or_else(|| "mock".to_string());

    // Optional error injection: {error: {category, message,
    // retry_after_ms?}}. When present the mock short-circuits the
    // provider call and surfaces as `VmError::CategorizedError`,
    // making it observable via `error_category`, the `llm_call`
    // thrown dict, and the `llm_call_safe` envelope.
    let error = match config.get("error") {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Dict(err_dict)) => {
            let category_str = err_dict
                .get("category")
                .map(|v| v.display())
                .unwrap_or_default();
            if category_str.is_empty() {
                return Err(VmError::Runtime(
                    "llm_mock: error.category is required".to_string(),
                ));
            }
            let category = crate::value::ErrorCategory::parse(&category_str);
            // Reject typos loudly: `parse` falls back to Generic on
            // unknown input. Let `"generic"` through; anything else
            // that fell back is a typo.
            if category.as_str() != category_str {
                return Err(VmError::Runtime(format!(
                    "llm_mock: unknown error category `{category_str}`",
                )));
            }
            let message = err_dict
                .get("message")
                .map(|v| v.display())
                .unwrap_or_default();
            let retry_after_ms = match err_dict.get("retry_after_ms") {
                None | Some(VmValue::Nil) => None,
                Some(v) => match v.as_int() {
                    Some(n) if n >= 0 => Some(n as u64),
                    _ => {
                        return Err(VmError::Runtime(
                            "llm_mock: error.retry_after_ms must be a non-negative int".to_string(),
                        ));
                    }
                },
            };
            Some(mock::MockError {
                category,
                message,
                retry_after_ms,
            })
        }
        _ => {
            return Err(VmError::Runtime(
                "llm_mock: error must be a dict {category, message, retry_after_ms?}".to_string(),
            ));
        }
    };

    mock::push_llm_mock(mock::LlmMock {
        text,
        tool_calls,
        match_pattern,
        consume_on_match,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        thinking,
        thinking_summary,
        stop_reason,
        model,
        provider: None,
        blocks: None,
        logprobs,
        error,
    });
    Ok(VmValue::Nil)
}

fn llm_mock_calls_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let calls = mock::get_llm_mock_calls();
    let result: Vec<VmValue> = calls
        .iter()
        .map(|c| {
            let mut dict = std::collections::BTreeMap::new();
            let messages: Vec<VmValue> = c.messages.iter().map(json_to_vm_value).collect();
            dict.insert("messages".to_string(), VmValue::List(Rc::new(messages)));
            dict.insert(
                "system".to_string(),
                match &c.system {
                    Some(s) => VmValue::String(Rc::from(s.as_str())),
                    None => VmValue::Nil,
                },
            );
            dict.insert(
                "tools".to_string(),
                match &c.tools {
                    Some(t) => {
                        let tools: Vec<VmValue> = t.iter().map(json_to_vm_value).collect();
                        VmValue::List(Rc::new(tools))
                    }
                    None => VmValue::Nil,
                },
            );
            dict.insert(
                "tool_choice".to_string(),
                match &c.tool_choice {
                    Some(choice) => json_to_vm_value(choice),
                    None => VmValue::Nil,
                },
            );
            dict.insert("thinking".to_string(), json_to_vm_value(&c.thinking));
            VmValue::Dict(Rc::new(dict))
        })
        .collect();
    Ok(VmValue::List(Rc::new(result)))
}

fn llm_mock_clear_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    mock::reset_llm_mock_state();
    Ok(VmValue::Nil)
}

fn llm_mock_push_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    mock::push_llm_mock_scope();
    Ok(VmValue::Nil)
}

fn llm_mock_pop_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if !mock::pop_llm_mock_scope() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(
            "llm_mock_pop_scope: no scope to pop",
        ))));
    }
    Ok(VmValue::Nil)
}

async fn llm_stream_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let opts = extract_llm_options(&args)?;
    let provider = opts.provider.clone();
    let prompt_text = opts
        .messages
        .last()
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string();

    let (tx, rx) = tokio::sync::mpsc::channel::<VmValue>(64);
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let closed_clone = closed.clone();
    #[allow(clippy::arc_with_non_send_sync)]
    let tx_arc = Arc::new(tx);
    let tx_for_task = tx_arc.clone();

    tokio::task::spawn_local(async move {
        if provider == "mock" {
            let words: Vec<&str> = prompt_text.split_whitespace().collect();
            for word in &words {
                let _ = tx_for_task.send(VmValue::String(Rc::from(*word))).await;
            }
            closed_clone.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }

        let result = vm_stream_llm(&opts, &tx_for_task).await;
        closed_clone.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Err(e) = result {
            let _ = tx_for_task
                .send(VmValue::String(Rc::from(format!("error: {e}"))))
                .await;
        }
    });

    #[allow(clippy::arc_with_non_send_sync)]
    let handle = VmChannelHandle {
        name: Rc::from("llm_stream"),
        sender: tx_arc,
        receiver: Arc::new(tokio::sync::Mutex::new(rx)),
        closed,
    };
    Ok(VmValue::Channel(handle))
}

fn llm_stream_chunk(
    delta: &str,
    visible_delta: &str,
    partial: &str,
    finish_reason: Option<&str>,
) -> VmValue {
    let mut dict = std::collections::BTreeMap::new();
    dict.insert(
        "delta".to_string(),
        VmValue::String(Rc::from(delta.to_string())),
    );
    dict.insert(
        "visible_delta".to_string(),
        VmValue::String(Rc::from(visible_delta.to_string())),
    );
    dict.insert(
        "partial".to_string(),
        VmValue::String(Rc::from(partial.to_string())),
    );
    dict.insert("role".to_string(), VmValue::String(Rc::from("assistant")));
    dict.insert(
        "finish_reason".to_string(),
        finish_reason
            .map(|reason| VmValue::String(Rc::from(reason.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(dict))
}

async fn forward_llm_stream_delta(
    stream_tx: &tokio::sync::mpsc::Sender<Result<VmValue, VmError>>,
    visible: &mut crate::visible_text::VisibleTextState,
    delta: String,
) -> Result<String, ()> {
    let (partial, visible_delta) = visible.push(&delta, true);
    let chunk = llm_stream_chunk(&delta, &visible_delta, &partial, None);
    stream_tx.send(Ok(chunk)).await.map_err(|_| ())?;
    Ok(partial)
}

async fn send_llm_stream_error(
    stream_tx: &tokio::sync::mpsc::Sender<Result<VmValue, VmError>>,
    err: VmError,
    provider: &str,
    model: &str,
) {
    let wrapped = VmError::Thrown(build_llm_error_dict(&err, provider, model));
    let _ = stream_tx.send(Err(wrapped)).await;
}

/// Shared implementation of `llm_stream_call`. Unlike the legacy
/// `llm_stream` channel helper, this returns a first-class `Stream` of
/// structured chunks and reuses `llm_call`'s provider error taxonomy.
async fn llm_stream_call_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let opts = extract_llm_options(&args)?;
    let provider = opts.provider.clone();
    let model = opts.model.clone();

    let (stream_tx, stream_rx) = tokio::sync::mpsc::channel::<Result<VmValue, VmError>>(64);
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let cancel = VmStreamCancel::new();
    let mut cancel_rx = cancel.subscribe();

    tokio::task::spawn_local(async move {
        let mut visible = crate::visible_text::VisibleTextState::default();
        let mut partial = String::new();
        let mut deltas_open = true;
        let mut llm_task = tokio::task::spawn_local(async move {
            api::vm_call_llm_full_streaming(&opts, delta_tx).await
        });

        loop {
            tokio::select! {
                _ = cancel_rx.changed() => {
                    llm_task.abort();
                    break;
                }
                _ = stream_tx.closed() => {
                    llm_task.abort();
                    break;
                }
                maybe_delta = delta_rx.recv(), if deltas_open => {
                    match maybe_delta {
                        Some(delta) => {
                            match forward_llm_stream_delta(&stream_tx, &mut visible, delta).await {
                                Ok(next_partial) => partial = next_partial,
                                Err(()) => {
                                    llm_task.abort();
                                    break;
                                }
                            }
                        }
                        None => deltas_open = false,
                    }
                }
                joined = &mut llm_task => {
                    while let Ok(delta) = delta_rx.try_recv() {
                        match forward_llm_stream_delta(&stream_tx, &mut visible, delta).await {
                            Ok(next_partial) => partial = next_partial,
                            Err(()) => break,
                        }
                    }
                    match joined {
                        Ok(Ok(result)) => {
                            let final_chunk = llm_stream_chunk(
                                "",
                                "",
                                &partial,
                                result.stop_reason.as_deref(),
                            );
                            let _ = stream_tx.send(Ok(final_chunk)).await;
                        }
                        Ok(Err(err)) => {
                            send_llm_stream_error(&stream_tx, err, &provider, &model).await;
                        }
                        Err(join_err) if join_err.is_cancelled() => {}
                        Err(join_err) => {
                            let err = VmError::Thrown(VmValue::String(Rc::from(format!(
                                "llm_stream_call background task failed: {join_err}"
                            ))));
                            send_llm_stream_error(&stream_tx, err, &provider, &model).await;
                        }
                    }
                    break;
                }
            }
        }
    });

    Ok(VmValue::Stream(VmStream {
        done: Rc::new(std::cell::Cell::new(false)),
        receiver: Rc::new(tokio::sync::Mutex::new(stream_rx)),
        cancel: Some(cancel),
    }))
}

#[cfg(test)]
mod tests {
    use super::api::LlmCallOptions;
    use super::{
        build_schema_nudge, compute_validation_errors, execute_llm_call, reset_llm_state,
        structured_output_errors, SchemaNudge,
    };
    use crate::llm::mock;
    use crate::value::VmValue;
    use std::rc::Rc;

    fn base_opts() -> LlmCallOptions {
        LlmCallOptions {
            provider: "mock".to_string(),
            model: "mock".to_string(),
            api_key: String::new(),
            route_policy: super::api::LlmRoutePolicy::Manual,
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::new(),
            routing_decision: None,
            session_id: None,
            messages: Vec::new(),
            system: None,
            transcript_summary: None,
            max_tokens: 128,
            temperature: None,
            top_p: None,
            top_k: None,
            logprobs: false,
            top_logprobs: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            output_format: super::api::OutputFormat::JsonObject,
            response_format: Some("json".to_string()),
            json_schema: None,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                }
            })),
            output_validation: Some("error".to_string()),
            thinking: crate::llm::api::ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: false,
            tools: None,
            native_tools: None,
            tool_choice: None,
            tool_search: None,
            cache: false,
            stream: true,
            timeout: None,
            idle_timeout: None,
            provider_overrides: None,
            budget: None,
            prefill: None,
            structural_experiment: None,
            applied_structural_experiment: None,
        }
    }

    #[test]
    fn output_validation_accepts_matching_schema() {
        let opts = base_opts();
        let mut map = std::collections::BTreeMap::new();
        map.insert("name".to_string(), VmValue::String(Rc::from("Ada")));
        let data = VmValue::Dict(Rc::new(map));
        let errors = compute_validation_errors(&data, &opts);
        assert!(errors.is_empty(), "schema should pass: {errors:?}");
    }

    #[test]
    fn output_validation_rejects_mismatched_schema_in_error_mode() {
        let opts = base_opts();
        let mut map = std::collections::BTreeMap::new();
        map.insert("name".to_string(), VmValue::Int(42));
        let data = VmValue::Dict(Rc::new(map));
        let errors = compute_validation_errors(&data, &opts);
        assert!(!errors.is_empty(), "schema should fail");
        assert!(errors.join(" ").contains("string"));
    }

    #[test]
    fn structured_output_errors_report_missing_json() {
        let result = VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
            (
                "text".to_string(),
                VmValue::String(Rc::from("Analyzing the task")),
            ),
            (
                "protocol_violations".to_string(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from(
                    "stray text outside response tags",
                ))])),
            ),
            (
                "stop_reason".to_string(),
                VmValue::String(Rc::from("length")),
            ),
        ])));

        let errors = structured_output_errors(&result, &base_opts());
        assert!(errors.iter().any(|err| err.contains("parseable JSON")));
        assert!(errors.iter().any(|err| err.contains("protocol violations")));
        assert!(errors.iter().any(|err| err.contains("token limit")));
    }

    #[test]
    fn schema_retry_nudge_includes_nested_array_object_keys() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["summary", "findings"],
            "additionalProperties": false,
            "properties": {
                "summary": {"type": "string"},
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "severity": {"type": "string"},
                            "title": {"type": "string"},
                            "detail": {"type": "string"},
                            "file": {"type": ["string", "null"]},
                            "line_start": {"type": ["integer", "null"]}
                        }
                    }
                }
            }
        });
        let nudge = build_schema_nudge(
            &["findings[0].description is not allowed".to_string()],
            Some(&schema),
            &SchemaNudge::Auto,
        );

        assert!(nudge.contains("Required keys: summary, findings."));
        assert!(nudge.contains("Expected JSON schema shape:"));
        assert!(nudge.contains("findings[] object allowed keys"));
        assert!(nudge.contains("detail"));
        assert!(nudge.contains("line_start"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_llm_call_retries_when_response_has_no_json_data() {
        reset_llm_state();
        mock::push_llm_mock(mock::LlmMock {
            text: "Analyzing the task carefully".to_string(),
            tool_calls: Vec::new(),
            match_pattern: None,
            consume_on_match: false,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            thinking: None,
            thinking_summary: None,
            stop_reason: None,
            model: "mock".to_string(),
            provider: Some("mock".to_string()),
            blocks: None,
            logprobs: Vec::new(),
            error: None,
        });
        mock::push_llm_mock(mock::LlmMock {
            text: "{\"name\":\"Ada\"}".to_string(),
            tool_calls: Vec::new(),
            match_pattern: None,
            consume_on_match: false,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            thinking: None,
            thinking_summary: None,
            stop_reason: None,
            model: "mock".to_string(),
            provider: Some("mock".to_string()),
            blocks: None,
            logprobs: Vec::new(),
            error: None,
        });

        let response = execute_llm_call(base_opts(), None, None)
            .await
            .expect("structured retry should recover");
        let dict = response.as_dict().expect("dict response");
        let data = dict
            .get("data")
            .and_then(VmValue::as_dict)
            .expect("parsed data");
        assert_eq!(
            data.get("name").map(VmValue::display).as_deref(),
            Some("Ada")
        );
        assert_eq!(mock::get_llm_mock_calls().len(), 2);
    }
}
