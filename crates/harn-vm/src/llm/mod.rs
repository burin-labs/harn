//! LLM integration: API calls, streaming, agent loops, tool handling, and tracing.
//!
//! This module is split into sub-modules for maintainability:
//! - `api`: core LLM API calls, request building, and response parsing
//! - `call`: public call wrappers, schema retry, routing dispatch, and safe envelopes
//! - `agent_*`: agent loop, host bridge, session, and tool orchestration support
//! - `*_builtins`: VM builtin registration glue grouped by runtime concern
//! - `helpers`: option extraction, provider/model/key resolution, and JSON conversion
//! - `mock`, `trace`, and `stream`: process-local test doubles, tracing, and streaming support

mod agent_config;
mod agent_host_primitives;
pub(crate) mod agent_observe;
mod agent_runtime;
mod agent_session_host;
mod agent_tools;
pub mod api;
pub(crate) mod autonomy_budget;
pub(crate) mod cache;
mod call;
pub mod capabilities;
pub(crate) mod config_builtins;
pub(crate) mod content;
mod conversation;
pub(crate) mod cost;
pub(crate) mod cost_route;
pub(crate) mod daemon;
pub mod eval;
pub(crate) mod fake;
pub(crate) mod helpers;
pub mod introspection;
pub mod jsonl;
pub mod local_profiles;
pub(crate) mod mock;
mod mock_builtins;
mod model_test;
pub(crate) mod permissions;
pub mod plan;
pub mod readiness;
pub mod reasoning_policy;
pub(crate) mod reminder_providers;
mod rerank;
pub(crate) mod routing;
pub(crate) mod routing_verifier;
pub(crate) mod schema_recover;
pub(crate) mod skill_score;
mod stream_builtins;
pub(crate) mod structural_experiments;
pub(crate) mod structured_envelope;
mod token_count;
pub mod tool_conformance;
mod tool_search_score;
mod trace_builtins;
pub(crate) mod transcript_seed;
mod transcript_stats;

use std::sync::OnceLock;

pub(crate) use call::snapshot_in_flight_llm_calls;

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
                .timeout(std::time::Duration::from_mins(2))
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
pub mod receipts;
mod stream;
pub(crate) mod tools;
mod trace;
pub(crate) mod trigger_predicate;

/// Shared process-wide lock for tests that mutate LLM-related environment
/// variables (LOCAL_LLM_BASE_URL, LOCAL_LLM_MODEL, HARN_LLM_*). Any test that
/// sets or removes one of these MUST hold this lock for its whole duration,
/// including through any async LLM call, so concurrent tests from sibling
/// modules cannot clobber each other's env and leak stale values into a
/// streaming request.
#[cfg(test)]
pub(crate) fn env_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub const LLM_CALLS_DISABLED_ENV: &str = "HARN_LLM_CALLS_DISABLED";

pub fn estimate_text_tokens(text: &str) -> i64 {
    token_count::estimate_text_tokens(text, None).tokens
}

pub fn llm_pricing_per_1k(provider: &str, model: &str) -> Option<(f64, f64)> {
    cost::pricing_per_1k_for(provider, model)
}

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

