//! OpenAI Responses-compatible provider path.
//!
//! This is intentionally separate from the OpenAI-compatible chat-completions
//! adapter. Responses has first-class hosted tools, provider-managed
//! conversation state, and different output items, so keeping the wire builder
//! distinct prevents compatibility shims from leaking into generic
//! OpenAI-compatible providers.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmRequestPayloadExt, LlmResult};
use crate::value::{VmError, VmValue};

const RESPONSES_ENDPOINT: &str = "/responses";
const RESPONSES_COMPACT_ENDPOINT: &str = "/responses/compact";

pub(crate) struct OpenAiResponsesProvider;

impl OpenAiResponsesProvider {
    pub(crate) async fn call(
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        if request.provider != "openai"
            && request.provider != "mock"
            && !crate::llm_config::provider_has_feature(&request.provider, "responses_api")
        {
            return Err(VmError::Thrown(VmValue::string(format!(
                "api_mode: \"responses\" is not supported by provider \"{}\"",
                request.provider
            ))));
        }

        let clock = harn_clock::RealClock::arc();
        let started_ms = clock.monotonic_ms();
        let compact = request.compact.unwrap_or(false);
        let mut resolved = crate::llm::helpers::ResolvedProvider::resolve(&request.provider);
        resolved.endpoint = if compact {
            RESPONSES_COMPACT_ENDPOINT
        } else {
            RESPONSES_ENDPOINT
        }
        .to_string();
        let boundary =
            crate::llm::api::inference_boundary::effective_result(request.inference_boundary)
                .map_err(VmError::Runtime)?;
        let approved = crate::llm::api::inference_boundary::ApprovedWireRoute::capture(
            boundary,
            &request.provider,
            &resolved.url(),
            &crate::llm_config::wire_model_id(&request.model),
        );
        let mut body = if compact {
            Self::build_compact_request_body(request)
        } else {
            Self::build_request_body(request)
        };
        crate::llm::provider::apply_provider_wire_overrides(
            &mut body,
            request.provider_overrides.as_ref(),
        );
        // Stream when a caller is listening for deltas, so visible text reaches
        // it as the model writes instead of in one piece at the end.
        let stream_text = delta_tx.is_some() && request.stream && !compact;
        if stream_text {
            body["stream"] = serde_json::Value::Bool(true);
        }

        if let Some(approved) = &approved {
            approved.verify(&request.provider, &resolved.url(), &body)?;
        }
        let client =
            crate::llm::blocking_client_for_base_url(&resolved.base_url, approved.as_ref());
        let req = client
            .post(resolved.url())
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .json(&body);
        let req = resolved.apply_headers(req, &request.api_key);
        let response = req.send().await.map_err(|error| {
            crate::llm::api::reqwest_send_error(&request.provider, "Responses", error)
        })?;

        if !response.status().is_success() {
            return Err(crate::llm::api::err_for_non_success(&request.provider, response).await);
        }

        let (json, streamed_text) = match delta_tx.as_ref().filter(|_| stream_text) {
            Some(tx) => read_responses_stream(response, &request.provider, tx).await?,
            None => {
                let json: serde_json::Value = response.json().await.map_err(|error| {
                    VmError::Thrown(VmValue::string(format!(
                        "{} Responses API response parse error: {error}",
                        request.provider
                    )))
                })?;
                (json, false)
            }
        };

        let mut result = crate::llm::api::parse_openai_responses_response(
            &json,
            &request.provider,
            &request.model,
        )?;
        result.telemetry.effective_reasoning_effort =
            crate::llm::EffectiveReasoningEffort::from_responses(&json, &body);
        result
            .telemetry
            .effective_reasoning_effort
            .resolve_source(request.reasoning_effort_source);
        if result.telemetry.client_wall_ms.is_none() {
            result.telemetry.client_wall_ms = Some(elapsed_ms(&*clock, started_ms));
        }
        if result.telemetry.source.is_empty() {
            result.telemetry.source = crate::llm::api::telemetry_source::UNKNOWN.to_string();
        }
        if let Some(tx) = delta_tx {
            // A stream that produced no text deltas (or no stream at all) still
            // hands the listener the visible text once, as before.
            if !streamed_text && !result.text.is_empty() {
                let _ = tx.send(result.text.clone());
            }
        }
        Ok(result)
    }

    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let reasoning_mode = opts
            .reasoning_mode
            .as_deref()
            .filter(|id| *id != crate::llm::reasoning_modes::STANDARD_MODE_ID)
            .and_then(|id| crate::llm::reasoning_modes::lookup(&opts.model, id))
            .filter(crate::llm::reasoning_modes::is_usable);
        let fast_tier = opts
            .fast
            .then(|| crate::llm::serving_tiers::fast_tier(&opts.model))
            .flatten()
            .filter(crate::llm::serving_tiers::is_usable);
        let context = harn_llm::wire::responses::ResponsesWireContext {
            wire_model: &wire_model,
            tool_search_extensions:
                crate::llm::provider::openai_tool_search_wire_extensions_enabled(
                    &opts.provider,
                    &opts.model,
                    opts.provider_tools
                        .iter()
                        .chain(opts.native_tools.iter().flatten()),
                    opts.provider_overrides.as_ref(),
                ),
            reasoning_request: reasoning_mode
                .as_ref()
                .and_then(|mode| mode.request.as_ref()),
            fast_request: fast_tier.as_ref().and_then(|tier| tier.request.as_ref()),
        };
        let body = crate::llm::providers::schema_compat::publish_wire_request(
            harn_llm::wire::responses::encode_request(opts, &context),
        );
        // The Responses API does not lower through `DialectContract`, so it
        // records its own reasoning receipt. Its one reasoning-bearing field
        // is the `reasoning` object, which carries `effort` and the
        // catalog-declared mode knob.
        crate::llm::reasoning_receipt::record(
            &opts.provider,
            &opts.model,
            "openai_responses",
            &opts.thinking,
            &["reasoning"],
            &body,
        );
        body
    }

    pub(crate) fn build_compact_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        harn_llm::wire::responses::encode_compact_request(opts, &wire_model)
    }
}

