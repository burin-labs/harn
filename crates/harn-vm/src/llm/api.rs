//! LLM API entry points and re-exports. The transport layer, request/
//! response parsing, auth, context-window discovery, and option/result
//! types each live in their own submodule under [`self`]; this file only
//! wires them together and hosts the `vm_call_llm_full*` chat entry
//! points that provider-specific completion / agent paths dispatch into.

mod auth;
mod completion;
mod context_window;
pub mod data_controls;
mod dialect;
pub(crate) mod errors;
pub(crate) mod inference_boundary;
mod isolated_request;
mod ollama;
mod openai_normalize;
pub(crate) mod options;
mod partial_tool_args;
mod response;
pub(crate) mod result;
mod schema_stream;
mod telemetry;
mod thinking;
mod tool_result_provenance;
mod transport;
pub(crate) use transport::reqwest_send_error;

use crate::value::{ErrorCategory, VmDictExt, VmError, VmValue};

use super::mock::{
    fixture_hash_for_request, get_replay_mode, load_fixture, mock_llm_response,
    record_cli_llm_result, LlmReplayMode,
};

// ─── Public surface (crate-wide) ────────────────────────────────────────

pub(crate) use auth::apply_auth_headers;
pub(crate) use completion::vm_call_completion_full;
pub use context_window::fetch_provider_max_context;
pub use data_controls::{
    declaration_for as provider_data_controls, AppliedDataControl, DataControlsOutcome,
    DataControlsReceipt,
};
pub(crate) use dialect::{DialectContract, StreamProtocol};
pub(crate) use errors::{
    classify_llm_error, classify_provider_stream_error, classify_vm_llm_error, err_for_non_success,
    err_for_non_success_with_dialect, parse_retry_after_value, provider_http_error,
    retry_after_header, LlmErrorInfo,
};
/// The closed LLM-outcome vocabularies carried on the ACP prompt-error
/// envelope. Public so the protocol-artifact generator can project them into
/// every host binding rather than leaving `kind`/`reason` as bare strings.
pub use errors::{LlmErrorKind, LlmErrorReason};
#[doc(inline)]
pub use harn_llm::inference::{InferenceBoundary, InferenceReach};
pub use inference_boundary::{
    capture_process_attestations, inference_admission_schemas, preview_inference_admission,
    DataControlsRefusal, InferenceAdmissionRequest, InferenceAdmissionSnapshot,
    InferenceAdmissionStatus, ProviderAccountAttestation, DATA_CONTROLS_TRAINING_REFUSED_RULE,
    HOST_ACCOUNT_ATTESTATIONS_ENV,
};
pub(crate) use ollama::apply_ollama_runtime_settings;
pub(crate) use ollama::ollama_unload_grace_duration_from_env;
pub use ollama::{
    normalize_ollama_keep_alive, ollama_readiness, ollama_readiness_for_provider,
    ollama_runtime_settings_from_env, warm_ollama_model, warm_ollama_model_with_settings,
    OllamaReadinessOptions, OllamaReadinessResult, OllamaRuntimeSettings, OllamaWarmupResult,
    HARN_OLLAMA_KEEP_ALIVE_ENV, HARN_OLLAMA_NUM_CTX_ENV, OLLAMA_DEFAULT_KEEP_ALIVE,
    OLLAMA_DEFAULT_NUM_CTX, OLLAMA_HOST_ENV,
};
pub(crate) use openai_normalize::normalize_openai_style_messages;
pub use options::{
    push_unique_anthropic_beta_feature, DeltaSender, LlmApiMode, LlmCallOptions, LlmRequestPayload,
    LlmRequestPayloadExt, LlmRouteAlternative, LlmRouteFallback, LlmRoutePolicy,
    LlmRoutingDecision, LogprobsConfig, MirostatConfig, OutputFormat, PromptCacheTtl,
    ReasoningEffort, ReminderLifecycleEmission, ResolvedSetting, ThinkingConfig, TokenBias,
    ToolSearchConfig, ToolSearchMode, ToolSearchVariant, Verbosity,
};
#[cfg(test)]
pub(crate) use response::empty_generation_error;
#[cfg(test)]
pub(crate) use response::parse_llm_response as parse_llm_response_for_provider;
pub(crate) use response::{
    extract_cache_counts, parse_openai_responses_response, ProviderResponseEnvelope,
};
#[cfg(test)]
pub(crate) use result::test_text_projection;
pub(crate) use result::{
    build_llm_text_projection, build_usage_value, ensure_llm_text_projection,
    parse_candidate_text_tools, parse_text_tools_with_harn, vm_build_llm_result, LlmResult,
    LlmTextProjection, ProviderAttempts, RawProviderToolCall,
};
pub(crate) use schema_stream::{
    aborted_result_value as schema_stream_aborted_result_value, parse_schema_stream_abort,
    SchemaStreamAbort, StreamSchemaWatch,
};
pub(crate) use telemetry::elapsed_ms;
pub use telemetry::{source as telemetry_source, OllamaPsModel, ProviderTelemetry};
pub(crate) use thinking::{split_openai_thinking_blocks, ThinkingStreamSplitter};
pub(crate) use transport::premature_eof as premature_stream_eof;
pub(crate) use transport::vm_call_llm_api_with_body;