use crate::stdlib::macros::{
    harn_builtin, register_builtin_defs, register_deferred_builtin_defs, VmBuiltinDef,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;
use std::rc::Rc;

use self::api::{vm_build_llm_result, vm_call_completion_full};
use self::call::{llm_call_impl, llm_safe_envelope_err, llm_safe_envelope_ok};
use self::stream_builtins::{llm_stream_builtin, llm_stream_call_impl};
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

pub(crate) use self::agent_config::agent_loop_result_from_llm;
pub use self::agent_config::{
    register_agent_loop_with_bridge, register_llm_call_structured_with_bridge,
    register_llm_call_with_bridge,
};
pub use self::agent_runtime::{current_agent_session_id, register_session_end_hook};
pub(crate) use self::agent_runtime::{
    current_host_bridge, emit_agent_event as emit_live_agent_event,
    emit_agent_event_sync as emit_live_agent_event_sync,
};
pub(crate) use self::api::vm_call_llm_full;
pub use self::api::{
    fetch_provider_max_context, probe_openai_compatible_model, selected_model_for_provider,
    supports_model_readiness_probe, ModelReadiness,
};
pub(crate) use self::call::{
    execute_llm_call, execute_schema_retry_loop, extract_structured_data, rewrite_structured_args,
    structured_output_errors, structured_safe_envelope_err, structured_safe_envelope_ok,
    SchemaLoopOutcome,
};
pub use self::cost::{
    calculate_cost_for_provider, install_llm_cost_budget, install_llm_token_budget,
    peek_total_cost, peek_total_tokens, LlmBudgetGuard, LlmTokenBudgetGuard,
};
pub use self::healthcheck::{
    build_healthcheck_url, run_provider_healthcheck, run_provider_healthcheck_with_options,
    ProviderHealthcheckOptions, ProviderHealthcheckResult,
};
pub(crate) use self::helpers::extract_llm_options;
pub use self::helpers::no_credentials_message;
pub use self::helpers::resolve_api_key;
pub use self::helpers::vm_value_to_json;
pub use self::jsonl::{load_llm_mocks_jsonl, parse_llm_mock_value, serialize_llm_mock};
pub use self::mock::{
    clear_cli_llm_mock_mode, enable_cli_llm_mock_recording, install_cli_llm_mocks, set_replay_mode,
    take_cli_llm_recordings, LlmMock, LlmReplayMode, MockError,
};
pub use self::model_test::{run_model_smoke_test, ModelSmokeTestOptions, ModelSmokeTestResult};
pub use self::trace::{
    agent_trace_summary, enable_tracing, peek_agent_trace, peek_trace, peek_trace_summary,
    take_agent_trace, take_trace, AgentTraceEvent, LlmTraceEntry,
};

/// Reset all thread-local LLM state (cost, trace, mock, rate limits). Call between test runs.
pub fn reset_llm_state() {
    call::clear_in_flight_llm_calls();
    introspection::reset_snapshot();
    cost::reset_cost_state();
    trace::reset_trace_state();
    trace::reset_agent_trace_state();
    provider::register_default_providers();
    rate_limit::reset_rate_limit_state();
    mock::reset_llm_mock_state();
    autonomy_budget::reset_autonomy_budget_state();
    agent_session_host::reset_agent_session_host_state();
    reminder_providers::clear_reminder_providers();
    permissions::clear_dynamic_permission_state();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    trigger_predicate::reset_trigger_predicate_state();
    capabilities::clear_user_overrides();
    // Per-`@step` registry, active stack, and completed-step log are
    // thread-local; clear them between runs to prevent stale step
    // metadata from one program leaking into the next.
    crate::step_runtime::reset_thread_local_state();
}

/// Route an LLM request by cost and capability metadata.
#[harn_builtin(
    sig = "__cost_route(options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.host",
    runtime_only = true
)]
async fn cost_route_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    cost_route::cost_route_impl(args).await
}

/// Execute one LLM call and return the normalized Harn result dict.
#[harn_builtin(
    sig = "llm_call(prompt: any, system?: any, options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.host",
    runtime_only = true
)]
async fn llm_call_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    llm_call_impl(args).await
}

/// Execute one streaming LLM call and return the normalized Harn result dict.
#[harn_builtin(
    sig = "llm_stream_call(prompt: any, system?: any, options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.host",
    runtime_only = true
)]
async fn llm_stream_call_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    llm_stream_call_impl(args).await
}

/// Rank a tool registry for Harn-managed client-mode tool search.
#[harn_builtin(
    sig = "__host_tool_search_score(query: string, registry: dict, opts?: dict|nil) -> list",
    category = "agent.host",
    runtime_only = true
)]
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

