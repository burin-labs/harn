use super::stream_fold::{StreamFold, StreamPart};
use super::*;
use crate::llm::inference_speed::{observe_output, OutputKind};
use crate::llm::usage::ProviderUsageReceipt;

/// Consume an SSE streaming response from an already-sent request.
/// Parses `data: {...}` lines from the response body, then defers to
/// [`consume_sse_lines`] for the parsing and event-emission logic so
/// tests can drive the same code path against an in-memory `AsyncBufRead`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn vm_call_llm_api_sse_from_response(
    response: reqwest::Response,
    provider: &str,
    model: &str,
    dialect: DialectContract,
    delta_tx: DeltaSender,
    session_id: Option<&str>,
    schema_watch: Option<super::super::schema_stream::StreamSchemaWatch>,
    tools_offered: bool,
    deadline_policy: super::liveness::StreamDeadlinePolicy,
    raw_capture: RawProviderCaptureTarget,
    provider_request_id: Option<&str>,
    request_origin: tokio::time::Instant,
) -> Result<LlmResult, VmError> {
    use tokio_stream::StreamExt;

    let status = response.status();
    let content_type = response_content_type(&response);
    let raw_bytes = raw_capture
        .enabled()
        .then(|| Arc::new(Mutex::new(Vec::new())));
    let stream_capture = raw_bytes.clone();
    let stream = response.bytes_stream();
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(stream.map(
        move |result| {
            if let Ok(bytes) = &result {
                capture_stream_bytes(stream_capture.as_ref(), bytes);
            }
            result.map_err(stream_body_read_error)
        },
    )));
    let result = consume_sse_lines_with_policy(
        reader,
        provider,
        model,
        dialect,
        delta_tx,
        session_id,
        schema_watch,
        tools_offered,
        deadline_policy,
        provider_request_id,
        request_origin,
    )
    .await;
    if let Some(raw_bytes) = raw_bytes {
        crate::llm::agent_observe::persist_raw_provider_response(
            raw_capture.context(),
            provider,
            model,
            "sse",
            raw_capture.attempt,
            status.as_u16(),
            content_type.as_deref(),
            &captured_stream_text(&raw_bytes),
        );
    }
    result
}

/// Try to publish the live `(tool_call_id, tool_name, accumulated_bytes)`
/// triple as a `ToolCallUpdate(Pending, raw_input | raw_input_partial)`
/// event. Coalescing + partial-parse logic lives here so both the
/// Anthropic and OpenAI branches of the SSE loop share one emit site.
fn try_emit_partial_tool_args(
    session_id: Option<&str>,
    tool_call_id: &str,
    raw_tool_name: &str,
    announced_tool_name: &str,
    accumulated: &str,
    coalescer: &mut DeltaCoalescer,
    now: Instant,
) {
    let Some(session_id) = session_id else {
        return;
    };
    if !coalescer.should_emit(now) {
        return;
    }
    let PartialToolArgs {
        mut value,
        raw_partial,
    } = project_partial(accumulated);
    if value.is_none() && raw_partial.is_none() {
        return;
    }
    if crate::llm::tools::is_generic_wrapper_name(raw_tool_name) {
        if let Some(raw_input) = value.take() {
            let (_, normalized_input) = canonical_stream_event_tool_call(raw_tool_name, raw_input);
            value = Some(normalized_input);
        }
    }
    let event = AgentEvent::ToolCallUpdate {
        session_id: session_id.to_string(),
        tool_call_id: tool_call_id.to_string(),
        tool_name: announced_tool_name.to_string(),
        status: ToolCallStatus::Pending,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        health: None,
        executor: None,
        raw_input: value,
        raw_input_partial: raw_partial,
        audit: crate::orchestration::current_mutation_session(),

        parsing: None,
    };
    crate::llm::agent_runtime::emit_agent_event_sync(&event);
}

