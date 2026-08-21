use super::*;

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
            result.map_err(std::io::Error::other)
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

/// Shared reqwest-`Error`-kind classifier for both the streaming and
/// non-streaming send paths. Maps the reqwest kind to an explicit
/// [`crate::value::ErrorCategory`] (carried on a `CategorizedError`) so the
/// retry/observability layer reads a typed category rather than re-deriving it
/// from the message text. `phase` ("stream" / "request") only flavors the
/// human-readable message; the typed category drives retry decisions.
pub(super) fn reqwest_send_error(provider: &str, phase: &str, error: reqwest::Error) -> VmError {
    use crate::value::ErrorCategory;
    let (kind, category) = if error.is_timeout() {
        ("timeout", Some(ErrorCategory::Timeout))
    } else if error.is_connect() {
        ("connect", Some(ErrorCategory::TransientNetwork))
    } else if error.is_request() {
        // A malformed request build is the caller's fault, not a transient
        // network blip — leave it uncategorized so it is not blindly retried.
        ("request_build", None)
    } else if error.is_body() {
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
        None => VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message))),
    }
}

/// Canonical wire id for a streamed tool call.
///
/// Streaming announcements and executed lifecycle events are emitted
/// separately, but consumers join them by `tool_call_id`. Use the
/// provider id verbatim whenever it is present. Only synthesize a
/// fallback when the provider sent no id, and then write that fallback
/// into the dispatched call's `id` so `__tool_envelope` continues the
/// lifecycle on the same wire id.
///
/// The fallback carries a UUID because it becomes the dispatch id; a
/// bare per-stream index would collide across loop iterations.
fn streaming_tool_call_id(provider_id: &str, fallback_index: usize) -> String {
    if provider_id.is_empty() {
        format!("stream-tool-{fallback_index}-{}", uuid::Uuid::now_v7())
    } else {
        provider_id.to_string()
    }
}

fn preview_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Finalize the accumulated Anthropic `input_json_delta` buffer for a streamed
/// `tool_use` block into dispatchable arguments.
///
/// An EMPTY buffer is a legitimate no-args call — a tool invoked with no
/// arguments streams zero `input_json_delta` events (the `content_block_start`
/// carries `"input": {}` and nothing follows) — so it maps to `{}`, never to a
/// parse error. A NON-empty buffer that fails to parse (malformed or truncated
/// accumulated JSON) must NOT silently dispatch the tool with empty arguments;
/// it becomes the same `{"__parse_error": "... Raw input: <raw>"}` carrier the
/// OpenAI streaming path and the non-streaming parser build, so the agent
/// loop's recoverable invalid-arguments feedback path asks the model to
/// re-issue the call instead of running the tool with arguments it never chose.
fn parse_anthropic_streamed_tool_input(input_json: &str) -> serde_json::Value {
    if input_json.trim().is_empty() {
        return serde_json::Value::Object(Default::default());
    }
    match serde_json::from_str::<serde_json::Value>(input_json) {
        Ok(value) => value,
        Err(json_error) => serde_json::json!({
            "__parse_error": format!(
                "Could not parse streamed tool arguments as JSON: {}. Raw input: {}",
                json_error,
                preview_chars(input_json, 200)
            )
        }),
    }
}

fn parse_openai_streamed_tool_argument_values(
    tool_name: &str,
    arguments: &str,
    stop_reason: Option<&str>,
) -> Vec<serde_json::Value> {
    if arguments.trim().is_empty() {
        return vec![serde_json::json!({})];
    }
    match parse_openai_tool_argument_json_values(arguments) {
        Ok(values) => values,
        Err(json_error) => {
            if crate::llm::agent_session_host::is_length_truncation(stop_reason) {
                return vec![serde_json::json!({})];
            }
            vec![
                crate::llm::tools::parse_text_tool_argument_payload(arguments, tool_name)
                    .unwrap_or_else(|text_error| {
                        serde_json::json!({
                        "__parse_error": format!(
                            "Could not parse streamed tool arguments as JSON or Harn text-tool arguments: JSON error: {}; Harn text-tool error: {}. Raw input: {}",
                            json_error,
                            text_error,
                            preview_chars(arguments, 200)
                        )
                    })
                    }),
            ]
        }
    }
}

