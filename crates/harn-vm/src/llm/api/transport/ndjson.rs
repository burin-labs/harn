use super::*;

/// Consume an NDJSON streaming response, forwarding text deltas to `delta_tx`
/// while accumulating the full result.
///
/// Supports Ollama format (one JSON object per line):
/// `{"message":{"role":"assistant","content":"Hi"},"done":false}`
/// Final line has `"done":true` with token counts.
///
/// Also supports OpenAI-compatible NDJSON where each line is `data: {...}`.
pub(super) async fn vm_call_llm_api_ndjson_from_response(
    response: reqwest::Response,
    provider: &str,
    model: &str,
    delta_tx: DeltaSender,
    unload_grace: Duration,
    warmup_gate: &mut bool,
    schema_watch: Option<super::super::schema_stream::StreamSchemaWatch>,
    raw_capture: RawProviderCaptureTarget,
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
    let result = consume_ollama_ndjson_lines(
        reader,
        provider,
        model,
        delta_tx,
        unload_grace,
        warmup_gate,
        schema_watch,
    )
    .await;
    if let Some(raw_bytes) = raw_bytes {
        crate::llm::agent_observe::persist_raw_provider_response(
            raw_capture.context(),
            provider,
            model,
            "ndjson",
            raw_capture.attempt,
            status.as_u16(),
            content_type.as_deref(),
            &captured_stream_text(&raw_bytes),
        );
    }
    result
}

