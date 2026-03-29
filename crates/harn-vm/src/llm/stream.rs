use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::helpers::ResolvedProvider;

// =============================================================================
// Streaming
// =============================================================================

pub(crate) async fn vm_stream_llm(
    provider: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
    system: Option<&str>,
    max_tokens: i64,
    tx: &tokio::sync::mpsc::Sender<VmValue>,
) -> Result<(), VmError> {
    use reqwest_eventsource::{Event, EventSource};
    use tokio_stream::StreamExt;

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resolved = ResolvedProvider::resolve(provider);

    let body = if resolved.is_anthropic_style {
        let mut body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
            "stream": true,
        });
        if let Some(sys) = system {
            body["system"] = serde_json::json!(sys);
        }
        body
    } else {
        let mut msgs = Vec::new();
        if let Some(sys) = system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.push(serde_json::json!({"role": "user", "content": prompt}));
        serde_json::json!({
            "model": model,
            "messages": msgs,
            "max_tokens": max_tokens,
            "stream": true,
        })
    };

    let req = client
        .post(resolved.url())
        .header("Content-Type", "application/json")
        .json(&body);
    let request = resolved.apply_headers(req, api_key);

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
