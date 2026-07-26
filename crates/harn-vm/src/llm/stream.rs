use crate::value::{VmError, VmValue};

use super::api::LlmCallOptions;
use super::helpers::ResolvedProvider;

pub(crate) async fn vm_stream_llm(
    opts: &LlmCallOptions,
    tx: &tokio::sync::mpsc::Sender<VmValue>,
) -> Result<(), VmError> {
    use reqwest_eventsource::{Event, EventSource};
    use tokio_stream::StreamExt;

    let provider = &opts.provider;
    super::ensure_real_llm_allowed(provider)?;

    let resolved = ResolvedProvider::resolve(provider);
    let client = super::streaming_client_for_base_url(&resolved.base_url);
    let is_anthropic = super::provider::provider_uses_anthropic_messages(provider, &opts.model);
    let wire_model = crate::llm_config::wire_model_id(&opts.model);

    let body = if is_anthropic {
        let mut body = serde_json::json!({
            "model": wire_model,
            "messages": opts.messages,
            "max_tokens": opts.max_tokens,
            "stream": true,
        });
        if let Some(ref sys) = opts.system {
            body["system"] = serde_json::json!(sys);
        }
        if let Some(temp) = opts.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        crate::llm::providers::anthropic::strip_unsupported_sampling_params(
            &mut body,
            &opts.model,
            &opts.thinking,
        );
        body
    } else {
        let mut msgs = Vec::new();
        if let Some(ref sys) = opts.system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.extend(opts.messages.iter().cloned());
        let mut body = serde_json::json!({
            "model": wire_model,
            "messages": msgs,
            "max_tokens": opts.max_tokens,
            "stream": true,
        });
        if let Some(temp) = opts.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        body
    };

    let req = client
        .post(resolved.url())
        .header("Content-Type", "application/json")
        .json(&body);
    let request = resolved.apply_headers(req, &opts.api_key);

    let mut es = EventSource::new(request).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "LLM stream setup error: {e}"
        ))))
    })?;

    let idle_timeout_secs = opts.idle_timeout.unwrap_or_else(|| {
        crate::stdlib::process::session_env_var("HARN_LLM_IDLE_TIMEOUT")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30)
    });
    let idle_dur = std::time::Duration::from_secs(idle_timeout_secs);

    // Prefill (time-to-first-token) streams no SSE bytes while the server
    // processes the prompt, so a slow model on a large context would trip the
    // short inter-token idle timeout *mid-prefill* (observed: a local 35B model
    // idle-timing-out before its first token on a long context). Give the first
    // token a more generous budget; subsequent inter-token gaps use the normal
    // idle timeout. Still bounded by the overall deadline below. Configurable via
    // HARN_LLM_FIRST_TOKEN_TIMEOUT; defaults to 4x the idle timeout, min 120s.
    let first_token_timeout_secs =
        crate::stdlib::process::session_env_var("HARN_LLM_FIRST_TOKEN_TIMEOUT")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or_else(|| idle_timeout_secs.saturating_mul(4).max(120));
    let first_token_dur = std::time::Duration::from_secs(first_token_timeout_secs);
    let mut got_first_token = false;

    // Bound total stream duration: a provider that dribbles bytes fast
    // enough to keep resetting the idle timer would otherwise hold the
    // stream open forever. Resolve the SAME whole-request deadline the
    // non-streaming path applies (explicit opts.timeout > HARN_LLM_TIMEOUT env
    // > model-catalog stream_timeout > 120s default). Using the raw
    // `opts.timeout` here silently fell back to 30 minutes whenever a caller
    // relied on the HARN_LLM_TIMEOUT env (leaving opts.timeout = None), so a
    // hung/dribbling provider stream could run for half an hour per call.
    let overall_budget_secs = opts.resolve_timeout();
    let overall_dur = std::time::Duration::from_secs(overall_budget_secs);
    let stream_start = std::time::Instant::now();

    loop {
        if stream_start.elapsed() >= overall_dur {
            es.close();
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "stream overall deadline exceeded: {overall_budget_secs}s budget reached before stream completed"
            )))));
        }
        let remaining_overall = overall_dur.saturating_sub(stream_start.elapsed());
        // Before the first token, allow the longer prefill budget; after, the
        // normal inter-token idle timeout.
        let active_idle = if got_first_token {
            idle_dur
        } else {
            first_token_dur
        };
        let wait = active_idle.min(remaining_overall);
        let event = match tokio::time::timeout(wait, es.next()).await {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(_) => {
                es.close();
                // Overall-deadline is caught at loop top; this is a gap timeout.
                let (secs, phase) = if got_first_token {
                    (idle_timeout_secs, "inter-token idle")
                } else {
                    (first_token_timeout_secs, "time-to-first-token (prefill)")
                };
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!("stream {phase} timeout: no data received for {secs}s"),
                ))));
            }
        };
        match event {
            Ok(Event::Message(msg)) => {
                got_first_token = true;
                if msg.data == "[DONE]" {
                    break;
                }
                let chunk_text = if is_anthropic {
                    parse_anthropic_sse_chunk(&msg.data)
                } else {
                    parse_openai_sse_chunk(&msg.data)
                };
                if let Some(text) = chunk_text {
                    if !text.is_empty()
                        && tx
                            .send(VmValue::String(arcstr::ArcStr::from(text)))
                            .await
                            .is_err()
                    {
                        break;
                    }
                }
            }
            Ok(Event::Open) => {}
            Err(_) => break,
        }
    }

    es.close();
    Ok(())
}

fn parse_openai_sse_chunk(data: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(data).ok()?;
    json["choices"][0]["delta"]["content"]
        .as_str()
        .map(|s| s.to_string())
}

fn parse_anthropic_sse_chunk(data: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(data).ok()?;
    if json["type"].as_str() == Some("content_block_delta") {
        return json["delta"]["text"].as_str().map(|s| s.to_string());
    }
    None
}