/// Build a first-class routing policy handle. Pass {chain: [{provider, model}, ...],
/// failover: {on_status?, on_timeout_ms?, on_error_kinds?, max_attempts?},
/// latency: {race_after_ms?, target_p95_ms?}, budget: {per_call_usd?, session_usd?, on_exceed?},
/// observe: {emit_event?}} and pipe the result through `llm_call(... routing: policy ...)`
/// to drive the chain with failover, latency-aware racing, and budget caps. Tape events:
/// <dispatch>.{decision,attempt,race_started,race_won,race_lost,budget_exceeded,exhausted}.
#[harn_builtin(
    sig = "routing_policy(config: dict) -> dict",
    category = "llm.host",
    runtime_only = true
)]
fn routing_policy_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config = match args.first() {
        Some(VmValue::Dict(dict)) => dict.as_ref().clone(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "routing_policy: expected a config dict, got {}",
                other.type_name()
            )));
        }
        None => {
            return Err(VmError::Runtime(
                "routing_policy: expected a config dict".to_string(),
            ));
        }
    };
    routing::build_routing_policy(&config)
}

/// Return resolved runtime facts {provider, model, model_alias, family, tool_format, tier,
/// context_window, runtime_context_window, capabilities, harn_version, harness} for the
/// most recent llm_call on this thread. Fields are nil before any llm_call has run. The
/// model-callable surface (current_model() / current_provider() / ...) is opt-in via
/// `runtime_introspection_tools(registry)`; this builtin is the underlying read.
#[harn_builtin(
    sig = "runtime_introspection() -> dict",
    category = "llm.introspection",
    runtime_only = true
)]
fn runtime_introspection_builtin_wrap(
    args: &[VmValue],
    out: &mut String,
) -> Result<VmValue, VmError> {
    introspection::runtime_introspection_builtin(args, out)
}

const LLM_RUNTIME_PRIMITIVE_BUILTINS: &[&VmBuiltinDef] = &[
    // trace
    &trace_builtins::AGENT_TRACE_BUILTIN_DEF,
    &trace_builtins::AGENT_TRACE_SUMMARY_BUILTIN_DEF,
    &trace_builtins::HOST_TYPED_CHECKPOINT_TRACE_BUILTIN_DEF,
    // agent.host
    &agent_host_primitives::HOST_AGENT_CAPTURE_EVENTS_IMPL_DEF,
    &agent_host_primitives::HOST_AGENT_PARSE_TOOL_CALLS_IMPL_DEF,
    &agent_host_primitives::HOST_AGENT_DISPATCH_TOOL_CALL_IMPL_DEF,
    &agent_host_primitives::HOST_AGENT_DISPATCH_TOOL_BATCH_IMPL_DEF,
    &agent_host_primitives::HOST_MCP_BOOTSTRAP_IMPL_DEF,
    &agent_host_primitives::HOST_MCP_DISCONNECT_IMPL_DEF,
    &agent_host_primitives::HOST_AGENT_REMINDER_PROVIDERS_FIRE_IMPL_DEF,
    // llm.host core
    &COST_ROUTE_BUILTIN_DEF,
    &LLM_CALL_BUILTIN_DEF,
    &LLM_STREAM_CALL_BUILTIN_DEF,
    &LLM_CALL_SAFE_BUILTIN_DEF,
    // llm.structured
    &LLM_CALL_STRUCTURED_BUILTIN_DEF,
    &LLM_CALL_STRUCTURED_SAFE_BUILTIN_DEF,
    &LLM_CALL_STRUCTURED_RESULT_BUILTIN_DEF,
    // schema.recovery
    &SCHEMA_RECOVER_BUILTIN_DEF,
    // llm.rate_limit
    &WITH_RATE_LIMIT_BUILTIN_DEF,
    // llm.host completion + stream
    &LLM_COMPLETION_BUILTIN_DEF,
    &LLM_STREAM_BUILTIN_WRAP_DEF,
    // agent.host tool search
    &HOST_TOOL_SEARCH_SCORE_BUILTIN_DEF,
    // llm.host routing
    &ROUTING_POLICY_BUILTIN_DEF,
    // llm.introspection
    &RUNTIME_INTROSPECTION_BUILTIN_WRAP_DEF,
];

