use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::api::LlmCallOptions;
use super::helpers::ResolvedProvider;

// =============================================================================
// Streaming
// =============================================================================

pub(crate) async fn vm_stream_llm(
    opts: &LlmCallOptions,
    tx: &tokio::sync::mpsc::Sender<VmValue>,
) -> Result<(), VmError> {
    use reqwest_eventsource::{Event, EventSource};
    use tokio_stream::StreamExt;

    let provider = &opts.provider;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resolved = ResolvedProvider::resolve(provider);

    let body = if resolved.is_anthropic_style {
        let mut body = serde_json::json!({
            "model": opts.model,
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
        body
    } else {
        let mut msgs = Vec::new();
        if let Some(ref sys) = opts.system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.extend(opts.messages.iter().cloned());
        let mut body = serde_json::json!({
            "model": opts.model,
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
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "LLM stream setup error: {e}"
        ))))
    })?;

    while let Some(event) = es.next().await {
        match event {
            Ok(Event::Message(msg)) => {
                if msg.data == "[DONE]" {
                    break;
                }
                let chunk_text = if resolved.is_anthropic_style {
                    parse_anthropic_sse_chunk(&msg.data)
                } else {
                    parse_openai_sse_chunk(&msg.data)
                };
                if let Some(text) = chunk_text {
                    if !text.is_empty() && tx.send(VmValue::String(Rc::from(text))).await.is_err() {
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