use transport::vm_call_llm_api;

/// Resolve the transport required by a native-tool request.
///
/// An omitted reasoning option preserves the provider's model default. It is
/// not the same as an explicit `effort: "none"` request.
pub(crate) fn effective_tool_api_mode(
    requested: LlmApiMode,
    provider: &str,
    caps: &crate::llm::capabilities::Capabilities,
    thinking: &ThinkingConfig,
    has_native_tools: bool,
) -> LlmApiMode {
    if caps.chat_completions_unsupported
        || (provider == "openai"
            && caps.reasoning_tools_require_responses
            && has_native_tools
            && !matches!(
                thinking,
                ThinkingConfig::Effort {
                    level: ReasoningEffort::None
                }
            ))
    {
        LlmApiMode::Responses
    } else {
        requested
    }
}

/// Send one already-normalized request through Harn's real provider adapter.
/// Provider probes use this boundary so they cannot drift into a second set of
/// endpoint, auth, request, streaming, and response rules.
pub(crate) async fn probe_llm_request(request: &LlmRequestPayload) -> Result<LlmResult, VmError> {
    super::admission::check_auxiliary(None, "provider conformance probes")?;
    vm_call_llm_api(request, None).await
}

#[derive(Debug, Clone)]
struct OffthreadLlmError {
    message: String,
    category: Option<ErrorCategory>,
    stream_failure: Option<Box<crate::value::ProviderStreamFailure>>,
    schema_stream_abort: Option<Box<crate::value::SchemaStreamAbort>>,
    thrown: Option<VmValue>,
}

impl OffthreadLlmError {
    fn from_vm_error(err: VmError) -> Self {
        match err {
            VmError::ProviderStreamFailure(failure) => Self {
                message: failure.to_string(),
                category: Some(failure.category()),
                stream_failure: Some(failure),
                schema_stream_abort: None,
                thrown: None,
            },
            VmError::SchemaStreamAbort(abort) => Self {
                message: abort.to_string(),
                category: Some(abort.category()),
                stream_failure: None,
                schema_stream_abort: Some(abort),
                thrown: None,
            },
            VmError::CategorizedError { message, category } => Self {
                message,
                category: Some(category),
                stream_failure: None,
                schema_stream_abort: None,
                thrown: None,
            },
            VmError::Thrown(VmValue::String(message)) => {
                Self::from_display_message(message.to_string())
            }
            VmError::Thrown(value) => Self {
                message: value.display(),
                category: None,
                stream_failure: None,
                schema_stream_abort: None,
                thrown: Some(value),
            },
            other => Self::from_display_message(other.to_string()),
        }
    }

    fn from_display_message(message: String) -> Self {
        if let Some((category, stripped)) = parse_displayed_categorized_error(&message) {
            return Self {
                message: stripped.to_string(),
                category: Some(category),
                stream_failure: None,
                schema_stream_abort: None,
                thrown: None,
            };
        }
        Self {
            message,
            category: None,
            stream_failure: None,
            schema_stream_abort: None,
            thrown: None,
        }
    }

    fn into_vm_error(self) -> VmError {
        if let Some(failure) = self.stream_failure {
            return VmError::ProviderStreamFailure(failure);
        }
        if let Some(abort) = self.schema_stream_abort {
            return VmError::SchemaStreamAbort(abort);
        }
        match self.category {
            Some(category) => VmError::CategorizedError {
                message: self.message,
                category,
            },
            None => self.thrown.map_or_else(
                || VmError::Thrown(VmValue::string(self.message)),
                VmError::Thrown,
            ),
        }
    }
}

fn parse_displayed_categorized_error(message: &str) -> Option<(ErrorCategory, &str)> {
    let body = message.strip_prefix("Error [")?;
    let (category, rest) = body.split_once("]: ")?;
    Some((ErrorCategory::parse(category), rest))
}

