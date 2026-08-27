//! SSE frame assembly for Gemini's `:streamGenerateContent` endpoint.
//!
//! The endpoint emits `GenerateContentResponse` chunks. This module rebuilds
//! one complete response envelope so the ordinary Gemini response parser stays
//! the single owner of transcript, tool, usage, and stop-reason mapping.

use serde_json::{Map, Value};

use crate::llm::api::{DeltaSender, DialectContract, StreamProtocol};
use crate::llm::providers::common::{maybe_emit_delta, vm_err};
use crate::value::{ProviderStreamPhase, VmError};

/// Return the endpoint selected by the GenerateContent stream flag.
pub(super) fn generate_content_url(base_url: &str, model: &str, stream: bool) -> String {
    if stream {
        format!("{base_url}/v1beta/models/{model}:streamGenerateContent?alt=sse")
    } else {
        format!("{base_url}/v1beta/models/{model}:generateContent")
    }
}

/// Accumulates Gemini GenerateContent response chunks into one response.
#[derive(Debug, Default)]
struct GenerateContentStream {
    envelope: Value,
}

impl GenerateContentStream {
    fn new() -> Self {
        Self {
            envelope: Value::Object(Map::new()),
        }
    }

    /// Merge one append-only response chunk and return its visible text.
    fn push(&mut self, frame: &Value) -> Option<String> {
        let delta = visible_text(frame);
        merge_response(&mut self.envelope, frame);
        (!delta.is_empty()).then_some(delta)
    }

    fn finish(self) -> Value {
        self.envelope
    }
}

/// Read a live SSE body from Gemini's GenerateContent streaming endpoint.
pub(crate) async fn read_generate_content_stream(
    response: reqwest::Response,
    delta_tx: Option<DeltaSender>,
    dialect: DialectContract,
) -> Result<Value, VmError> {
    use tokio_stream::StreamExt;

    let stream = response
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(stream));
    consume_generate_content_sse(reader, delta_tx, dialect).await
}

/// Consume GenerateContent SSE lines into one ordinary response envelope.
///
/// This stays generic over the reader so the production framing path is tested
/// against in-memory provider transcripts.
pub(crate) async fn consume_generate_content_sse<R: tokio::io::AsyncBufRead + Unpin>(
    reader: R,
    delta_tx: Option<DeltaSender>,
    dialect: DialectContract,
) -> Result<Value, VmError> {
    use tokio::io::AsyncBufReadExt;

    if dialect.stream_protocol() != StreamProtocol::GeminiJson {
        return Err(vm_err(
            "Gemini GenerateContent stream received a mismatched dialect",
        ));
    }

    let mut lines = reader.lines();
    let mut stream = GenerateContentStream::new();
    let mut saw_provider_frame = false;
    let mut saw_terminal_frame = false;
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| vm_err(format!("gemini stream read error: {error}")))?
    {
        let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        saw_provider_frame = true;
        saw_terminal_frame |= terminal_frame(&frame);
        if let Some(text) = stream.push(&frame) {
            maybe_emit_delta(delta_tx.clone(), &text);
        }
    }

    if !saw_terminal_frame {
        return Err(crate::llm::api::premature_stream_eof(
            "gemini",
            if saw_provider_frame {
                ProviderStreamPhase::Streaming
            } else {
                ProviderStreamPhase::AwaitingFirstChunk
            },
            saw_provider_frame,
            "finishReason or usageMetadata",
        ));
    }

    Ok(stream.finish())
}

