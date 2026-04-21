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

mod agent;
mod agent_config;
mod agent_observe;
mod agent_tools;
pub(crate) mod api;
pub mod capabilities;
mod config_builtins;
mod conversation;
pub(crate) mod cost;
pub(crate) mod daemon;
pub(crate) mod helpers;
pub(crate) mod ledger;
pub(crate) mod mock;
pub(crate) mod structural_experiments;
pub(crate) mod tool_search;
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

pub use mock::{
    drain_tool_recordings, load_tool_replay_fixtures, set_tool_recording_mode, ToolRecordingMode,
};
pub(crate) mod provider;
pub(crate) mod providers;
pub(crate) mod rate_limit;
mod stream;
mod tools;
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

use std::rc::Rc;
use std::sync::Arc;

use crate::stdlib::{json_to_vm_value, schema_result_value};
use crate::value::{VmChannelHandle, VmError, VmValue};
use crate::vm::Vm;

use self::api::{vm_build_llm_result, vm_call_completion_full};
use self::daemon::parse_daemon_loop_config;
use self::helpers::{opt_bool, opt_int, opt_str, opt_str_list};
use self::stream::vm_stream_llm;
use self::trace::emit_agent_event;
use self::trace::trace_llm_call;

pub fn install_current_host_bridge(bridge: Rc<crate::bridge::HostBridge>) {
    agent::install_current_host_bridge(bridge);
}