fn canonical_stream_event_tool_call(
    tool_name: &str,
    arguments: serde_json::Value,
) -> (String, serde_json::Value) {
    let tool_name = match crate::llm::tools::parse_text_tool_call_from_native_name(tool_name) {
        crate::llm::tools::NativeToolNameTextCall::Parsed {
            name,
            arguments: embedded_arguments,
        } => return crate::llm::tools::normalize_tool_call_shape(&name, embedded_arguments),
        crate::llm::tools::NativeToolNameTextCall::Malformed { name, .. } => name,
        crate::llm::tools::NativeToolNameTextCall::NotCall => tool_name.to_string(),
    };
    crate::llm::tools::normalize_tool_call_shape(&tool_name, arguments)
}

fn resolved_stream_event_tool_call(
    tool_name: &str,
    arguments: serde_json::Value,
) -> Option<(String, serde_json::Value)> {
    let (name, arguments) = canonical_stream_event_tool_call(tool_name, arguments);
    if name.trim().is_empty() || crate::llm::tools::is_generic_wrapper_name(&name) {
        return None;
    }
    Some((name, arguments))
}

fn emit_stream_tool_call_start(
    session_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    raw_input: serde_json::Value,
) {
    let tool_kind = crate::orchestration::current_tool_annotations(tool_name)
        .map(|annotations| annotations.kind);
    crate::llm::agent_runtime::emit_agent_event_sync(&AgentEvent::ToolCall {
        session_id: session_id.to_string(),
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        kind: tool_kind,
        status: ToolCallStatus::Pending,
        raw_input,
        audit: crate::orchestration::current_mutation_session(),
        intent: None,
        parsing: None,
    });
}