/// Route a logical call when policy is present. The boxed boundary breaks the
/// intentional async cycle: routing executes links through observability, which
/// reaches the explicit single-route primitives after clearing the policy.
fn routed_llm_call<'a>(
    opts: &'a LlmCallOptions,
    delta_tx: Option<DeltaSender>,
) -> Option<impl std::future::Future<Output = Result<LlmResult, VmError>> + 'a> {
    let policy = opts.routing_policy.as_ref()?;
    Some(async move {
        Box::pin(super::routing::execute_with_routing(
            policy,
            opts.clone(),
            None,
            delta_tx,
        ))
        .await
        .map(|(result, _trace)| result)
    })
}

/// Execute a logical LLM call. A configured routing policy runs first; each
/// routed link re-enters the observed single-route path with its policy
/// cleared. Calls without routing enter that same observation boundary
/// directly, so no logical entry point can dispatch an unjournalled provider
/// attempt.
pub(crate) async fn vm_call_llm_full(opts: &LlmCallOptions) -> Result<LlmResult, VmError> {
    if let Some(call) = routed_llm_call(opts, None) {
        return call.await;
    }
    Box::pin(super::agent_observe::observed_llm_call(
        opts, None, None, None, false, false, None, None,
    ))
    .await
}

/// A provider call that completed, with the budget stop its settlement raised.
///
/// The response was already paid for when the ceiling refused it, so the stop
/// travels with the result instead of replacing it. Dropping the result there
/// lost the call from every ledger that a stop is reported from.
pub(crate) struct SettledCall {
    pub(crate) result: LlmResult,
    pub(crate) stop: Option<VmError>,
}

fn settle_completed_call(
    reservation: Option<super::admission::AttemptReservation>,
    result: LlmResult,
) -> SettledCall {
    let admission_result = reservation
        .map(|reservation| reservation.settle(&result))
        .transpose();
    let usage_result = super::cost::record_llm_usage(&result);
    SettledCall {
        stop: admission_result.and(usage_result).err(),
        result,
    }
}

/// The error a settlement stop throws: its usual taxonomy plus the completed
/// call, so the agent loop can record the spend that triggered it.
pub(crate) fn settlement_stop_error(stop: &VmError, result: &LlmResult) -> VmError {
    let VmValue::Dict(dict) =
        super::call::build_llm_error_dict(stop, &result.provider, &result.model)
    else {
        return stop.clone();
    };
    let mut dict = dict.as_ref().clone();
    let mut call = std::collections::BTreeMap::new();
    call.put_str("provider", &result.provider);
    call.put_str("model", &result.model);
    call.put(
        "stop_reason",
        result
            .stop_reason
            .as_deref()
            .map_or(VmValue::Nil, VmValue::string),
    );
    call.put(
        "usage",
        VmValue::dict(result.usage().to_vm_dict(&result.attempts)),
    );
    dict.insert(
        crate::value::intern_key("settled_call"),
        VmValue::dict(call),
    );
    VmError::Thrown(VmValue::dict(dict))
}

/// Execute exactly one prepared provider/model route. Observability calls this
/// primitive after it emits the request receipt. The unforgeable token keeps
/// every other caller on the logical, observed entry points above.
pub(crate) async fn vm_call_llm_full_single_route_prepared(
    observed: &super::agent_observe::ObservedAttemptToken,
    opts: &LlmCallOptions,
    request: &LlmRequestPayload,
) -> Result<SettledCall, VmError> {
    let reservation = super::admission::reserve(opts, request, observed.token_projection())?;
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut first_token = super::first_token::FirstTokenTimer::for_current_span();
    let mut deltas_open = true;
    let mut call = Box::pin(vm_call_llm_full_inner_request(
        observed,
        request,
        Some(delta_tx),
        ProviderExecution::Inline,
    ));
    let result = loop {
        tokio::select! {
            maybe_delta = delta_rx.recv(), if deltas_open => {
                match maybe_delta {
                    Some(_) => first_token.observe_delta(),
                    None => deltas_open = false,
                }
            }
            result = &mut call => break result?,
        }
    };
    while delta_rx.try_recv().is_ok() {
        first_token.observe_delta();
    }
    Ok(settle_completed_call(reservation, result))
}

/// Execute an LLM call, streaming text deltas to `delta_tx`.
pub(crate) async fn vm_call_llm_full_streaming(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    if let Some(call) = routed_llm_call(opts, Some(delta_tx.clone())) {
        return call.await;
    }
    Box::pin(super::agent_observe::observed_llm_call(
        opts,
        None,
        None,
        None,
        false,
        false,
        None,
        Some(delta_tx),
    ))
    .await
}