fn terminal_frame(frame: &Value) -> bool {
    frame.get("error").is_some()
        || frame.get("usageMetadata").is_some()
        || frame
            .get("candidates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|candidate| candidate.get("finishReason").is_some())
}

fn merge_response(target: &mut Value, frame: &Value) {
    let Some(source) = frame.as_object() else {
        return;
    };
    let target = target
        .as_object_mut()
        .expect("GenerateContent stream envelope is always an object");
    for (key, value) in source {
        if key == "candidates" {
            merge_candidates(target, value);
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn merge_candidates(target: &mut Map<String, Value>, incoming: &Value) {
    let Some(incoming) = incoming.as_array() else {
        return;
    };
    let candidates = target
        .entry("candidates".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("GenerateContent candidates stay an array");
    for (index, candidate) in incoming.iter().enumerate() {
        while candidates.len() <= index {
            candidates.push(Value::Object(Map::new()));
        }
        merge_candidate(&mut candidates[index], candidate);
    }
}

fn merge_candidate(target: &mut Value, incoming: &Value) {
    let (Some(target), Some(incoming)) = (target.as_object_mut(), incoming.as_object()) else {
        *target = incoming.clone();
        return;
    };
    for (key, value) in incoming {
        if key == "content" {
            merge_content(target, value);
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn merge_content(candidate: &mut Map<String, Value>, incoming: &Value) {
    let Some(incoming) = incoming.as_object() else {
        candidate.insert("content".to_string(), incoming.clone());
        return;
    };
    let content = candidate
        .entry("content".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("GenerateContent content stays an object");
    for (key, value) in incoming {
        if key == "parts" {
            merge_parts(content, value);
        } else {
            content.insert(key.clone(), value.clone());
        }
    }
}

fn merge_parts(content: &mut Map<String, Value>, incoming: &Value) {
    let Some(incoming) = incoming.as_array() else {
        return;
    };
    let parts = content
        .entry("parts".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("GenerateContent parts stay an array");
    for part in incoming {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            if let Some(existing) = parts.iter_mut().rev().find(|existing| {
                existing.get("text").is_some() && existing.get("thought") == part.get("thought")
            }) {
                merge_text_part(existing, part, text);
            } else {
                parts.push(part.clone());
            }
        } else if let Some(existing) = parts
            .iter_mut()
            .rev()
            .find(|existing| same_function_call(existing, part))
        {
            merge_json(existing, part);
        } else {
            parts.push(part.clone());
        }
    }
}

fn merge_text_part(existing: &mut Value, incoming: &Value, text: &str) {
    let existing = existing
        .as_object_mut()
        .expect("GenerateContent text part stays an object");
    let current = existing
        .get_mut("text")
        .and_then(|value| value.as_str())
        .expect("GenerateContent text part stays text")
        .to_string();
    existing.insert(
        "text".to_string(),
        Value::String(format!("{current}{text}")),
    );
    for (key, value) in incoming.as_object().into_iter().flatten() {
        if key != "text" {
            existing.insert(key.clone(), value.clone());
        }
    }
}

fn same_function_call(left: &Value, right: &Value) -> bool {
    let (Some(left), Some(right)) = (
        left.get("functionCall").and_then(Value::as_object),
        right.get("functionCall").and_then(Value::as_object),
    ) else {
        return false;
    };
    left.get("id").is_some_and(|id| right.get("id") == Some(id))
        || left
            .get("name")
            .is_some_and(|name| right.get("name") == Some(name))
}

fn merge_json(target: &mut Value, incoming: &Value) {
    match (target, incoming) {
        (Value::Object(target), Value::Object(incoming)) => {
            for (key, value) in incoming {
                match target.get_mut(key) {
                    Some(existing) => merge_json(existing, value),
                    None => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (target, incoming) => *target = incoming.clone(),
    }
}

fn visible_text(envelope: &Value) -> String {
    envelope["candidates"][0]["content"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::ThinkingConfig;
    use crate::llm::capabilities::WireDialect;
    use crate::llm::providers::gemini::{parse_response, test_support::gemini_payload};

    fn dialect() -> DialectContract {
        DialectContract::new(WireDialect::Gemini, None)
    }

    async fn assembled_text_and_deltas(wire: &str) -> (String, Vec<String>) {
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
        let envelope = consume_generate_content_sse(wire.as_bytes(), Some(delta_tx), dialect())
            .await
            .expect("stream parses");
        let result = parse_response(
            &envelope,
            &gemini_payload("gemini-2.5-flash", ThinkingConfig::Disabled),
        )
        .expect("assembled response parses");
        let deltas = std::iter::from_fn(|| delta_rx.try_recv().ok()).collect();
        (result.text, deltas)
    }

    #[test]
    fn streaming_selects_the_dedicated_generate_content_endpoint() {
        assert_eq!(
            generate_content_url("https://example.test", "gemini-2.5-flash", true),
            "https://example.test/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            generate_content_url("https://example.test", "gemini-2.5-flash", false),
            "https://example.test/v1beta/models/gemini-2.5-flash:generateContent"
        );
    }

    #[tokio::test]
    async fn reassembles_text_tools_and_terminal_usage() {
        let wire = concat!(
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"hel"}]}}]}"#,
            "\n\n",
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"lo"},{"functionCall":{"name":"echo","args":{"value":"marker"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3}}"#,
            "\n",
        );
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
        let envelope = consume_generate_content_sse(wire.as_bytes(), Some(delta_tx), dialect())
            .await
            .expect("stream parses");
        let result = parse_response(
            &envelope,
            &gemini_payload("gemini-2.5-flash", ThinkingConfig::Disabled),
        )
        .expect("assembled response parses");

        assert_eq!(result.text, "hello");
        assert_eq!(result.input_tokens, 7);
        assert_eq!(result.output_tokens, 3);
        assert_eq!(result.stop_reason.as_deref(), Some("STOP"));
        assert_eq!(result.tool_calls[0]["name"], "echo");
        assert_eq!(result.tool_calls[0]["arguments"]["value"], "marker");
        assert_eq!(delta_rx.recv().await.as_deref(), Some("hel"));
        assert_eq!(delta_rx.recv().await.as_deref(), Some("lo"));
    }

    #[tokio::test]
    async fn appends_repeated_identical_text_chunks() {
        let wire = concat!(
            r#"data: {"candidates":[{"content":{"parts":[{"text":"ha"}]}}]}"#,
            "\n\n",
            r#"data: {"candidates":[{"content":{"parts":[{"text":"ha"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":2,"candidatesTokenCount":2}}"#,
            "\n",
        );
        let (text, deltas) = assembled_text_and_deltas(wire).await;

        assert_eq!(text, "haha");
        assert_eq!(deltas, ["ha", "ha"]);
    }

    #[tokio::test]
    async fn text_chunks_that_share_a_prefix_remain_append_only() {
        let wire = concat!(
            r#"data: {"candidates":[{"content":{"parts":[{"text":"ha"}]}}]}"#,
            "\n\n",
            r#"data: {"candidates":[{"content":{"parts":[{"text":"haha"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":2,"candidatesTokenCount":3}}"#,
            "\n",
        );
        let (text, deltas) = assembled_text_and_deltas(wire).await;

        assert_eq!(text, "hahaha");
        assert_ne!(text, "haha", "chunks are not cumulative snapshots");
        assert_eq!(deltas, ["ha", "haha"]);
    }

    #[tokio::test]
    async fn rejects_a_stream_without_terminal_usage_or_finish_reason() {
        let error = consume_generate_content_sse(
            concat!(
                r#"data: {"candidates":[{"content":{"parts":[{"text":"partial"}]}}]}"#,
                "\n",
            )
            .as_bytes(),
            None,
            dialect(),
        )
        .await
        .expect_err("partial stream must not look complete");
        assert!(error.to_string().contains("finishReason or usageMetadata"));
    }

    #[tokio::test]
    async fn rejects_a_mismatched_dialect_contract() {
        let mismatch = DialectContract::new(WireDialect::OpenAiCompat, None);
        let error = consume_generate_content_sse(b"".as_slice(), None, mismatch)
            .await
            .expect_err("an OpenAI contract must not decode Gemini events");
        assert!(error.to_string().contains("mismatched dialect"));
    }
}