fn try_announce_stream_tool_call(
    session_id: Option<&str>,
    tool_call_id: &str,
    raw_tool_name: &str,
    raw_input: serde_json::Value,
    closeout: &mut StreamToolCallCloseout,
) -> Option<String> {
    let (tool_name, raw_input) = resolved_stream_event_tool_call(raw_tool_name, raw_input)?;
    if let Some(session_id) = session_id {
        emit_stream_tool_call_start(session_id, tool_call_id, &tool_name, raw_input);
        closeout.announce(tool_call_id, &tool_name);
    }
    Some(tool_name)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AnnouncedStreamToolCall {
    tool_call_id: String,
    tool_name: String,
}

struct StreamToolCallCloseout {
    session_id: Option<String>,
    announced: Vec<AnnouncedStreamToolCall>,
    disarmed: bool,
}

impl StreamToolCallCloseout {
    fn new(session_id: Option<&str>) -> Self {
        Self {
            session_id: session_id.map(str::to_string),
            announced: Vec::new(),
            disarmed: false,
        }
    }

    fn announce(&mut self, tool_call_id: &str, tool_name: &str) {
        if self.session_id.is_none() {
            return;
        }
        let id = tool_call_id.trim();
        if id.is_empty() {
            return;
        }
        let name = tool_name.trim();
        if let Some(existing) = self
            .announced
            .iter_mut()
            .find(|entry| entry.tool_call_id == id)
        {
            if !name.is_empty() {
                existing.tool_name = name.to_string();
            }
            return;
        }
        self.announced.push(AnnouncedStreamToolCall {
            tool_call_id: id.to_string(),
            tool_name: name.to_string(),
        });
    }

    fn finish_success(&mut self, returned_tool_calls: &[serde_json::Value]) {
        let Some(session_id) = self.session_id.clone() else {
            self.announced.clear();
            self.disarmed = true;
            return;
        };
        for announcement in self.announced.drain(..) {
            if returned_tool_calls.iter().any(|call| {
                call.get("id").and_then(serde_json::Value::as_str)
                    == Some(announcement.tool_call_id.as_str())
            }) {
                continue;
            }
            emit_stream_tool_call_closeout(&session_id, &announcement);
        }
        self.disarmed = true;
    }

    fn fail_all(&mut self) {
        let Some(session_id) = self.session_id.clone() else {
            self.announced.clear();
            return;
        };
        for announcement in self.announced.drain(..) {
            emit_stream_tool_call_closeout(&session_id, &announcement);
        }
    }
}

impl Drop for StreamToolCallCloseout {
    fn drop(&mut self) {
        if !self.disarmed {
            self.fail_all();
        }
    }
}

fn emit_stream_tool_call_closeout(session_id: &str, announcement: &AnnouncedStreamToolCall) {
    let event = AgentEvent::ToolCallUpdate {
        session_id: session_id.to_string(),
        tool_call_id: announcement.tool_call_id.clone(),
        tool_name: announcement.tool_name.clone(),
        status: ToolCallStatus::Failed,
        raw_output: None,
        error: Some(
            "provider stream ended before this announced tool call reached dispatch".to_string(),
        ),
        duration_ms: None,
        execution_duration_ms: None,
        error_category: Some(ToolCallErrorCategory::ParseAborted),
        mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        health: None,
        executor: None,
        raw_input: None,
        raw_input_partial: None,
        audit: crate::orchestration::current_mutation_session(),
        parsing: None,
    };
    crate::llm::agent_runtime::emit_agent_event_sync(&event);
}

pub(super) async fn send_stream_request_with_ollama_warmup(
    req: reqwest::RequestBuilder,
    provider: &str,
    model: &str,
    protocol: StreamProtocol,
    unload_grace: Duration,
    warmup_gate: &mut bool,
) -> Result<reqwest::Response, VmError> {
    let send = req.send();
    if protocol != StreamProtocol::OllamaNdjson || *warmup_gate || unload_grace.is_zero() {
        return send
            .await
            .map_err(|error| stream_send_error(provider, error));
    }

    tokio::pin!(send);
    tokio::select! {
        response = &mut send => response.map_err(|error| stream_send_error(provider, error)),
        _ = tokio::time::sleep(unload_grace) => {
            *warmup_gate = true;
            emit_ollama_warmup_progress(model);
            send.await.map_err(|error| stream_send_error(provider, error))
        }
    }
}

fn stream_send_error(provider: &str, error: reqwest::Error) -> VmError {
    reqwest_send_error(provider, "stream", error)
}

/// Classify a non-streaming `req.send()` transport failure with the *same*
/// reqwest-kind classifier the streaming path uses, so timeouts and connection
/// failures are typed (`ErrorCategory::Timeout` / `TransientNetwork`) at the
/// source instead of being reclassified downstream by fragile substring
/// matching of a bare `"{provider} API error: {e}"` string.
pub(super) fn non_stream_send_error(provider: &str, error: reqwest::Error) -> VmError {
    reqwest_send_error(provider, "request", error)
}

/// Classify a non-streaming response-body read failure through the same typed
/// reqwest-kind boundary as request dispatch. A timeout remains a timeout;
/// another body failure is a transient network error rather than an untyped
/// response-parse string.
pub(super) fn non_stream_body_error(provider: &str, error: reqwest::Error) -> VmError {
    reqwest_send_error(provider, "response body", error)
}

/// The one way a `reqwest::Error` becomes a `VmError` on an LLM path: streaming
/// sends, non-streaming sends, provider-specific sends, and response-body reads.
/// Maps the reqwest kind to an explicit [`crate::value::ErrorCategory`] (carried
/// on a `CategorizedError`) so the retry/observability layer reads a typed
/// category rather than re-deriving it from the message text.
///
/// The message cannot carry that decision. reqwest's `Display` for every
/// transport failure is `error sending request for url (...)`; whether it was a
/// refused connect, a DNS failure or a reset lives in the source chain, which
/// `redact_reqwest_error` drops. `phase` only flavors the human-readable
/// message; the typed category drives retry decisions.
pub(crate) fn reqwest_send_error(provider: &str, phase: &str, error: reqwest::Error) -> VmError {
    use crate::value::ErrorCategory;
    let (kind, category) = if error.is_timeout() {
        ("timeout", Some(ErrorCategory::Timeout))
    } else if error.is_connect() {
        ("connect", Some(ErrorCategory::TransientNetwork))
    } else if error.is_builder() {
        // A malformed request build is the caller's fault, not a transient
        // network blip — leave it uncategorized so it is not blindly retried.
        ("request_build", None)
    } else if error.is_request() {
        // reqwest wraps every failure of the HTTP client future in this kind:
        // a connection closed before the response, a reset mid-write, an HTTP/2
        // GOAWAY. `is_builder` is where a malformed request lands, so what is
        // left here is the transport, and the transport is transient.
        ("send", Some(ErrorCategory::TransientNetwork))
    } else if error.is_body() || error.is_decode() {
        // `Response::text()` drains bytes through reqwest's decode wrapper, so
        // a connection reset or truncated body reports `is_decode()` even
        // though no application-level parsing has started yet.
        ("body", Some(ErrorCategory::TransientNetwork))
    } else {
        ("unknown", None)
    };
    // "unknown" uses Debug repr to surface the inner cause.
    let error = if kind == "unknown" {
        crate::egress::redact_diagnostic_text(&format!("{error:?}"))
    } else {
        crate::egress::redact_reqwest_error(&error)
    };
    let message = format!("{provider} {phase} error ({kind}): {error}");
    match category {
        Some(category) => VmError::CategorizedError { message, category },
        None => VmError::Thrown(VmValue::string(message)),
    }
}

/// True when an SSE `data:` JSON frame is a terminal provider error rather than
/// a completion/delta chunk. Named `event: error` frames always qualify; otherwise
/// recognize Anthropic `type=error` and OpenAI-compatible / managed top-level
/// `error` payloads that lack a usable `choices` array.
fn is_structured_sse_error_frame(
    event_name: Option<&str>,
    json: &serde_json::Value,
    protocol: StreamProtocol,
) -> bool {
    if event_name.is_some_and(|name| name.eq_ignore_ascii_case("error")) {
        return true;
    }
    if json.get("type").and_then(serde_json::Value::as_str) == Some("error") {
        return true;
    }
    let Some(error) = json.get("error") else {
        return false;
    };
    if error.is_null() {
        return false;
    }
    // Managed OpenAI-compatible transports often stamp taxonomy beside `error`.
    if json
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .is_some()
        && json
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .is_some()
    {
        return true;
    }
    match protocol {
        StreamProtocol::OpenAiSse => json
            .get("choices")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|choices| choices.is_empty()),
        // Anthropic errors use `type=error` (handled above). A bare `error`
        // field on other Anthropic event types is not a recognized shape.
        StreamProtocol::AnthropicSse
        | StreamProtocol::OllamaNdjson
        | StreamProtocol::GeminiJson
        | StreamProtocol::GeminiInteractionsSse => false,
    }
}