/// Execute one LLM call and return a non-throwing safe envelope.
#[harn_builtin(
    sig = "llm_call_safe(prompt: any, system?: any, options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.host",
    runtime_only = true
)]
async fn llm_call_safe_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    match llm_call_impl(args).await {
        Ok(response) => Ok(llm_safe_envelope_ok(response)),
        Err(err) => Ok(llm_safe_envelope_err(&err)),
    }
}

/// Call an LLM for JSON data and return parsed schema-valid data.
#[harn_builtin(
    sig = "llm_call_structured(prompt: any, schema: dict, options?: dict|nil) -> any",
    kind = "async",
    category = "llm.structured",
    runtime_only = true
)]
async fn llm_call_structured_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let rewritten = rewrite_structured_args(args)?;
    let response = llm_call_impl(rewritten).await?;
    Ok(extract_structured_data(response))
}

/// Call an LLM for JSON data and return a non-throwing schema envelope.
#[harn_builtin(
    sig = "llm_call_structured_safe(prompt: any, schema: dict, options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.structured",
    runtime_only = true
)]
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

/// Call an LLM for JSON data and return a diagnostic structured-output envelope.
#[harn_builtin(
    sig = "llm_call_structured_result(prompt: any, schema: dict, options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.structured",
    runtime_only = true
)]
async fn llm_call_structured_result_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    structured_envelope::llm_call_structured_result_impl(args, None).await
}

/// Recover malformed JSON text against a schema using deterministic and optional LLM repair.
#[harn_builtin(
    sig = "schema_recover(text: string, schema: dict, options?: dict|nil) -> dict",
    kind = "async",
    category = "schema.recovery",
    runtime_only = true
)]
async fn schema_recover_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    schema_recover::schema_recover_impl(args, None).await
}

/// Run a closure behind the provider rate limiter with retryable-error backoff.
#[harn_builtin(
    sig = "with_rate_limit(provider: string, callback: closure, options?: dict|nil) -> any",
    kind = "async",
    category = "llm.rate_limit",
    runtime_only = true
)]
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
                crate::clock_mock::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = backoff_ms.saturating_mul(2).min(30_000);
                attempt += 1;
            }
        }
    }
}

/// Execute a fill-in-the-middle LLM completion request.
#[harn_builtin(
    sig = "llm_completion(prefix: string, suffix?: string|nil, system?: any, options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.host",
    runtime_only = true
)]
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

/// Execute a channel-based streaming LLM request.
#[harn_builtin(
    sig = "llm_stream(prompt: any, system?: any, options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.host",
    runtime_only = true
)]
async fn llm_stream_builtin_wrap(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    llm_stream_builtin(args).await
}

/// Register LLM builtins on a VM.
pub fn register_llm_builtins(vm: &mut Vm) {
    agent_config::register_agent_control_primitives(vm);
    register_builtin_defs(vm, LLM_RUNTIME_PRIMITIVE_BUILTINS);
    agent_config::register_agent_loop(vm);
    agent_session_host::register_agent_session_host_primitives(vm);

    conversation::register_conversation_builtins(vm);
    cache::register_cache_builtins(vm);
    config_builtins::register_config_builtins(vm);
    cost::register_cost_builtins(vm);
    rerank::register_rerank_builtins(vm);
    mock_builtins::register_llm_mock_builtins(vm);
    transcript_stats::register_transcript_builtins(vm);
}

