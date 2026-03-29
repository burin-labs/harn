use std::rc::Rc;

use crate::llm_config;
use crate::value::{VmError, VmValue};

use super::api::apply_auth_headers;

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

    // Streaming: only connect_timeout (no overall timeout -- streams can run long)
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let pdef = llm_config::provider_config(provider);
    let is_anthropic = pdef
        .map(|p| p.chat_endpoint.contains("/messages"))
        .unwrap_or(provider == "anthropic");

    let request = if is_anthropic {
        let base_url = pdef
            .map(llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://api.anthropic.com/v1".to_string());
        let endpoint = pdef
            .map(|p| p.chat_endpoint.as_str())
            .unwrap_or("/messages");

        let mut body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
            "stream": true,
        });
        if let Some(sys) = system {
            body["system"] = serde_json::json!(sys);
        }

        let mut req = client
            .post(format!("{base_url}{endpoint}"))
            .header("Content-Type", "application/json")
            .json(&body);
        req = apply_auth_headers(req, api_key, pdef);
        if let Some(p) = pdef {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        req
    } else {
        let base_url = pdef
            .map(llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let endpoint = pdef
            .map(|p| p.chat_endpoint.as_str())
            .unwrap_or("/chat/completions");

        let mut msgs = Vec::new();
        if let Some(sys) = system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.push(serde_json::json!({"role": "user", "content": prompt}));

        let body = serde_json::json!({
            "model": model,
            "messages": msgs,
            "max_tokens": max_tokens,
            "stream": true,
        });

        let mut req = client
            .post(format!("{base_url}{endpoint}"))
            .header("Content-Type", "application/json")
            .json(&body);
        req = apply_auth_headers(req, api_key, pdef);
        if let Some(p) = pdef {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        req
    };

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
                let chunk_text = if is_anthropic {
                    parse_anthropic_sse_chunk(&msg.data)
                } else {
                    parse_openai_sse_chunk(&msg.data)
                };
                if let Some(text) = chunk_text {
                    if !text.is_empty()
                        && tx
                            .send(VmValue::String(Rc::from(text.as_str())))
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