pub fn clear_current_host_bridge() {
    agent::clear_current_host_bridge();
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

/// Extract an initial task ledger from agent_loop options. Mirrors the
/// identical helper in `agent_config.rs` — kept in both places because
/// the two registration paths (bridge-aware and bridge-less) each
/// build their own `AgentLoopConfig` literal.
fn parse_task_ledger_from_vm_options(
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
) -> ledger::TaskLedger {
    use ledger::{Deliverable, DeliverableStatus, TaskLedger};

    let Some(opts) = options.as_ref() else {
        return TaskLedger::default();
    };
    if let Some(explicit) = opts.get("task_ledger") {
        let json = helpers::vm_value_to_json(explicit);
        if let Ok(parsed) = serde_json::from_value::<TaskLedger>(json) {
            return parsed;
        }
    }
    let mut builder = TaskLedger::default();
    if let Some(VmValue::String(s)) = opts.get("root_task") {
        builder.root_task = s.trim().to_string();
    }
    if let Some(deliverables) = opts.get("deliverables").and_then(|v| match v {
        VmValue::List(items) => Some(items.clone()),
        _ => None,
    }) {
        for (idx, item) in deliverables.iter().enumerate() {
            let text = item.display().trim().to_string();
            if text.is_empty() {
                continue;
            }
            builder.deliverables.push(Deliverable {
                id: format!("deliverable-{}", idx + 1),
                text,
                status: DeliverableStatus::Open,
                note: None,
            });
        }
    }
    builder
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

fn structured_output_errors(result: &VmValue, opts: &api::LlmCallOptions) -> Vec<String> {
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
enum SchemaNudge {
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

fn parse_schema_nudge(
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
/// `schema_retry_nudge`; the `Auto` variant enumerates the schema's
/// top-level required keys so small / local models re-emit conforming
/// JSON reliably (see `docs/llm/harn-quickref.md` "Schema retries").
fn build_schema_nudge(
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
            msg.push_str(
                "\nRespond again with ONLY valid JSON conforming to the schema. No prose, no markdown fences.",
            );
            msg
        }
    }
}

pub(crate) use self::agent::parse_skill_match_config_public as parse_skill_match_config_dict;
pub(crate) use self::agent::SkillMatchConfig;
pub(crate) use self::agent::{
    current_agent_session_id, current_host_bridge, emit_agent_event as emit_live_agent_event,
    parse_skill_config, run_agent_loop_internal,
};
pub(crate) use self::agent_config::{agent_loop_result_from_llm, AgentLoopConfig};
pub use self::agent_config::{register_agent_loop_with_bridge, register_llm_call_with_bridge};
pub use self::api::fetch_provider_max_context;
pub(crate) use self::api::vm_call_llm_full;
pub use self::cost::peek_total_cost;
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
    trigger_predicate::reset_trigger_predicate_state();
    capabilities::clear_user_overrides();
}

/// Shared implementation of `llm_call` / `llm_call_safe`. Runs the
/// full schema-retry loop; on success returns the LLM result dict, on
/// failure returns the underlying `VmError`. `llm_call` propagates the
/// error; `llm_call_safe` wraps it in a `{ok: false, error: …}` envelope.
async fn llm_call_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let options = args.get(2).and_then(|a| a.as_dict()).cloned();
    let opts = extract_llm_options(&args)?;
    execute_llm_call(opts, options).await
}

pub(crate) async fn execute_llm_call(
    mut opts: api::LlmCallOptions,
    options: Option<std::collections::BTreeMap<String, VmValue>>,
) -> Result<VmValue, VmError> {
    let _ = structural_experiments::apply_structural_experiment(&mut opts, None).await?;
    // Default `llm_retries` to 2 for resilience against transient
    // HTTP / provider failures. Pass `llm_retries: 0` to opt out.
    let retry_config = agent_observe::LlmRetryConfig {
        retries: helpers::opt_int(&options, "llm_retries").unwrap_or(2) as usize,
        backoff_ms: helpers::opt_int(&options, "llm_backoff_ms").unwrap_or(2000) as u64,
    };
    // Schema retry loop is orthogonal to transient retries. Each
    // schema retry gets a fresh transient budget. Small/local models
    // often need the corrective nudge to produce conforming JSON.
    let schema_retries = helpers::opt_int(&options, "schema_retries")
        .unwrap_or(1)
        .max(0) as usize;
    let nudge_mode = parse_schema_nudge(&options);

    let tool_format = helpers::opt_str(&options, "tool_format");
    for attempt in 0..=schema_retries {
        let result = agent_observe::observed_llm_call(
            &opts,
            tool_format.as_deref(),
            None, // no bridge
            &retry_config,
            None,
            false,
            false, // non-bridge path runs on the local set
        )
        .await?;

        // Non-bridge path runs schema validation; bridge path
        // delegates validation to the host.
        let vm_result = agent_config::build_llm_call_result(&result, &opts);
        if !helpers::expects_structured_output(&opts) {
            return Ok(vm_result);
        }
        let errors = structured_output_errors(&vm_result, &opts);
        if errors.is_empty() {
            return Ok(vm_result);
        }

        let more_attempts = attempt < schema_retries;
        let should_retry = more_attempts;
        if should_retry {
            let nudge = build_schema_nudge(&errors, opts.output_schema.as_ref(), &nudge_mode);
            emit_agent_event(AgentTraceEvent::SchemaRetry {
                attempt: attempt + 1,
                errors: errors.clone(),
                nudge_used: !nudge.is_empty(),
            });
            // Append broken response + corrective nudge so the next
            // call has progressively richer context.
            opts.messages.push(serde_json::json!({
                "role": "assistant",
                "content": result.text,
            }));
            if !nudge.is_empty() {
                opts.messages.push(serde_json::json!({
                    "role": "user",
                    "content": nudge,
                }));
            }
            continue;
        }

        // Attempts exhausted: honor the caller's output_validation mode.
        let hint = if schema_retries == 0 {
            " (hint: set `schema_retries: N` in the llm_call options to automatically re-prompt the model with a corrective nudge)"
        } else {
            " (hint: schema_retries budget exhausted — the model did not produce conforming output after the configured retries; consider raising `schema_retries` or relaxing the schema)"
        };
        let message = format!(
            "LLM output failed schema validation: {}{hint}",
            errors.join("; ")
        );
        match output_validation_mode(&opts) {
            "error" => {
                return Err(crate::value::VmError::CategorizedError {
                    message,
                    category: crate::value::ErrorCategory::SchemaValidation,
                });
            }
            "warn" => {
                crate::events::log_warn("llm", &message);
                return Ok(vm_result);
            }
            _ => return Ok(vm_result),
        }
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
    let mut err_dict = std::collections::BTreeMap::new();
    err_dict.insert(
        "category".to_string(),
        VmValue::String(Rc::from(category.as_str())),
    );
    err_dict.insert("message".to_string(), VmValue::String(Rc::from(message)));
    let mut dict = std::collections::BTreeMap::new();
    dict.insert("ok".to_string(), VmValue::Bool(false));
    dict.insert("response".to_string(), VmValue::Nil);
    dict.insert("error".to_string(), VmValue::Dict(Rc::new(err_dict)));
    VmValue::Dict(Rc::new(dict))
}

/// Register LLM builtins on a VM.
pub fn register_llm_builtins(vm: &mut Vm) {
    rate_limit::init_from_config();
    agent_config::register_agent_subscribe(vm);
    agent_config::register_agent_inject_feedback(vm);
    vm.register_async_builtin("llm_call", |args| async move { llm_call_impl(args).await });
    // `llm_call_safe` shares the exact same execution path as `llm_call`
    // but replaces the throw-on-failure contract with a normalized
    // `{ok, response, error}` envelope. Saves five lines of
    // `try`/`guard`/`unwrap`/`?.data` boilerplate at every callsite.
    vm.register_async_builtin("llm_call_safe", |args| async move {
        match llm_call_impl(args).await {
            Ok(response) => Ok(llm_safe_envelope_ok(response)),
            Err(err) => Ok(llm_safe_envelope_err(&err)),
        }
    });

    // `with_rate_limit(provider, fn() -> T, opts?) -> T` — acquires a
    // permit from the provider's sliding-window rate limiter, invokes
    // the closure, and retries with exponential backoff on
    // classifier-retryable errors. Composes with
    // `HARN_RATE_LIMIT_<PROVIDER>` env vars and `llm_rate_limit(...)`.
    vm.register_async_builtin("with_rate_limit", |args| async move {
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
    });

    vm.register_async_builtin("llm_completion", |args| async move {
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
            crate::tracing::span_set_metadata(
                span_id,
                "model",
                serde_json::json!(opts.model.clone()),
            );
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
    });

    vm.register_async_builtin("agent_loop", |args| async move {
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();
        let max_iterations = opt_int(&options, "max_iterations").unwrap_or(50) as usize;
        let persistent = opt_bool(&options, "persistent");
        let max_nudges = opt_int(&options, "max_nudges").unwrap_or(3) as usize;
        let custom_nudge = opt_str(&options, "nudge");
        let tool_retries = opt_int(&options, "tool_retries").unwrap_or(0) as usize;
        let tool_backoff_ms = opt_int(&options, "tool_backoff_ms").unwrap_or(1000) as u64;
        let tool_format = opt_str(&options, "tool_format").unwrap_or_else(|| "text".to_string());
        let native_tool_fallback = opt_str(&options, "native_tool_fallback")
            .map(|value| {
                crate::orchestration::NativeToolFallbackPolicy::parse(&value).ok_or_else(|| {
                    crate::value::VmError::Runtime(format!(
                        "agent_loop: native_tool_fallback must be one of allow, allow_once, reject; got `{value}`"
                    ))
                })
            })
            .transpose()?
            .unwrap_or_default();
        let daemon = opt_bool(&options, "daemon");
        // Empty string means "mint an anonymous session" (state.rs handles
        // this path and does not persist). A caller-provided id flows
        // through as the session's persistent identity.
        let session_id = opt_str(&options, "session_id").unwrap_or_default();
        let auto_compact = if opt_bool(&options, "auto_compact") {
            let mut ac = crate::orchestration::AutoCompactConfig::default();
            if let Some(v) = opt_int(&options, "compact_threshold") {
                ac.token_threshold = v as usize;
            }
            if let Some(v) = opt_int(&options, "tool_output_max_chars") {
                ac.tool_output_max_chars = v as usize;
            }
            if let Some(v) = opt_int(&options, "compact_keep_last") {
                ac.keep_last = v as usize;
            }
            if let Some(strategy) = opt_str(&options, "compact_strategy") {
                ac.compact_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
            }
            if let Some(callback) = options.as_ref().and_then(|o| o.get("compact_callback")) {
                ac.custom_compactor = Some(callback.clone());
                if !options
                    .as_ref()
                    .is_some_and(|o| o.contains_key("compact_strategy"))
                {
                    ac.compact_strategy = crate::orchestration::CompactStrategy::Custom;
                }
            }
            if let Some(callback) = options.as_ref().and_then(|o| o.get("compress_callback")) {
                ac.compress_callback = Some(callback.clone());
            }
            Some(ac)
        } else {
            None
        };
        let policy = options.as_ref().and_then(|o| o.get("policy")).map(|v| {
            let json = crate::llm::helpers::vm_value_to_json(v);
            serde_json::from_value::<crate::orchestration::CapabilityPolicy>(json)
                .unwrap_or_default()
        });
        let turn_policy = options
            .as_ref()
            .and_then(|o| o.get("turn_policy"))
            .map(|v| {
                let json = crate::llm::helpers::vm_value_to_json(v);
                serde_json::from_value::<crate::orchestration::TurnPolicy>(json).unwrap_or_default()
            });
        let approval_policy = options
            .as_ref()
            .and_then(|o| o.get("approval_policy"))
            .map(|v| {
                let json = crate::llm::helpers::vm_value_to_json(v);
                serde_json::from_value::<crate::orchestration::ToolApprovalPolicy>(json)
                    .unwrap_or_default()
            });
        let done_sentinel = opt_str(&options, "done_sentinel");
        let break_unless_phase = opt_str(&options, "break_unless_phase");
        let exit_when_verified = opt_bool(&options, "exit_when_verified");
        let daemon_config = parse_daemon_loop_config(options.as_ref());
        let (skill_registry, skill_match, working_files) =
            crate::llm::agent::parse_skill_config(&options);
        let mut opts = extract_llm_options(&args)?;
        let result = run_agent_loop_internal(
            &mut opts,
            AgentLoopConfig {
                persistent,
                max_iterations,
                max_nudges,
                nudge: custom_nudge,
                done_sentinel,
                break_unless_phase,
                tool_retries,
                tool_backoff_ms,
                tool_format,
                native_tool_fallback,
                auto_compact,
                policy,
                approval_policy,
                daemon,
                daemon_config,
                llm_retries: opt_int(&options, "llm_retries").unwrap_or(3) as usize,
                llm_backoff_ms: opt_int(&options, "llm_backoff_ms").unwrap_or(2000) as u64,
                token_budget: opt_int(&options, "token_budget"),
                exit_when_verified,
                loop_detect_warn: opt_int(&options, "loop_detect_warn").unwrap_or(2) as usize,
                loop_detect_block: opt_int(&options, "loop_detect_block").unwrap_or(3) as usize,
                loop_detect_skip: opt_int(&options, "loop_detect_skip").unwrap_or(4) as usize,
                tool_examples: opt_str(&options, "tool_examples"),
                turn_policy,
                stop_after_successful_tools: opt_str_list(&options, "stop_after_successful_tools"),
                require_successful_tools: opt_str_list(&options, "require_successful_tools"),
                session_id,
                event_sink: None,
                task_ledger: parse_task_ledger_from_vm_options(&options),
                post_turn_callback: options
                    .as_ref()
                    .and_then(|o| o.get("post_turn_callback"))
                    .filter(|v| matches!(v, crate::value::VmValue::Closure(_)))
                    .cloned(),
                skill_registry,
                skill_match,
                working_files,
            },
        )
        .await?;
        Ok(json_to_vm_value(&result))
    });

    register_llm_stream(vm);
    conversation::register_conversation_builtins(vm);
    config_builtins::register_config_builtins(vm);
    cost::register_cost_builtins(vm);
    register_llm_mock_builtins(vm);
    transcript_stats::register_transcript_builtins(vm);

    vm.register_builtin("agent_trace", |_args, _out| {
        let events = trace::peek_agent_trace();
        let list: Vec<VmValue> = events
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .map(|v| json_to_vm_value(&v))
            .collect();
        Ok(VmValue::List(Rc::new(list)))
    });

    vm.register_builtin("agent_trace_summary", |_args, _out| {
        let summary = trace::agent_trace_summary();
        Ok(json_to_vm_value(&summary))
    });
}

/// Register llm_mock / llm_mock_calls / llm_mock_clear builtins.
fn register_llm_mock_builtins(vm: &mut Vm) {
    use mock::{get_llm_mock_calls, push_llm_mock, reset_llm_mock_state, LlmMock, MockError};

    vm.register_builtin("llm_mock", |args, _out| {
        let config = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(crate::value::VmError::Runtime(
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
        let thinking = config.get("thinking").and_then(|v| {
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

        // Optional error injection: {error: {category, message}}. When
        // present the mock short-circuits the provider call and surfaces
        // as `VmError::CategorizedError`, making it observable via
        // `error_category`, `try { ... }`, and the `llm_call_safe`
        // envelope.
        let error = match config.get("error") {
            None | Some(VmValue::Nil) => None,
            Some(VmValue::Dict(err_dict)) => {
                let category_str = err_dict
                    .get("category")
                    .map(|v| v.display())
                    .unwrap_or_default();
                if category_str.is_empty() {
                    return Err(crate::value::VmError::Runtime(
                        "llm_mock: error.category is required".to_string(),
                    ));
                }
                let category = crate::value::ErrorCategory::parse(&category_str);
                // Reject typos loudly: `parse` falls back to Generic on
                // unknown input. Let `"generic"` through; anything else
                // that fell back is a typo.
                if category.as_str() != category_str {
                    return Err(crate::value::VmError::Runtime(format!(
                        "llm_mock: unknown error category `{category_str}`",
                    )));
                }
                let message = err_dict
                    .get("message")
                    .map(|v| v.display())
                    .unwrap_or_default();
                Some(MockError { category, message })
            }
            _ => {
                return Err(crate::value::VmError::Runtime(
                    "llm_mock: error must be a dict {category, message}".to_string(),
                ));
            }
        };

        push_llm_mock(LlmMock {
            text,
            tool_calls,
            match_pattern,
            consume_on_match,
            input_tokens,
            output_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
            thinking,
            stop_reason,
            model,
            provider: None,
            blocks: None,
            error,
        });
        Ok(VmValue::Nil)
    });

    vm.register_builtin("llm_mock_calls", |_args, _out| {
        let calls = get_llm_mock_calls();
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
                VmValue::Dict(Rc::new(dict))
            })
            .collect();
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_builtin("llm_mock_clear", |_args, _out| {
        reset_llm_mock_state();
        Ok(VmValue::Nil)
    });
}

/// Register llm_stream builtin.
fn register_llm_stream(vm: &mut Vm) {
    vm.register_async_builtin("llm_stream", |args| async move {
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
    });
}

#[cfg(test)]
mod tests {
    use super::api::LlmCallOptions;
    use super::{
        compute_validation_errors, execute_llm_call, reset_llm_state, structured_output_errors,
    };
    use crate::llm::mock;
    use crate::value::VmValue;
    use std::rc::Rc;

    fn base_opts() -> LlmCallOptions {
        LlmCallOptions {
            provider: "mock".to_string(),
            model: "mock".to_string(),
            api_key: String::new(),
            messages: Vec::new(),
            system: None,
            transcript_summary: None,
            max_tokens: 128,
            temperature: None,
            top_p: None,
            top_k: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            response_format: Some("json".to_string()),
            json_schema: None,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                }
            })),
            output_validation: Some("error".to_string()),
            thinking: None,
            tools: None,
            native_tools: None,
            tool_choice: None,
            tool_search: None,
            cache: false,
            stream: true,
            timeout: None,
            idle_timeout: None,
            provider_overrides: None,
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
            stop_reason: None,
            model: "mock".to_string(),
            provider: Some("mock".to_string()),
            blocks: None,
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
            stop_reason: None,
            model: "mock".to_string(),
            provider: Some("mock".to_string()),
            blocks: None,
            error: None,
        });

        let response = execute_llm_call(base_opts(), None)
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