/// Register LLM builtin names without installing their handlers. The first
/// resolution of any deferred name installs the complete LLM surface.
pub(crate) fn register_deferred_llm_builtins(vm: &mut Vm) {
    agent_config::register_deferred_agent_control_primitives(vm, register_llm_builtins);
    register_deferred_builtin_defs(vm, LLM_RUNTIME_PRIMITIVE_BUILTINS, register_llm_builtins);
    agent_config::register_deferred_agent_loop(vm, register_llm_builtins);
    agent_session_host::register_deferred_agent_session_host_primitives(vm, register_llm_builtins);

    conversation::register_deferred_conversation_builtins(vm, register_llm_builtins);
    cache::register_deferred_cache_builtins(vm, register_llm_builtins);
    config_builtins::register_deferred_config_builtins(vm, register_llm_builtins);
    cost::register_deferred_cost_builtins(vm, register_llm_builtins);
    rerank::register_deferred_rerank_builtins(vm, register_llm_builtins);
    mock_builtins::register_deferred_llm_mock_builtins(vm, register_llm_builtins);
    transcript_stats::register_deferred_transcript_builtins(vm, register_llm_builtins);
}

#[cfg(test)]
mod tests {
    use super::api::LlmCallOptions;
    use super::call::{build_schema_nudge, compute_validation_errors, SchemaNudge};
    use super::{
        execute_llm_call, register_deferred_llm_builtins, register_llm_builtins, reset_llm_state,
        structured_output_errors,
    };
    use crate::llm::mock;
    use crate::value::VmValue;
    use std::rc::Rc;

    fn base_opts() -> LlmCallOptions {
        LlmCallOptions {
            provider: "mock".to_string(),
            model: "mock".to_string(),
            api_key: String::new(),
            api_mode: super::api::LlmApiMode::ChatCompletions,
            route_policy: super::api::LlmRoutePolicy::Manual,
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::new(),
            routing_decision: None,
            routing_policy: None,
            session_id: None,
            reminders: None,
            reminder_lifecycle: Vec::new(),
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
            schema_stream_abort: true,
            thinking: crate::llm::api::ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: false,
            tools: None,
            native_tools: None,
            provider_tools: Vec::new(),
            tool_choice: None,
            tool_search: None,
            cache: false,
            stream: true,
            timeout: None,
            idle_timeout: None,
            provider_overrides: None,
            previous_response_id: None,
            store: None,
            background: None,
            truncation: None,
            compact: None,
            include: None,
            max_tool_calls: None,
            budget: None,
            prefill: None,
            structural_experiment: None,
            applied_structural_experiment: None,
        }
    }

    #[test]
    fn deferred_llm_builtins_install_on_first_resolution() {
        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib_with_deferred_llm(&mut vm);

        assert!(!vm
            .builtin_names()
            .iter()
            .any(|name| name == "llm_rate_limit"));
        assert!(vm.ensure_deferred_builtin("llm_rate_limit"));
        assert!(vm
            .builtin_names()
            .iter()
            .any(|name| name == "llm_rate_limit"));
        assert!(vm.builtin_names().iter().any(|name| name == "llm_call"));
    }

    #[test]
    fn deferred_sync_llm_builtin_installs_on_direct_call() {
        mock::reset_llm_mock_state();
        let chunk = crate::compile_source("llm_mock({text: \"ok\"})\n").expect("compile");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut vm = crate::Vm::new();
                    crate::register_vm_stdlib_with_deferred_llm(&mut vm);
                    vm.execute(&chunk).await.expect("execute");
                    assert!(mock::builtin_llm_mock_active());
                    assert!(vm.builtin_names().iter().any(|name| name == "llm_mock"));
                })
                .await;
        });
        mock::reset_llm_mock_state();
    }

    #[test]
    fn deferred_llm_builtin_names_match_eager_registration() {
        let mut eager = crate::Vm::new();
        register_llm_builtins(&mut eager);
        let eager_names = eager
            .builtin_names()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();

        let mut deferred = crate::Vm::new();
        register_deferred_llm_builtins(&mut deferred);
        let deferred_names = deferred
            .deferred_builtin_registrars
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(deferred_names, eager_names);
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