pub(crate) async fn vm_call_llm_full_streaming_single_route_prepared(
    observed: &super::agent_observe::ObservedAttemptToken,
    opts: &LlmCallOptions,
    request: &LlmRequestPayload,
    delta_tx: DeltaSender,
) -> Result<SettledCall, VmError> {
    let reservation = super::admission::reserve(opts, request, observed.token_projection())?;
    let result = vm_call_llm_full_inner_request(
        observed,
        request,
        Some(delta_tx),
        ProviderExecution::Inline,
    )
    .await?;
    Ok(settle_completed_call(reservation, result))
}

/// Execute provider I/O on Tokio's multithreaded scheduler while keeping
/// VM-local values and transcript assembly on the caller's LocalSet.
#[cfg(test)]
pub(crate) async fn vm_call_llm_full_streaming_offthread(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    if let Some(call) = routed_llm_call(opts, Some(delta_tx.clone())) {
        return call.await;
    }
    Box::pin(super::agent_observe::observed_llm_call(
        opts,
        None,
        None,
        None,
        false,
        true,
        None,
        Some(delta_tx),
    ))
    .await
}

pub(crate) async fn vm_call_llm_full_streaming_offthread_single_route_prepared(
    observed: &super::agent_observe::ObservedAttemptToken,
    opts: &LlmCallOptions,
    request: LlmRequestPayload,
    delta_tx: DeltaSender,
) -> Result<SettledCall, VmError> {
    let reservation = super::admission::reserve(opts, &request, observed.token_projection())?;
    let result = vm_call_llm_full_inner_request(
        observed,
        &request,
        Some(delta_tx),
        ProviderExecution::Offthread,
    )
    .await?;
    Ok(settle_completed_call(reservation, result))
}

#[derive(Clone, Copy)]
enum ProviderExecution {
    Inline,
    Offthread,
}

/// The recording answered this call from a scripted mock, consuming one
/// entry; a served call consumes it too, so a mocked live tail answers from
/// where the recording left off rather than from the script's first entry.
fn consume_served_mock(request: &LlmRequestPayload) {
    if crate::llm::providers::MockProvider::should_intercept_request(request) {
        let _ = mock_llm_response(request);
        let _ = super::mock::take_mock_stream_chunks();
    }
}

/// Project a completed local result without dispatching a provider stream.
fn complete_bypass_result(
    request: &LlmRequestPayload,
    result: LlmResult,
    delta_tx: Option<DeltaSender>,
    chunks: Option<Vec<String>>,
) -> LlmResult {
    record_cli_llm_result(request, &result);
    if let Some(tx) = delta_tx {
        if let Some(chunks) = chunks {
            for chunk in chunks {
                let _ = tx.send(chunk);
            }
        } else if !result.text.is_empty() {
            let _ = tx.send(result.text.clone());
        }
    }
    result
}

async fn vm_call_llm_full_inner_request(
    observed: &super::agent_observe::ObservedAttemptToken,
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
    execution: ProviderExecution,
) -> Result<LlmResult, VmError> {
    if let Some(served) = crate::run_tape::serve_llm_call(request) {
        let served = served?;
        consume_served_mock(request);
        observed.record_provider_dispatch();
        request.emit_reminder_lifecycle();
        return Ok(complete_bypass_result(request, served, delta_tx, None));
    }

    if let Some(result) = super::trigger_predicate::lookup_cached_result(request) {
        request.emit_reminder_lifecycle();
        return Ok(complete_bypass_result(request, result, delta_tx, None));
    }

    if crate::llm::providers::MockProvider::should_intercept_request(request) {
        observed.record_provider_dispatch();
        request.emit_reminder_lifecycle();
        let result = mock_llm_response(request)?;
        super::trigger_predicate::note_result(request, &result);
        let chunks = delta_tx
            .as_ref()
            .and_then(|_| super::mock::take_mock_stream_chunks());
        return Ok(complete_bypass_result(request, result, delta_tx, chunks));
    }

    if crate::llm::fake::FakeLlmProvider::should_intercept(&request.provider) {
        observed.record_provider_dispatch();
        // Bypass fixture/replay so the script-driven fake never collides
        // with HARN_LLM_REPLAY/RECORD being set from an outer harness.
        request.emit_reminder_lifecycle();
        let mut result = crate::llm::fake::FakeLlmProvider
            .chat_impl(request, delta_tx)
            .await?;
        super::mock::mark_live_after_mock_prefix(request, &mut result);
        super::trigger_predicate::note_result(request, &result);
        record_cli_llm_result(request, &result);
        return Ok(result);
    }

    let replay_mode = get_replay_mode();
    let hash = fixture_hash_for_request(request);

    if replay_mode == LlmReplayMode::Replay {
        if let Some(result) = load_fixture(&hash) {
            request.emit_reminder_lifecycle();
            super::trigger_predicate::note_result(request, &result);
            return Ok(complete_bypass_result(request, result, delta_tx, None));
        }
        return Err(VmError::Thrown(VmValue::string(format!(
            "No fixture found for LLM call (hash: {hash}). Run with --record first."
        ))));
    }

    super::ensure_real_llm_allowed(&request.provider)?;
    let boundary_rule = inference_boundary::preflight_chat(request)?;
    request.emit_reminder_lifecycle();
    // Provider/model failover is owned by `routing::execute_with_routing`.
    // This layer executes exactly one route so no attempt can bypass the
    // canonical ledger, quarantine, or exhaustion contract.
    let result = execute_provider_request(
        observed,
        request,
        delta_tx,
        execution,
        boundary_rule.map(str::to_owned),
        super::hash_replay::FixtureRecorder::capture(&hash),
    )
    .await?;
    super::trigger_predicate::note_result(request, &result);

    Ok(result)
}