/// Read a streamed Responses reply: forward each `response.output_text.delta`
/// to `delta_tx` as it arrives, and return the final response object the
/// `response.completed` event carries, which the non-streaming parser reads
/// unchanged. The `bool` says whether any text delta was forwarded.
async fn read_responses_stream(
    response: reqwest::Response,
    provider: &str,
    delta_tx: &DeltaSender,
) -> Result<(serde_json::Value, bool), VmError> {
    use tokio_stream::StreamExt as _;

    let stream = response
        .bytes_stream()
        .map(|chunk| chunk.map_err(std::io::Error::other));
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(stream));
    read_responses_events(reader, provider, delta_tx).await
}

/// The event loop behind [`read_responses_stream`], over any line reader so a
/// test can drive it with a recorded stream.
async fn read_responses_events<R: tokio::io::AsyncBufRead + Unpin>(
    reader: R,
    provider: &str,
    delta_tx: &DeltaSender,
) -> Result<(serde_json::Value, bool), VmError> {
    let interrupted = |detail: String| VmError::CategorizedError {
        message: format!("{provider} Responses stream {detail}"),
        category: crate::value::ErrorCategory::TransientNetwork,
    };
    let mut frames = crate::sse_frames::SseFrames::new(reader);
    let mut streamed_text = false;
    while let Some(frame) = frames
        .next()
        .await
        .map_err(|error| interrupted(format!("read failed: {error}")))?
    {
        let data = frame.data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let event: serde_json::Value = serde_json::from_str(data).map_err(|error| {
            VmError::Thrown(VmValue::string(format!(
                "{provider} Responses stream event parse error: {error}"
            )))
        })?;
        match event.get("type").and_then(serde_json::Value::as_str) {
            Some("response.output_text.delta") => {
                let delta = event
                    .get("delta")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if !delta.is_empty() {
                    streamed_text = true;
                    let _ = delta_tx.send(delta.to_string());
                }
            }
            Some(
                kind @ ("response.reasoning_text.delta"
                | "response.reasoning_summary_text.delta"
                | "response.function_call_arguments.delta"),
            ) => {
                let output = if kind == "response.function_call_arguments.delta" {
                    crate::llm::inference_speed::OutputKind::ToolArguments
                } else {
                    crate::llm::inference_speed::OutputKind::Reasoning
                };
                crate::llm::inference_speed::observe_output(
                    output,
                    event
                        .get("delta")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                );
            }
            Some("response.completed" | "response.incomplete") => {
                let response = event.get("response").cloned().unwrap_or_default();
                return Ok((response, streamed_text));
            }
            Some("response.failed" | "error") => {
                let message = event
                    .pointer("/response/error/message")
                    .or_else(|| event.pointer("/error/message"))
                    .or_else(|| event.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the provider reported a failed response");
                return Err(VmError::Thrown(VmValue::string(format!(
                    "{provider} Responses API error: {message}"
                ))));
            }
            _ => {}
        }
    }
    Err(interrupted(
        "ended before the provider sent response.completed".to_string(),
    ))
}

fn elapsed_ms(clock: &dyn harn_clock::Clock, started_ms: i64) -> u64 {
    clock.monotonic_ms().saturating_sub(started_ms).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{LlmApiMode, LlmRequestPayload, ThinkingConfig};

    /// Send one Responses request to `endpoint` and return its error.
    ///
    /// The endpoint goes through the host-verified runtime override, which is
    /// the supported way to point a provider somewhere for one execution. An
    /// environment variable is not: the catalog decides whether a provider
    /// reads one, and a test that guesses wrong sends a real request to the
    /// real API.
    async fn responses_send_error(endpoint: std::net::SocketAddr) -> VmError {
        let _allow = crate::llm::test_env::ScopedEnvVar::remove(crate::llm::LLM_CALLS_DISABLED_ENV);
        let overrides = crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "openai",
            format!("http://{endpoint}/v1"),
        )
        .expect("a localhost endpoint is a valid override");
        crate::llm_config::set_runtime_provider_endpoint_overrides(overrides);
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.stream = false;
        let result = OpenAiResponsesProvider::call(&LlmRequestPayload::from(&opts), None).await;
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        result.expect_err("a failed transport cannot succeed")
    }

    /// A recorded Responses stream: two text deltas, a reasoning item the
    /// listener must not see, and the completed response the parser reads.
    const RECORDED_RESPONSES_STREAM: &str = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello, \"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"world.\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",",
        "\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",",
        "\"content\":[{\"type\":\"output_text\",\"text\":\"Hello, world.\"}]}]}}\n\n",
    );

    #[tokio::test]
    async fn a_streamed_reply_forwards_each_text_delta_before_the_final_response() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let reader = tokio::io::BufReader::new(RECORDED_RESPONSES_STREAM.as_bytes());
        let (response, streamed) = read_responses_events(reader, "openai", &tx)
            .await
            .expect("a completed stream parses");
        let mut deltas = Vec::new();
        while let Ok(delta) = rx.try_recv() {
            deltas.push(delta);
        }
        assert_eq!(deltas, vec!["Hello, ".to_string(), "world.".to_string()]);
        assert!(streamed);
        assert_eq!(response["status"], "completed");
        let parsed = crate::llm::api::parse_openai_responses_response(&response, "openai", "gpt")
            .expect("the completed response parses like a non-streamed one");
        assert_eq!(parsed.text, "Hello, world.");
    }

    #[tokio::test]
    async fn a_stream_cut_before_completion_is_a_transient_error() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let cut = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n";
        let reader = tokio::io::BufReader::new(cut.as_bytes());
        let error = read_responses_events(reader, "openai", &tx)
            .await
            .expect_err("a stream without response.completed cannot succeed");
        assert_transient_network(&error);
    }

    #[tokio::test]
    async fn a_failed_response_event_is_an_error_naming_the_provider_message() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let failed = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"quota\"}}}\n\n";
        let reader = tokio::io::BufReader::new(failed.as_bytes());
        let error = read_responses_events(reader, "openai", &tx)
            .await
            .expect_err("a failed response cannot succeed");
        assert!(error.to_string().contains("quota"), "got {error}");
    }

    fn assert_transient_network(error: &VmError) {
        match error {
            VmError::CategorizedError { category, message } => assert_eq!(
                *category,
                crate::value::ErrorCategory::TransientNetwork,
                "expected a transient category, got {category:?} for {message}"
            ),
            other => panic!("expected a typed CategorizedError, got {other:?}"),
        }
    }

    /// A refused connection on the real Responses path must arrive as a typed
    /// `CategorizedError` carrying `TransientNetwork`, because that is what the
    /// agent loop's retry predicate reads. The negative control is the path
    /// this replaced, which built a bare string from `redact_reqwest_error` and
    /// classified as `Generic`.
    ///
    /// The port is bound and then dropped, so the address is real and nothing
    /// is listening: the connect is refused immediately.
    #[tokio::test]
    async fn responses_refused_connect_is_typed_transient() {
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            drop(listener);
            addr
        };
        assert_transient_network(&responses_send_error(closed).await);
    }

    /// A server that accepts the connection and closes it before answering.
    /// reqwest reports this as a request-kind error that is not a connect
    /// error, with the same `error sending request for url (...)` text the
    /// production failure carried. The classifier used to file every
    /// request-kind error as a malformed request build and leave it untyped;
    /// a malformed build is reqwest's builder kind, so this is the transport.
    #[tokio::test]
    async fn responses_connection_closed_before_reply_is_typed_transient() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = crate::runtime_stack::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0_u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buffer);
            drop(stream);
        });
        let error = responses_send_error(addr).await;
        server.join().expect("stub thread");
        assert!(
            error.to_string().contains("(send)"),
            "expected the request-kind send branch, got {error}"
        );
        assert_transient_network(&error);
    }

    /// The canonical product path for pro mode: the real Responses body
    /// builder must emit `reasoning.mode` BESIDE the caller's effort, not
    /// instead of it. Anything less means a caller who opted into a
    /// several-times-more-expensive mode either loses their effort rung or
    /// silently pays standard-mode prices for standard-mode work.
    #[test]
    fn responses_body_carries_pro_reasoning_mode_beside_effort() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-5.6-sol".to_string();
        opts.thinking = crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::High,
        };
        opts.reasoning_mode = Some("pro".to_string());
        let body = OpenAiResponsesProvider::build_request_body(&LlmRequestPayload::from(&opts));
        assert_eq!(body["reasoning"]["mode"], "pro");
        assert_eq!(
            body["reasoning"]["effort"], "high",
            "pro mode must not clobber the requested effort rung"
        );
    }

    /// Negative control for the test above. Without it, an implementation
    /// that inserted `mode: "pro"` unconditionally would look identical.
    #[test]
    fn responses_body_omits_reasoning_mode_when_none_requested() {
        for (model, mode) in [
            ("gpt-5.6-sol", None),
            (
                "gpt-5.6-sol",
                Some(crate::llm::reasoning_modes::STANDARD_MODE_ID),
            ),
            ("claude-opus-5", Some("pro")),
        ] {
            let mut opts = crate::llm::api::options::base_opts("openai");
            opts.model = model.to_string();
            opts.reasoning_mode = mode.map(String::from);
            opts.thinking = ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::High,
            };
            let body = OpenAiResponsesProvider::build_request_body(&LlmRequestPayload::from(&opts));
            assert_eq!(body["reasoning"]["effort"], "high");
            assert!(
                body["reasoning"].get("mode").is_none(),
                "unexpected mode for {model}/{mode:?}: {body}"
            );
        }
    }

    /// Pro mode without an explicit rung is the single-shot oracle shape.
    /// `responses_reasoning_config` returns `None` there, so the mode has to
    /// create the `reasoning` object itself rather than being dropped.
    #[test]
    fn responses_body_carries_pro_mode_without_an_explicit_effort() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-5.6-sol".to_string();
        opts.reasoning_mode = Some("pro".to_string());
        let body = OpenAiResponsesProvider::build_request_body(&LlmRequestPayload::from(&opts));
        assert_eq!(body["reasoning"]["mode"], "pro");
    }

    fn tool_search_native_tools() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"type": "tool_search"}),
            serde_json::json!({
                "type": "function",
                "namespace": "ops",
                "defer_loading": true,
                "function": {
                    "name": "deploy",
                    "description": "Deploy the app",
                    "parameters": {
                        "type": "object",
                        "properties": {"env": {"type": "string"}},
                        "required": ["env"],
                    },
                },
            }),
        ]
    }

    #[test]
    fn vercel_responses_body_uses_wire_model_and_gateway_routing_options() {
        let mut opts = crate::llm::api::options::base_opts("vercel_ai_gateway");
        opts.model = "vercel/openai/gpt-5.4-nano".to_string();
        opts.api_mode = LlmApiMode::Responses;
        opts.provider_overrides = Some(serde_json::json!({
            "providerOptions": {
                "gateway": {
                    "sort": "cost",
                    "models": ["google/gemini-3.1-flash-lite"]
                }
            }
        }));
        let payload = LlmRequestPayload::from(&opts);

        let mut body = OpenAiResponsesProvider::build_request_body(&payload);
        crate::llm::provider::apply_provider_wire_overrides(
            &mut body,
            payload.provider_overrides.as_ref(),
        );

        assert_eq!(body["model"], "openai/gpt-5.4-nano");
        assert_eq!(body["providerOptions"]["gateway"]["sort"], "cost");
        assert_eq!(
            body["providerOptions"]["gateway"]["models"][0],
            "google/gemini-3.1-flash-lite"
        );
    }

    #[test]
    fn responses_strict_route_omits_tool_search_extensions() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-5.3".to_string();
        opts.api_mode = LlmApiMode::Responses;
        opts.native_tools = Some(vec![tool_search_native_tools()
            .into_iter()
            .nth(1)
            .expect("function tool")]);
        let payload = LlmRequestPayload::from(&opts);

        let body = OpenAiResponsesProvider::build_request_body(&payload);

        assert_eq!(body["tools"][0]["name"], "deploy");
        assert!(body["tools"][0].get("namespace").is_none());
        assert!(body["tools"][0].get("defer_loading").is_none());
        let source = &payload.native_tools.as_ref().expect("source tools")[0];
        assert_eq!(source["namespace"], "ops");
        assert_eq!(source["defer_loading"], true);
    }

    #[test]
    fn responses_capable_tool_search_request_keeps_extensions() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-5.4".to_string();
        opts.api_mode = LlmApiMode::Responses;
        opts.native_tools = Some(tool_search_native_tools());
        let payload = LlmRequestPayload::from(&opts);

        let body = OpenAiResponsesProvider::build_request_body(&payload);

        assert_eq!(body["tools"][0], serde_json::json!({"type": "tool_search"}));
        assert_eq!(body["tools"][1]["name"], "deploy");
        assert!(body["tools"][1].get("namespace").is_none());
        assert_eq!(body["tools"][1]["defer_loading"], true);
    }

    #[test]
    fn responses_forced_native_search_keeps_extensions_for_unknown_model() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "my-internal-model".to_string();
        opts.api_mode = LlmApiMode::Responses;
        opts.native_tools = Some(tool_search_native_tools());
        opts.provider_overrides = Some(serde_json::json!({"force_native_tool_search": true}));
        let payload = LlmRequestPayload::from(&opts);

        let body = OpenAiResponsesProvider::build_request_body(&payload);

        assert_eq!(body["tools"][0]["type"], "tool_search");
        assert!(body["tools"][1].get("namespace").is_none());
        assert_eq!(body["tools"][1]["defer_loading"], true);
    }
}