fn streamed_native_tool_name_text_call_parse_error(
    raw_name: &str,
    error: &str,
) -> serde_json::Value {
    serde_json::json!({
        "__parse_error": format!(
            "Could not parse streamed provider tool name as Harn text-tool call: {}. Raw input: {}",
            error,
            preview_chars(raw_name, 200)
        )
    })
}

fn streamed_native_tool_arguments_text_call_parse_error(
    raw_arguments: &str,
    error: &str,
) -> serde_json::Value {
    serde_json::json!({
        "__parse_error": format!(
            "Could not parse streamed provider tool arguments as Harn text-tool call: {}. Raw input: {}",
            error,
            preview_chars(raw_arguments, 200)
        )
    })
}

fn push_internal_tool_call(
    tool_calls: &mut Vec<serde_json::Value>,
    blocks: &mut Vec<serde_json::Value>,
    id: String,
    name: String,
    arguments: serde_json::Value,
) {
    tool_calls.push(serde_json::json!({
        "id": id,
        "name": name,
        "arguments": arguments,
    }));
    blocks.push(serde_json::json!({
        "type": "tool_call",
        "id": id,
        "name": name,
        "arguments": arguments,
        "visibility": "internal",
    }));
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
) -> Result<LlmResult, VmError> {
    use tokio::io::AsyncBufReadExt;
    let mut lines = reader.lines();
    let mut liveness = super::liveness::StreamLiveness::new(provider, deadline_policy);

    let mut text = String::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut raw_tool_calls: Vec<RawProviderToolCall> = Vec::new();
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let mut telemetry = ProviderTelemetry::default();
    let mut anth_request_id: Option<String> = None;
    // Set once the provider echoes the fast-mode knob (`speed` /
    // `service_tier`) on a streamed event; drives premium-tier billing.
    let mut served_fast = false;

    struct ToolBlock {
        name: String,
        input_json: String,
        provider_id: String,
        /// Stable id used for `AgentEvent::ToolCall*` streaming
        /// emissions and as the dispatched call's `id`.
        /// `__tool_envelope` reuses this id for the executed
        /// lifecycle, so clients can correlate streaming progress with
        /// the eventual outcome.
        tool_call_id: String,
        /// Concrete canonical name already published for this lifecycle.
        /// Generic provider wrappers stay unannounced until their streamed
        /// arguments reveal the real tool.
        announced_tool_name: Option<String>,
        /// Coalescing gate so a tool that arrives in 30 small deltas
        /// emits ~6 `ToolCallUpdate` events instead of 30.
        coalescer: DeltaCoalescer,
    }
    let mut current_tool: Option<ToolBlock> = None;
    // Mirror structure for server-side tool-search queries: Anthropic
    // streams the query JSON the same way as a regular tool_use, but we
    // route it to a `tool_search_query` transcript event instead of the
    // dispatchable `tool_calls` vector.
    struct ServerToolBlock {
        id: String,
        name: String,
        input_json: String,
    }
    let mut current_server_tool: Option<ServerToolBlock> = None;
    let mut thinking_text = String::new();
    let mut current_thinking: Option<crate::llm::reasoning_history::AnthropicThinkingBlock> = None;
    let mut stop_reason: Option<String> = None;
    let mut cache_read_tokens: i64 = 0;
    let mut cache_write_tokens: i64 = 0;
    // Counter for fallback streaming-tool-call ids when a provider sent
    // an empty id on the first tool_use block. Kept stable across the
    // stream so the coalesced updates reuse the same id the dispatcher
    // would compute.
    let mut anth_tool_block_index: usize = 0;
    let mut stream_tool_closeout = StreamToolCallCloseout::new(session_id);

    /// Per-tool-call OpenAI streaming state. Tracks the accumulated
    /// arguments string, the tool name (filled when the first delta
    /// carries `function.name`), the canonical `tool_call_id` used for
    /// both `AgentEvent::ToolCall*` emission and the dispatched call's
    /// `id`, whether the initial `ToolCall(Pending)` event has fired
    /// yet, and a coalescer so argument-delta storms don't fan out
    /// per-byte.
    struct OaiToolStream {
        /// Raw provider-sent id ("" until/unless a delta carries one).
        /// Only consulted to decide whether a late-arriving id may
        /// still be adopted into `tool_call_id`.
        id: String,
        name: String,
        args: String,
        tool_call_id: String,
        announced_tool_name: Option<String>,
        coalescer: DeltaCoalescer,
    }
    let mut oai_tool_map: std::collections::HashMap<u64, OaiToolStream> =
        std::collections::HashMap::new();
    // Qwen3/3.5 via vLLM emit inline `<think>...</think>`. Strip these
    // out of the visible delta stream so the tool-call parser / progress
    // UI only see the real response.
    let mut oai_thinking_splitter = ThinkingStreamSplitter::new();
    // Most recent SSE `event:` name. Consumed with the next `data:` frame so
    // named `event: error` payloads classify before delta parsing.
    let mut current_event: Option<String> = None;
    let managed_transport = crate::llm::managed_supply::is_managed_transport(provider);
    let awaits_stream_usage = dialect.awaits_stream_usage(provider);

    loop {
        let line = match liveness.next_line(lines.next_line()).await? {
            Some(line) => line,
            None => return Err(liveness.premature_eof("a provider terminal event")),
        };
        if let Some(event) = line.strip_prefix("event:") {
            current_event = Some(event.trim().to_string());
            continue;
        }
        let data = if let Some(d) = line.strip_prefix("data: ") {
            d
        } else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let json: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => {
                // Drop a stale event name so a later data frame cannot inherit
                // an earlier `event:` label after a malformed payload.
                current_event = None;
                continue;
            }
        };
        let event_name = current_event.take();
        if is_structured_sse_error_frame(event_name.as_deref(), &json, dialect.stream_protocol()) {
            // Flush any OpenAI-compat visible carry held for `<think>` boundary
            // detection so short partial text still counts as partial output.
            let pending_visible = oai_thinking_splitter.flush();
            if !pending_visible.is_empty() {
                text.push_str(&pending_visible);
                let _ = delta_tx.send(pending_visible);
            }
            return Err(super::super::classify_provider_stream_error(
                provider,
                data,
                !text.is_empty(),
            ));
        }
        liveness.mark_partial_output();
        let managed_receipt_frame = managed_transport
            && json
                .get(crate::llm::managed_supply::MANAGED_SUPPLY_WIRE_KEY)
                .is_some_and(|receipt| !receipt.is_null());
        let terminal_frame = if dialect.stream_protocol() == StreamProtocol::AnthropicSse {
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

        if dialect.stream_protocol() == StreamProtocol::AnthropicSse {
            match json["type"].as_str() {
                Some("message_start") => {
                    if let Some(n) = json["message"]["usage"]["input_tokens"].as_i64() {
                        input_tokens = n;
                    }
                    served_fast |= crate::llm::serving_tiers::served_fast(model, &json["message"]);
                    let usage = &json["message"]["usage"];
                    let cr = extract_cache_read_tokens(usage);
                    if cr > 0 {
                        cache_read_tokens = cr;
                    }
                    let cw = extract_cache_write_tokens(usage);
                    if cw > 0 {
                        cache_write_tokens = cw;
                    }
                    if let Some(rid) = json["message"]["id"].as_str() {
                        if !rid.is_empty() {
                            anth_request_id = Some(rid.to_string());
                        }
                    }
                }
                Some("content_block_start") => {
                    let block = &json["content_block"];
                    match block["type"].as_str() {
                        Some("tool_use") => {
                            let id = block["id"].as_str().unwrap_or("").to_string();
                            let name = block["name"].as_str().unwrap_or("").to_string();
                            anth_tool_block_index += 1;
                            let tool_call_id = streaming_tool_call_id(&id, anth_tool_block_index);
                            let announced_tool_name = try_announce_stream_tool_call(
                                session_id,
                                &tool_call_id,
                                &name,
                                serde_json::Value::Object(Default::default()),
                                &mut stream_tool_closeout,
                            );
                            current_tool = Some(ToolBlock {
                                name,
                                input_json: String::new(),
                                provider_id: id,
                                tool_call_id,
                                announced_tool_name,
                                coalescer: DeltaCoalescer::new(),
                            });
                        }
                        Some("server_tool_use") => {
                            current_server_tool = Some(ServerToolBlock {
                                id: block["id"].as_str().unwrap_or("").to_string(),
                                name: block["name"].as_str().unwrap_or("").to_string(),
                                input_json: String::new(),
                            });
                        }
                        Some("tool_search_tool_result") => {
                            // Non-streaming content: Anthropic embeds the
                            // references directly in the block_start
                            // payload. Record immediately — no deltas
                            // follow for this block type.
                            let refs: Vec<serde_json::Value> = block["content"]["tool_references"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default();
                            blocks.push(serde_json::json!({
                                "type": "tool_search_result",
                                "tool_use_id": block["tool_use_id"].clone(),
                                "tool_references": refs,
                                "visibility": "internal",
                            }));
                        }
                        Some("thinking") => {
                            current_thinking = Some(
                                crate::llm::reasoning_history::AnthropicThinkingBlock::from_start(
                                    block,
                                ),
                            );
                        }
                        Some("redacted_thinking") => {
                            if let Some(block) =
                                crate::llm::reasoning_history::capture_anthropic_block(block)
                            {
                                blocks.push(block);
                            }
                        }
                        _ => {}
                    }
                }
                Some("content_block_delta") => {
                    let delta = &json["delta"];
                    match delta["type"].as_str() {
                        Some("text_delta") => {
                            if let Some(t) = delta["text"].as_str() {
                                text.push_str(t);
                                let _ = delta_tx.send(t.to_string());
                                blocks.push(serde_json::json!({"type": "output_text", "text": t, "visibility": "public"}));
                                if let Some(watch) = schema_watch.as_mut() {
                                    if let Some(abort) = watch.observe(t) {
                                        return Err(abort.into_vm_error());
                                    }
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(t) = delta["thinking"].as_str() {
                                thinking_text.push_str(t);
                                if let Some(thinking) = current_thinking.as_mut() {
                                    thinking.push_thinking(t);
                                }
                            }
                        }
                        Some("signature_delta") => {
                            if let (Some(signature), Some(thinking)) =
                                (delta["signature"].as_str(), current_thinking.as_mut())
                            {
                                thinking.push_signature(signature);
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(ref mut tool) = current_tool {
                                if let Some(j) = delta["partial_json"].as_str() {
                                    tool.input_json.push_str(j);
                                }
                                if tool.announced_tool_name.is_none() {
                                    let partial = project_partial(&tool.input_json);
                                    if let Some(raw_input) = partial.value {
                                        tool.announced_tool_name = try_announce_stream_tool_call(
                                            session_id,
                                            &tool.tool_call_id,
                                            &tool.name,
                                            raw_input,
                                            &mut stream_tool_closeout,
                                        );
                                    }
                                }
                                if let Some(event_tool_name) = tool.announced_tool_name.as_deref() {
                                    try_emit_partial_tool_args(
                                        session_id,
                                        &tool.tool_call_id,
                                        &tool.name,
                                        event_tool_name,
                                        &tool.input_json,
                                        &mut tool.coalescer,
                                        Instant::now(),
                                    );
                                }
                            } else if let Some(ref mut server_tool) = current_server_tool {
                                if let Some(j) = delta["partial_json"].as_str() {
                                    server_tool.input_json.push_str(j);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some("content_block_stop") => {
                    if let Some(tool) = current_tool.take() {
                        let args = parse_anthropic_streamed_tool_input(&tool.input_json);
                        raw_tool_calls.push(
                            RawProviderToolCall::new(serde_json::json!({
                                "type": "tool_use",
                                "id": tool.provider_id,
                                "name": tool.name,
                                "input": args.clone(),
                            }))
                            .expect("anthropic stream raw tool call is an object"),
                        );
                        let (name, args) =
                            crate::llm::tools::normalize_tool_call_shape(&tool.name, args);
                        // Dispatch under the id already used for
                        // streaming progress so the executed lifecycle
                        // continues on the same wire id.
                        tool_calls.push(serde_json::json!({
                            "id": tool.tool_call_id, "name": name, "arguments": args,
                        }));
                        blocks.push(serde_json::json!({"type": "tool_call", "id": tool.tool_call_id, "name": name, "arguments": args, "visibility": "internal"}));
                    } else if let Some(server_tool) = current_server_tool.take() {
                        // Emit a `tool_search_query` transcript event —
                        // not dispatchable, just observability.
                        let query =
                            serde_json::from_str::<serde_json::Value>(&server_tool.input_json)
                                .unwrap_or(serde_json::Value::Object(Default::default()));
                        blocks.push(serde_json::json!({
                            "type": "tool_search_query",
                            "id": server_tool.id,
                            "name": server_tool.name,
                            "query": query,
                            "visibility": "internal",
                        }));
                    } else if let Some(thinking) = current_thinking.take() {
                        blocks.push(thinking.finish());
                    }
                }
                Some("message_delta") => {
                    if let Some(n) = json["usage"]["output_tokens"].as_i64() {
                        output_tokens = n;
                    }
                    let usage = &json["usage"];
                    let cr = extract_cache_read_tokens(usage);
                    if cr > 0 {
                        cache_read_tokens = cr;
                    }
                    let cw = extract_cache_write_tokens(usage);
                    if cw > 0 {
                        cache_write_tokens = cw;
                    }
                    if let Some(sr) = json["delta"]["stop_reason"].as_str() {
                        stop_reason = Some(sr.to_string());
                    }
                }
                _ => {}
            }
        } else {
            let choice = &json["choices"][0];
            let delta = &choice["delta"];

            if let Some(content) = delta["content"].as_str() {
                let visible = oai_thinking_splitter.push(content);
                if !visible.is_empty() {
                    text.push_str(&visible);
                    let _ = delta_tx.send(visible.clone());
                    blocks.push(serde_json::json!({"type": "output_text", "text": visible, "visibility": "public"}));
                    if let Some(watch) = schema_watch.as_mut() {
                        if let Some(abort) = watch.observe(&visible) {
                            return Err(abort.into_vm_error());
                        }
                    }
                }
            }
            // Streaming deltas for `reasoning` (Ollama OpenAI-compat,
            // OpenRouter passthrough), `reasoning_content` (DashScope,
            // Together), and `reasoning_details` (MiniMax) arrive as
            // token-sized fragments — `"Here"`,
            // `"'s"`, `" a"`, `" thinking"`. Concatenate them verbatim;
            // `extract_openai_message_field_as_text` + `append_paragraph`
            // would `.trim()` each fragment (losing inter-token spaces)
            // and inject a newline between every chunk, producing
            // one-token-per-line reasoning text like
            // `"The\ntask\nis\nto\nextend"`. The non-streaming response
            // path still uses `append_paragraph` because there each
            // field arrives as a single complete block.
            let reasoning_delta = extract_openai_delta_field_str(
                delta,
                &["reasoning", "reasoning_content", "reasoning_details"],
            );
            if !reasoning_delta.is_empty() {
                thinking_text.push_str(reasoning_delta);
                blocks.push(serde_json::json!({"type": "reasoning", "text": reasoning_delta, "visibility": "private"}));
            }

            // Only capture finish_reason once; OpenRouter can send
            // duplicates (qwen-code#2402) that truncate in-progress tool
            // calls.
            if stop_reason.is_none() {
                if let Some(fr) = choice["finish_reason"].as_str() {
                    stop_reason = Some(fr.to_string());
                }
            }

            if let Some(tcs) = delta["tool_calls"].as_array() {
                for tc in tcs {
                    // OpenAI Responses-API server-side tool_search
                    // (harn#71) streams as `tool_search_call` /
                    // `tool_search_output` entries in the tool_calls
                    // array. Record them as transcript events, never
                    // as dispatchable calls.
                    let tc_type = tc["type"].as_str().unwrap_or("");
                    if tc_type == "tool_search_call" {
                        let id = tc["id"].as_str().unwrap_or("").to_string();
                        let query = tc.get("query").cloned().unwrap_or_else(|| {
                            tc.get("input").cloned().unwrap_or(serde_json::Value::Null)
                        });
                        blocks.push(serde_json::json!({
                            "type": "tool_search_query",
                            "id": id,
                            "name": "tool_search",
                            "query": query,
                            "visibility": "internal",
                        }));
                        continue;
                    }
                    if tc_type == "tool_search_output" {
                        let tool_use_id = tc["call_id"]
                            .as_str()
                            .or_else(|| tc["id"].as_str())
                            .unwrap_or("")
                            .to_string();
                        let references = tc["tool_references"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        blocks.push(serde_json::json!({
                            "type": "tool_search_result",
                            "tool_use_id": tool_use_id,
                            "tool_references": references,
                            "visibility": "internal",
                        }));
                        continue;
                    }
                    let idx = tc["index"].as_u64().unwrap_or(0);
                    let stream_index = idx as usize + 1;
                    let entry = oai_tool_map.entry(idx).or_insert_with(|| {
                        let id = tc["id"].as_str().unwrap_or("").to_string();
                        let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                        let tool_call_id = streaming_tool_call_id(&id, stream_index);
                        OaiToolStream {
                            id,
                            name,
                            args: String::new(),
                            tool_call_id,
                            announced_tool_name: None,
                            coalescer: DeltaCoalescer::new(),
                        }
                    });
                    // OpenAI sometimes splits the metadata across deltas:
                    // the first one carries `name`, later ones carry only
                    // `arguments`. Patch missing fields if a later delta
                    // fills them in.
                    if entry.id.is_empty() {
                        if let Some(id) = tc["id"].as_str() {
                            if !id.is_empty() {
                                entry.id = id.to_string();
                                // Adopt a late-arriving provider id only
                                // while nothing has been emitted under
                                // the fallback yet. Once announced, the
                                // published id stays canonical for both
                                // the remaining updates and dispatch.
                                if entry.announced_tool_name.is_none() {
                                    entry.tool_call_id =
                                        streaming_tool_call_id(&entry.id, stream_index);
                                }
                            }
                        }
                    }
                    if entry.name.is_empty() {
                        if let Some(name) = tc["function"]["name"].as_str() {
                            if !name.is_empty() {
                                entry.name = name.to_string();
                            }
                        }
                    }
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        entry.args.push_str(args);
                    }
                    // Real names announce immediately. Generic provider
                    // wrappers wait until streamed arguments reveal the
                    // canonical inner tool, so no lifecycle starts under a
                    // `tool_call`/`tool` placeholder name.
                    if entry.announced_tool_name.is_none() && !entry.name.is_empty() {
                        let raw_input = project_partial(&entry.args)
                            .value
                            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
                        entry.announced_tool_name = try_announce_stream_tool_call(
                            session_id,
                            &entry.tool_call_id,
                            &entry.name,
                            raw_input,
                            &mut stream_tool_closeout,
                        );
                    }
                    if let Some(event_tool_name) = entry.announced_tool_name.as_deref() {
                        try_emit_partial_tool_args(
                            session_id,
                            &entry.tool_call_id,
                            &entry.name,
                            event_tool_name,
                            &entry.args,
                            &mut entry.coalescer,
                            Instant::now(),
                        );
                    }
                }
            }

            served_fast |= crate::llm::serving_tiers::served_fast(model, &json);
            if let Some(usage) = json.get("usage") {
                if let Some(n) = usage["prompt_tokens"].as_i64() {
                    input_tokens = n;
                }
                if let Some(n) = usage["completion_tokens"].as_i64() {
                    output_tokens = n;
                }
                let cr = extract_cache_read_tokens(usage);
                if cr > 0 {
                    cache_read_tokens = cr;
                }
                let cw = extract_cache_write_tokens(usage);
                if cw > 0 {
                    cache_write_tokens = cw;
                }
                let request_id = json
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty());
                // The usage frame replaces the envelope wholesale, but the
                // serving fingerprint is stream-scoped: a server that
                // announces it only on the opening chunk would otherwise have
                // it erased by the frame carrying the counters. Re-capture
                // below still lets this frame's own value win.
                let carried_fingerprint = telemetry.serving_fingerprint.take();
                telemetry = ProviderTelemetry::from_openai_usage(usage, request_id);
                telemetry.serving_fingerprint = carried_fingerprint;
            }
            telemetry.capture_provider_metadata(&json);
        }
        // A finish_reason ends content, not necessarily accounting. Providers
        // with a declared stream-usage contract and managed gateways append
        // their authoritative receipt in later empty-choices frames.
        if managed_receipt_frame || (terminal_frame && !managed_transport && !awaits_stream_usage) {
            break;
        }
    }

    for (_, stream) in oai_tool_map {
        raw_tool_calls.push(
            RawProviderToolCall::new(serde_json::json!({
                "id": stream.id.clone(),
                "type": "function",
                "function": {
                    "name": stream.name.clone(),
                    "arguments": stream.args.clone(),
                },
            }))
            .expect("openai stream raw tool call is an object"),
        );
        match crate::llm::tools::parse_text_tool_call_from_native_name(&stream.name) {
            crate::llm::tools::NativeToolNameTextCall::Parsed { name, arguments } => {
                let (name, arguments) =
                    crate::llm::tools::normalize_tool_call_shape(&name, arguments);
                let id = stream.tool_call_id;
                push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, arguments);
                continue;
            }
            crate::llm::tools::NativeToolNameTextCall::Malformed { name, error } => {
                let arguments =
                    streamed_native_tool_name_text_call_parse_error(&stream.name, &error);
                let (name, arguments) =
                    crate::llm::tools::normalize_tool_call_shape(&name, arguments);
                let id = stream.tool_call_id;
                push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, arguments);
                continue;
            }
            crate::llm::tools::NativeToolNameTextCall::NotCall => {}
        }
        // Keep streaming and non-streaming OpenAI-compatible parsing in
        // parity. Some routes put Harn's complete text call in the native
        // wrapper's arguments (`tool_call` + `search({...})`). Parsing only
        // the argument object discards the inner name and sends the wrapper
        // itself to the tool ceiling.
        if crate::llm::tools::is_generic_wrapper_name(&stream.name) {
            match crate::llm::tools::parse_text_tool_call_from_native_arguments(&stream.args) {
                crate::llm::tools::NativeToolNameTextCall::Parsed { name, arguments } => {
                    let (name, arguments) =
                        crate::llm::tools::normalize_tool_call_shape(&name, arguments);
                    let id = stream.tool_call_id;
                    push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, arguments);
                    continue;
                }
                crate::llm::tools::NativeToolNameTextCall::Malformed { name, error } => {
                    let arguments =
                        streamed_native_tool_arguments_text_call_parse_error(&stream.args, &error);
                    let (name, arguments) =
                        crate::llm::tools::normalize_tool_call_shape(&name, arguments);
                    let id = stream.tool_call_id;
                    push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, arguments);
                    continue;
                }
                crate::llm::tools::NativeToolNameTextCall::NotCall => {}
            }
        }
        let args_values = parse_openai_streamed_tool_argument_values(
            &stream.name,
            &stream.args,
            stop_reason.as_deref(),
        );
        let base_tool_call_id = stream.tool_call_id;
        // Dispatch under the id already used for streaming progress so
        // the executed lifecycle continues on the same wire id. If a
        // provider packed multiple top-level JSON objects into one streamed
        // arguments string, split them into synthetic sibling calls matching
        // the non-streaming parser's semantics.
        for (arg_index, args) in args_values.into_iter().enumerate() {
            let id = if arg_index == 0 {
                base_tool_call_id.clone()
            } else {
                format!("{}_{}", base_tool_call_id, arg_index + 1)
            };
            let (name, args) = crate::llm::tools::normalize_tool_call_shape(&stream.name, args);
            push_internal_tool_call(&mut tool_calls, &mut blocks, id, name, args);
        }
    }

    let final_visible = oai_thinking_splitter.flush();
    if !final_visible.is_empty() {
        text.push_str(&final_visible);
        let _ = delta_tx.send(final_visible.clone());
        blocks.push(serde_json::json!({"type": "output_text", "text": final_visible, "visibility": "public"}));
        if let Some(watch) = schema_watch.as_mut() {
            if let Some(abort) = watch.observe(&final_visible) {
                return Err(abort.into_vm_error());
            }
        }
    }
    if !oai_thinking_splitter.thinking.is_empty() {
        append_paragraph(&mut thinking_text, &oai_thinking_splitter.thinking);
    }

    // When the stream is cut off mid-thought (finish_reason == "length") with
    // no committed visible content, the reasoning trace is a partial, garbage
    // not-an-answer. Promoting it into `.text` surfaces that garbage as the
    // final answer. Leave `text` empty and expose the partial trace only via
    // `thinking`, mirroring `openai_normalize::normalize_openai_message_text`.
    let truncated = stop_reason.as_deref() == Some("length");
    // When the turn also carries a tool call, or tools were offered for this
    // turn, the reasoning is intermediate chain-of-thought or hidden action
    // planning, not committed final answer text.
    // gpt-oss / harmony models stream their analysis channel into the reasoning
    // delta and emit a tool call with no committed content; promoting that
    // reasoning into `.text` leaks private chain-of-thought into the user-facing
    // assistant message AND the transcript the eval grader mines. Keep `.text`
    // empty and surface the reasoning only via `thinking`, mirroring
    // `openai_normalize::normalize_openai_message_text`.
    let caps = crate::llm::managed_supply::capabilities_for(provider, model);
    if caps.reasoning_text_promotable
        && !truncated
        && !tools_offered
        && tool_calls.is_empty()
        && text.is_empty()
        && !thinking_text.is_empty()
    {
        text = thinking_text.clone();
        blocks
            .push(serde_json::json!({"type": "output_text", "text": text, "visibility": "public"}));
    }
    let has_tool_search_block = blocks.iter().any(|b| {
        matches!(
            b.get("type").and_then(|v| v.as_str()),
            Some("tool_search_query") | Some("tool_search_result")
        )
    });
    if text.is_empty()
        && thinking_text.is_empty()
        && output_tokens > 0
        && tool_calls.is_empty()
        && !has_tool_search_block
    {
        // Name the ACTUAL wire style, not a hardcoded "openai-compatible".
        // This streaming parser handles BOTH the native Anthropic SSE shape
        // and the OpenAI-compatible shape selected by the contract, so a
        // native Anthropic empty-completion flake used to be mislabeled as
        // "openai-compatible model", which sent a real root-cause hunt down
        // the wrong (transport-routing) path. The `provider` id is the ground
        // truth; the wire style disambiguates native vs. compat.
        let wire_style = if dialect.stream_protocol() == StreamProtocol::AnthropicSse {
            "anthropic-native"
        } else {
            "openai-compatible"
        };
        return Err(empty_generation_error(
            provider,
            model,
            Some(output_tokens),
            format!(
                "{wire_style} model {provider}:{model} reported completion_tokens={output_tokens} but delivered no content, reasoning, or tool calls"
            ),
        ));
    }
    // Deterministic upstream contract-violation backstop (streaming path).
    // Mirrors the non-streaming detector in `response::parse_llm_response`: a
    // clean, tool-offered turn that billed output but committed no visible text
    // and dispatched no tool call is a billed no-op (the action went only to a
    // hidden reasoning channel or nowhere).
    if is_billed_noncommittal_completion(&CompletionContractSignals {
        stop_reason: stop_reason.as_deref(),
        output_tokens,
        tools_offered,
        tool_call_count: tool_calls.len(),
        has_tool_search_block,
        text: &text,
    }) {
        return Err(billed_noncommittal_completion_error(
            provider,
            model,
            output_tokens,
        ));
    }
    stream_tool_closeout.finish_success(&tool_calls);

    // Use the caller-supplied provider id rather than collapsing every
    // non-anthropic stream to "openai". The provider name shows up in the
    // observability transcript (`agent_observe::dump_llm_response`) and is
    // load-bearing for downstream classifiers (e.g. honors_chat_template_kwargs
    // routing in capability lookup) — collapsing it to "openai" hides which
    // OpenAI-compatible server (vLLM, llama.cpp, OpenRouter, llamacpp) the
    // call actually went to. Anthropic's classic SSE shape still implies
    // provider="anthropic" because the wire protocol is anthropic-specific
    // even when the configured provider name disagrees (proxies / mocks).
    let result_provider = if dialect.stream_protocol() == StreamProtocol::AnthropicSse {
        "anthropic".to_string()
    } else {
        provider.to_string()
    };
    if telemetry.is_empty()
        && dialect.stream_protocol() == StreamProtocol::AnthropicSse
        && (input_tokens > 0 || output_tokens > 0)
    {
        let usage = serde_json::json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        });
        telemetry = ProviderTelemetry::from_anthropic_usage(&usage, anth_request_id.as_deref());
    }
    telemetry.capture_request_id(provider_request_id);
    Ok(LlmResult {
        attempts: Default::default(),
        text_projection: None,
        text,
        tool_calls,
        raw_tool_calls,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cache_supported: true,
        model: model.to_string(),
        provider: result_provider,
        thinking: if thinking_text.is_empty() {
            None
        } else {
            Some(thinking_text)
        },
        thinking_summary: None,
        stop_reason,
        served_fast,
        blocks,
        logprobs: Vec::new(),
        telemetry,
    })
}