async fn execute_provider_request(
    observed: &super::agent_observe::ObservedAttemptToken,
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
    execution: ProviderExecution,
    boundary_rule: Option<String>,
    recording: Option<super::hash_replay::FixtureRecorder>,
) -> Result<LlmResult, VmError> {
    if matches!(execution, ProviderExecution::Inline) {
        return dispatch_provider_request(observed, request, delta_tx, boundary_rule, recording)
            .await;
    }
    let request = request.clone();
    let observed = observed.clone();
    let raw_capture_context = crate::llm::agent_observe::current_raw_provider_capture_context();
    let reasoning_receipts = observed.reasoning_receipts();
    tokio::task::spawn(crate::orchestration::scope_inline_subtask(async move {
        super::reasoning_receipt::scope_request_receipts(
            reasoning_receipts,
            Box::pin(async {
                let call = async {
                    dispatch_provider_request(
                        &observed,
                        &request,
                        delta_tx,
                        boundary_rule,
                        recording,
                    )
                    .await
                    .map_err(OffthreadLlmError::from_vm_error)
                };
                if let Some(context) = raw_capture_context {
                    crate::llm::agent_observe::with_raw_provider_capture_context(context, call)
                        .await
                } else {
                    call.await
                }
            }),
        )
        .await
    }))
    .await
    .map_err(|join_err| {
        VmError::Thrown(VmValue::string(format!(
            "llm_call background task failed: {join_err}"
        )))
    })?
    .map_err(OffthreadLlmError::into_vm_error)
}

/// Both inline and offthread requests arrive here after local bypasses.
/// The executing transport owns its permit even if its caller is dropped.
async fn dispatch_provider_request(
    observed: &super::agent_observe::ObservedAttemptToken,
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
    boundary_rule: Option<String>,
    recording: Option<super::hash_replay::FixtureRecorder>,
) -> Result<LlmResult, VmError> {
    let _permit = observed.admit_provider_request().await?;
    observed.record_provider_dispatch();
    let meter = observed.inference_speed().clone();
    meter.start();
    let (delta_tx, text_tee) = match delta_tx {
        Some(tx) => {
            let (tx, tee) = meter.tee_text(tx);
            (Some(tx), Some(tee))
        }
        None => (None, None),
    };
    let outcome =
        super::inference_speed::scope(meter.clone(), Box::pin(vm_call_llm_api(request, delta_tx)))
            .await;
    if let Some(tee) = text_tee {
        // The tee drains once the adapter drops its sender; a sender an
        // adapter leaked must not hold the call open.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), tee).await;
    }
    let mut result = match outcome {
        Ok(result) => result,
        Err(error) => {
            meter.abandon();
            return Err(error);
        }
    };
    result.telemetry.inference_boundary_rule = boundary_rule;
    super::mock::mark_live_after_mock_prefix(request, &mut result);
    if let Some(recording) = recording {
        recording.record(&result);
    }
    meter.finish(&mut result);
    record_cli_llm_result(request, &result);
    Ok(result)
}

#[cfg(test)]
mod model_resolution_tests;
#[cfg(test)]
mod request_shaping_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
pub(crate) mod transport_stub_tests;