pub(super) async fn consume_ollama_ndjson_lines<R>(
    reader: R,
    provider: &str,
    model: &str,
    delta_tx: DeltaSender,
    unload_grace: Duration,
    warmup_gate: &mut bool,
    mut schema_watch: Option<super::super::schema_stream::StreamSchemaWatch>,
) -> Result<LlmResult, VmError>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;

    let mut lines = reader.lines();

    let mut text = String::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;
    let mut result_model = model.to_string();

    let mut thinking_text = String::new();
    let mut tool_calls = Vec::new();
    let mut blocks = Vec::new();
    let mut saw_done = false;
    let mut saw_chunk = false;
    let mut done_reason: Option<String> = None;
    let mut telemetry = ProviderTelemetry::default();

    loop {
        let line = next_ollama_ndjson_line(&mut lines, model, unload_grace, warmup_gate)
            .await
            .map_err(|error| {
                let error = crate::egress::redact_diagnostic_text(&error.to_string());
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "ollama stream error: unexpected EOF or read failure before done=true: {error}"
                ))))
            })?;
        let Some(line) = line else {
            break;
        };
        *warmup_gate = true;
        if line.is_empty() {
            continue;
        }
        let data = line.strip_prefix("data: ").unwrap_or(&line);
        if data == "[DONE]" {
            break;
        }
        let json: serde_json::Value = serde_json::from_str(data).map_err(|error| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "ollama stream parse error: partial or invalid NDJSON frame before done=true: {error}; line={}",
                &data[..data.len().min(200)]
            ))))
        })?;
        saw_chunk = true;

        // Ollama streams content and thinking as separate channels for
        // reasoning-capable models (gemma3/4, qwen3, etc.); we always set
        // `think: true` so thinking tokens aren't dropped.
        let content = json["message"]["content"].as_str().unwrap_or("");
        let thinking = json["message"]["thinking"].as_str().unwrap_or("");
        if !content.is_empty() {
            text.push_str(content);
            let _ = delta_tx.send(content.to_string());
            blocks.push(
                serde_json::json!({"type": "output_text", "text": content, "visibility": "public"}),
            );
            if let Some(watch) = schema_watch.as_mut() {
                if let Some(abort) = watch.observe(content) {
                    return Err(abort.into_vm_error());
                }
            }
        } else if !thinking.is_empty() {
            thinking_text.push_str(thinking);
            blocks.push(
                serde_json::json!({"type": "reasoning", "text": thinking, "visibility": "private"}),
            );
        }
        append_ollama_tool_calls(&json["message"], &mut tool_calls, &mut blocks);

        if let Some(m) = json["model"].as_str() {
            result_model = m.to_string();
        }

        if json["done"].as_bool() == Some(true) {
            if let Some(n) = json["prompt_eval_count"].as_i64() {
                input_tokens = n;
            }
            if let Some(n) = json["eval_count"].as_i64() {
                output_tokens = n;
            }
            // Capture Ollama's `done_reason` so length-truncation is visible on
            // the most-used local chat path. Without this the agent never learns
            // it was cut off at the token cap.
            done_reason = json["done_reason"].as_str().map(str::to_string);
            telemetry = ProviderTelemetry::from_ollama_done(&json, telemetry_source::OLLAMA_CHAT);
            saw_done = true;
            break;
        }
    }

    if !saw_done {
        let suffix = if saw_chunk {
            " after partial content"
        } else {
            " before any chunks"
        };
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("ollama stream error: unexpected EOF before done=true{suffix}"),
        ))));
    }

    let thinking = if thinking_text.is_empty() {
        None
    } else {
        Some(thinking_text.clone())
    };

    // Guard against upstream parser bugs reporting generated tokens with
    // no visible content. Observed with `gemma4:26b` + ollama's
    // server-side `PARSER gemma4` on tool-heavy prompts: eval_count is
    // nonzero but every delta and the done chunk are empty strings.
    // Silently returning empty text would make the agent loop burn
    // iterations on a no-op.
    //
    // BUT: `done_reason == "length"` is a deterministic token-cap
    // truncation, not a parser bug. Re-running it just re-truncates, so we
    // must NOT raise the retryable parser-bug error here. Surface the empty
    // result carrying `stop_reason: Some("length")` so the caller sees a
    // non-retryable truncation signal instead. The parser-bug error stays
    // reserved for `done_reason` == stop/absent.
    if text.is_empty()
        && tool_calls.is_empty()
        && output_tokens > 0
        && done_reason.as_deref() != Some("length")
    {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "ollama model {model} reported eval_count={output_tokens} but delivered no visible content or tool calls [ollama_empty_content_parser_bug]"
        )))));
    }

    Ok(LlmResult {
        text,
        raw_tool_calls: Vec::new(),
        tool_calls,
        input_tokens,
        output_tokens,
        // Native Ollama `/api/chat` NDJSON done frames carry no cache field;
        // 0 is "unknown", not a real miss. The `/v1` shim on these hosts also
        // omits `prompt_tokens_details`, so routing to `/v1` is not a fix.
        // `cache_supported: false` keeps cost/telemetry from scoring a local
        // model as a 100% cache miss.
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: false,
        model: result_model,
        // NDJSON is currently only consumed for Ollama's `/api/chat`, but
        // pass the caller-supplied provider through anyway so the result
        // matches `opts.provider` exactly. Future engines that adopt
        // NDJSON streaming (some llama.cpp builds, mlx-vlm) will get the
        // right label without additional plumbing.
        provider: provider.to_string(),
        thinking,
        thinking_summary: None,
        stop_reason: done_reason,
        // NDJSON (Ollama) has no accelerated-serving tier.
        served_fast: false,
        blocks,
        logprobs: Vec::new(),
        telemetry,
    })
}

async fn next_ollama_ndjson_line<R>(
    lines: &mut tokio::io::Lines<R>,
    model: &str,
    unload_grace: Duration,
    warmup_progress_sent: &mut bool,
) -> std::io::Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    if *warmup_progress_sent || unload_grace.is_zero() {
        return lines.next_line().await;
    }

    tokio::select! {
        line = lines.next_line() => line,
        _ = tokio::time::sleep(unload_grace) => {
            *warmup_progress_sent = true;
            emit_ollama_warmup_progress(model);
            lines.next_line().await
        }
    }
}

pub(super) fn emit_ollama_warmup_progress(model: &str) {
    let message = format!("warming up Ollama model {model}");
    if let Some(bridge) = crate::llm::current_host_bridge() {
        bridge.send_progress(
            "llm",
            &message,
            None,
            None,
            Some(serde_json::json!({
                "provider": "ollama",
                "model": model,
                "reason": "model_unload",
            })),
        );
    }
    crate::events::log_info("llm", &message);
}

pub(super) fn is_ollama_empty_content_parser_bug(err: &VmError) -> bool {
    match err {
        VmError::Thrown(VmValue::String(message)) => {
            message.contains("[ollama_empty_content_parser_bug]")
        }
        VmError::Runtime(message) => message.contains("[ollama_empty_content_parser_bug]"),
        _ => false,
    }
}