fn choices_are_empty(json: &serde_json::Value) -> bool {
    json.get("choices")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty)
}

/// Pure SSE-line consumer used by the response wrapper and by tests
/// that drive canned byte streams without standing up a full
/// `reqwest::Response`. The Anthropic / OpenAI branches and the
/// trailing accumulator drain that finalize the call live here.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) async fn consume_sse_lines<R: tokio::io::AsyncBufRead + Unpin>(
    reader: R,
    provider: &str,
    model: &str,
    dialect: DialectContract,
    delta_tx: DeltaSender,
    session_id: Option<&str>,
    schema_watch: Option<super::super::schema_stream::StreamSchemaWatch>,
    tools_offered: bool,
) -> Result<LlmResult, VmError> {
    consume_sse_lines_with_policy(
        reader,
        provider,
        model,
        dialect,
        delta_tx,
        session_id,
        schema_watch,
        tools_offered,
        super::liveness::StreamDeadlinePolicy {
            total: Duration::from_hours(1),
            first_chunk: Duration::from_hours(1),
            idle: Duration::from_hours(1),
        },
        None,
        tokio::time::Instant::now(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn consume_sse_lines_with_policy<R: tokio::io::AsyncBufRead + Unpin>(
    reader: R,
    provider: &str,
    model: &str,
    dialect: DialectContract,
    delta_tx: DeltaSender,
    session_id: Option<&str>,
    mut schema_watch: Option<super::super::schema_stream::StreamSchemaWatch>,
    tools_offered: bool,
    deadline_policy: super::liveness::StreamDeadlinePolicy,
    provider_request_id: Option<&str>,
    request_origin: tokio::time::Instant,
) -> Result<LlmResult, VmError> {
    let mut frames = crate::sse_frames::SseFrames::new(reader);
    let mut liveness =
        super::liveness::StreamLiveness::new(provider, deadline_policy, request_origin);
    let protocol = dialect.stream_protocol();
    let mut fold = StreamFold::new(protocol);
    let mut progress = ToolCallProgress::new(session_id);
    // OpenAI-compatible routes may stream inline `<think>...</think>`; only
    // the visible remainder is forwarded live. The folded body keeps the raw
    // content and the shared parser splits it the same way.
    let mut live_text = ThinkingStreamSplitter::new();
    // Per tool slot, how many argument bytes the inference meter has seen.
    let mut metered_tool_arguments: Vec<usize> = Vec::new();
    let mut forwarded_text = false;
    let managed_transport = crate::llm::managed_supply::is_managed_transport(provider);
    let awaits_stream_usage = dialect.awaits_stream_usage(provider);

    loop {
        let frame = match liveness.next_item(frames.next()).await? {
            Some(frame) => frame,
            None => return Err(liveness.premature_eof("a provider terminal event")),
        };
        let data = frame.data.as_str();
        if data == "[DONE]" {
            break;
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        // A named `event: error` frame classifies before delta parsing.
        let event_name = frame.event;
        if is_structured_sse_error_frame(event_name.as_deref(), &json, protocol) {
            // Flush visible text held for `<think>` boundary detection so a
            // short partial answer still counts as partial output.
            let pending_visible = live_text.flush();
            if !pending_visible.is_empty() {
                forwarded_text = true;
                let _ = delta_tx.send(pending_visible);
            }
            return Err(super::super::classify_provider_stream_error(
                provider,
                data,
                forwarded_text,
            ));
        }
        liveness.mark_partial_output();
        liveness.mark_first_frame();
        for part in fold.observe(&json) {
            match part {
                StreamPart::Text(raw) => {
                    let visible = if protocol == StreamProtocol::AnthropicSse {
                        raw
                    } else {
                        let hidden_before = live_text.thinking.len();
                        let visible = live_text.push(&raw);
                        observe_output(
                            OutputKind::Reasoning,
                            live_text.thinking.get(hidden_before..).unwrap_or_default(),
                        );
                        visible
                    };
                    forward_text(&visible, &delta_tx, &mut schema_watch, &mut forwarded_text)?;
                }
                StreamPart::Reasoning(text) => observe_output(OutputKind::Reasoning, &text),
                StreamPart::ToolCall(slot) => {
                    let metered = metered_tool_arguments.get(slot).copied().unwrap_or(0);
                    let arguments = &fold.tool(slot).arguments;
                    observe_output(
                        OutputKind::ToolArguments,
                        arguments.get(metered..).unwrap_or_default(),
                    );
                    if metered_tool_arguments.len() <= slot {
                        metered_tool_arguments.resize(slot + 1, 0);
                    }
                    metered_tool_arguments[slot] = arguments.len();
                    progress.observe(&mut fold, slot);
                }
            }
        }
        let managed_receipt_frame = managed_transport
            && json
                .get(crate::llm::managed_supply::MANAGED_SUPPLY_WIRE_KEY)
                .is_some_and(|receipt| !receipt.is_null());
        let terminal_frame = if protocol == StreamProtocol::AnthropicSse {
            json["type"].as_str() == Some("message_stop")
        } else {
            json["choices"].as_array().is_some_and(|choices| {
                choices.iter().any(|choice| {
                    choice
                        .get("finish_reason")
                        .is_some_and(|reason| !reason.is_null())
                })
            })
        };
        let terminal_usage_receipt = awaits_stream_usage
            && choices_are_empty(&json)
            && json
                .get("usage")
                .and_then(ProviderUsageReceipt::from_openai_usage_tokens)
                .as_ref()
                .is_some_and(ProviderUsageReceipt::has_any_reported_token_count);
        // A finish_reason ends content, not necessarily accounting. Providers
        // with a declared stream-usage contract and managed gateways append
        // their authoritative receipt in later empty-choices frames.
        if managed_receipt_frame
            || terminal_usage_receipt
            || (terminal_frame && !managed_transport && !awaits_stream_usage)
        {
            break;
        }
    }

    let final_visible = live_text.flush();
    forward_text(
        &final_visible,
        &delta_tx,
        &mut schema_watch,
        &mut forwarded_text,
    )?;

    let wire = if protocol == StreamProtocol::AnthropicSse {
        crate::llm::capabilities::WireDialect::Anthropic
    } else {
        crate::llm::capabilities::WireDialect::OpenAiCompat
    };
    let mut result = super::super::response::parse_llm_response(
        &fold.into_body(),
        provider,
        model,
        wire,
        tools_offered,
    )?;
    progress.finish_success(&result.tool_calls);
    result.telemetry.capture_request_id(provider_request_id);
    result.telemetry.client_first_frame_ms = liveness.first_frame_ms();
    Ok(result)
}

/// Forward visible text to the live channel and the structured-output watch.
fn forward_text(
    visible: &str,
    delta_tx: &DeltaSender,
    schema_watch: &mut Option<super::super::schema_stream::StreamSchemaWatch>,
    forwarded_text: &mut bool,
) -> Result<(), VmError> {
    if visible.is_empty() {
        return Ok(());
    }
    *forwarded_text = true;
    let _ = delta_tx.send(visible.to_string());
    if let Some(abort) = schema_watch
        .as_mut()
        .and_then(|watch| watch.observe(visible))
    {
        return Err(abort.into_vm_error());
    }
    Ok(())
}

/// Live progress for streamed tool calls: one `ToolCall(Pending)` announcement
/// per call under a concrete tool name, then coalesced partial-argument
/// updates, and a failed closeout for any announced call the finished
/// response does not dispatch.
struct ToolCallProgress {
    session_id: Option<String>,
    /// Per fold slot: the announced canonical name and the update coalescer.
    calls: Vec<(Option<String>, DeltaCoalescer)>,
    closeout: StreamToolCallCloseout,
}

impl ToolCallProgress {
    fn new(session_id: Option<&str>) -> Self {
        Self {
            session_id: session_id.map(str::to_string),
            calls: Vec::new(),
            closeout: StreamToolCallCloseout::new(session_id),
        }
    }

    fn observe(&mut self, fold: &mut StreamFold, slot: usize) {
        while self.calls.len() <= slot {
            self.calls.push((None, DeltaCoalescer::new()));
        }
        let session_id = self.session_id.as_deref();
        let tool = fold.tool(slot);
        let (announced, coalescer) = &mut self.calls[slot];
        // Real names announce immediately. Generic provider wrappers wait
        // until streamed arguments reveal the canonical inner tool, so no
        // lifecycle starts under a `tool_call`/`tool` placeholder name.
        if announced.is_none() && !tool.name.is_empty() {
            let raw_input = project_partial(&tool.arguments)
                .value
                .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
            *announced = try_announce_stream_tool_call(
                session_id,
                &tool.tool_call_id,
                &tool.name,
                raw_input,
                &mut self.closeout,
            );
        }
        let Some(event_tool_name) = announced.as_deref() else {
            return;
        };
        // Arguments only start the coalescing window once they exist, so the
        // announcement itself never spends it.
        if !tool.arguments.is_empty() {
            try_emit_partial_tool_args(
                session_id,
                &tool.tool_call_id,
                &tool.name,
                event_tool_name,
                &tool.arguments,
                coalescer,
                Instant::now(),
            );
        }
        fold.publish(slot);
    }

    fn finish_success(&mut self, returned_tool_calls: &[serde_json::Value]) {
        self.closeout.finish_success(returned_tool_calls);
    }
}
