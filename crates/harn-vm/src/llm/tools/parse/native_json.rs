use std::collections::BTreeSet;

use super::super::normalize_tool_call_shape;
use super::syntax::{preview_str, unknown_tool_feedback};

/// What one native-JSON scan recovered from a body of text.
pub(crate) struct NativeJsonParse {
    pub calls: Vec<serde_json::Value>,
    pub errors: Vec<String>,
    /// Byte ranges of the source the accepted payloads occupied, in order and
    /// non-overlapping. Everything outside them is text this scan did not
    /// claim, and still belongs to whoever owns the rest of the turn.
    pub spans: Vec<(usize, usize)>,
}

/// Detect and parse OpenAI-style native function calling JSON that a model
/// emitted as raw text. Matches `[{"id":...,"function":{"name":"...",
/// "arguments":"..."}}]` patterns (array or single object) embedded anywhere
/// in the text — whitespace-tolerant and id-agnostic, so pretty-printed
/// payloads and non-`call_` ids (common from local vLLM/llama.cpp templates)
/// parse instead of silently vanishing. See [`find_native_json_payloads`].
///
/// A message can carry more than one payload. A provider whose native channel
/// leaks into `content` leaks every call it was making, and a model that
/// batches two reads writes two objects; reading only the first turned the
/// rest into nothing at all — no call, no error, no prose (harn#6787).
pub(crate) fn parse_native_json_tool_calls(
    text: &str,
    known_tools: &BTreeSet<String>,
) -> NativeJsonParse {
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let mut spans = Vec::new();

    let mut items = Vec::new();
    for payload in find_native_json_payloads(text) {
        spans.push(payload.span);
        items.extend(payload.items);
    }

    for item in items {
        // Two envelope shapes carry a tool call:
        //   1. OpenAI native: `{"function":{"name":..,"arguments":..}}`.
        //   2. JSON-RPC / MCP-ish flat: `{"name":..,"arguments"|"parameters":..}`
        //      — value models emit this when they ignore the text format and
        //      reach for a generic function-call envelope. Read `name` +
        //      `arguments`/`parameters` from whichever object actually carries
        //      them (the nested `function`, else the item itself).
        let Some(item_obj) = item.as_object() else {
            continue;
        };
        let func = item
            .get("function")
            .and_then(|function| function.as_object())
            .unwrap_or(item_obj);
        // Canonical key is `name`. Accept `tool` as an alias so the bare
        // `{"tool":..,"arguments":..}` dialect gpt-oss / Harmony emits when its
        // native channel leaks into `content` is recovered (the fenced-JSON
        // parser already honors this alias). Without it, a leaked gpt-oss call
        // is silently dropped and the dirty content is persisted verbatim.
        let name = func
            .get("name")
            .or_else(|| func.get("tool"))
            .and_then(|name| name.as_str())
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        // OpenAI format encodes arguments as a JSON string; others as an object.
        // The flat JSON-RPC/MCP envelope sometimes names the slot `parameters`.
        let arguments = match func.get("arguments").or_else(|| func.get("parameters")) {
            Some(serde_json::Value::String(raw)) => match serde_json::from_str(raw) {
                Ok(value) => value,
                Err(error) => {
                    errors.push(format!(
                        "Could not parse arguments for tool '{}': {}. Raw: {}",
                        name,
                        error,
                        preview_str(raw, 200)
                    ));
                    continue;
                }
            },
            Some(obj @ serde_json::Value::Object(_)) => obj.clone(),
            _ => serde_json::Value::Object(Default::default()),
        };
        let (name, arguments) = normalize_tool_call_shape(name, arguments);
        if !known_tools.contains(&name) {
            errors.push(unknown_tool_feedback(&name, known_tools));
            continue;
        }
        let call_id = item
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or("native_fallback");
        results.push(serde_json::json!({
            "id": call_id,
            "name": name,
            "arguments": arguments,
        }));
    }

    NativeJsonParse {
        calls: results,
        errors,
        spans,
    }
}

/// One accepted native-JSON payload and the byte range it occupies.
struct NativeJsonPayload {
    items: Vec<serde_json::Value>,
    span: (usize, usize),
}

/// True when a decoded item carries a tool call rather than incidental JSON.
///
/// Two payload shapes qualify:
///   - OpenAI native: an item with an object `function` field.
///   - flat JSON-RPC/MCP: an item with a string `name` AND an
///     `arguments`/`parameters` slot that is an object OR a string — the
///     generic function-call envelope value models reach for when ignoring the
///     text format. The slot is a JSON STRING in OpenAI's on-the-wire shape
///     (`{"name":"read","arguments":"{\"path\":..}"}`), which local
///     llama.cpp/vLLM/Ollama OpenAI-mimic templates commonly emit; the
///     extractor above already decodes that string, so this gate must accept it
///     too or the call silently vanishes.
///
/// Requiring the args slot (not just a bare `name`) keeps prose JSON that
/// merely has a `name` key (config, package.json) from matching. `tool` is
/// accepted as a `name` alias for the gpt-oss / Harmony `{"tool":..}`
/// channel-leak dialect (mirrors the extractor and the fenced-JSON parser).
fn is_native_tool_call_item(item: &serde_json::Value) -> bool {
    let slot_present = |key: &str| {
        item.get(key)
            .is_some_and(|slot| slot.is_object() || slot.is_string())
    };
    let name_present = item.get("name").is_some_and(serde_json::Value::is_string)
        || item.get("tool").is_some_and(serde_json::Value::is_string);
    item.get("function")
        .is_some_and(serde_json::Value::is_object)
        || (name_present && (slot_present("arguments") || slot_present("parameters")))
}

/// Locate every native-JSON tool-call payload in `text`, in source order, each
/// with the byte range it occupies (a single object is a one-item payload).
///
/// Detection is whitespace- and id-agnostic: we no longer match brittle
/// `[{"id":` / `{"id":"call_` prefixes (those silently dropped pretty-printed
/// arrays like `[{ "id": "0", "function": {...} }]` and any non-`call_` id).
/// Instead we walk every position where a JSON value can begin (`[` or `{`),
/// let the boundary-safe `serde_json::Deserializer` attempt a parse, and accept
/// each candidate whose decoded value actually carries a tool call (see
/// [`is_native_tool_call_item`]). The Deserializer stops at the value's
/// structural end, so trailing prose — including multi-byte UTF-8
/// (emoji/accents/CJK) — is ignored without the old O(n^2) backward byte scan
/// that panicked on mid-codepoint slicing.
///
/// An accepted payload owns exactly the span the Deserializer consumed, and the
/// scan resumes at its end rather than stopping there. Stopping was harn#6787:
/// a second `{"name":..,"arguments":..}` object anywhere after the first was
/// never looked at, and because the caller also blanked the prose it was not
/// even retained as narration.
#[expect(
    clippy::string_slice,
    reason = "offset sits on an ASCII `[` or `{` byte and consumed lands on a JSON value's \
              structural end, both char boundaries"
)]
fn find_native_json_payloads(text: &str) -> Vec<NativeJsonPayload> {
    let bytes = text.as_bytes();
    let mut payloads = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if bytes[offset] != b'[' && bytes[offset] != b'{' {
            offset += 1;
            continue;
        }
        let mut stream =
            serde_json::Deserializer::from_str(&text[offset..]).into_iter::<serde_json::Value>();
        let Some(Ok(value)) = stream.next() else {
            offset += 1;
            continue;
        };
        let consumed = stream.byte_offset();
        let items = match value {
            serde_json::Value::Array(items) => items,
            other => vec![other],
        };
        // Only accept JSON that looks like a native tool-call payload. This
        // skips incidental prose JSON (config snippets, examples) and keeps
        // scanning for the real call.
        if consumed > 0 && items.iter().any(is_native_tool_call_item) {
            payloads.push(NativeJsonPayload {
                items,
                span: (offset, offset + consumed),
            });
            offset += consumed;
        } else {
            offset += 1;
        }
    }
    payloads
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::parse_native_json_tool_calls;

    fn known() -> BTreeSet<String> {
        ["look", "run"]
            .iter()
            .map(|name| (*name).to_string())
            .collect()
    }

    fn names(calls: &[serde_json::Value]) -> Vec<String> {
        calls
            .iter()
            .map(|call| call["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn paths(calls: &[serde_json::Value]) -> Vec<String> {
        calls
            .iter()
            .map(|call| {
                call["arguments"]["path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn a_run_of_bare_call_objects_recovers_every_one() {
        let text = "{\"name\": \"look\", \"arguments\": {\"path\": \"b\"}}\n\
                    {\"name\": \"look\", \"arguments\": {\"path\": \"z\"}}\n";

        let parsed = parse_native_json_tool_calls(text, &known());

        assert_eq!(paths(&parsed.calls), vec!["b", "z"], "{:?}", parsed.calls);
        assert_eq!(parsed.spans.len(), 2);
    }

    #[test]
    fn narration_between_two_payloads_survives_as_prose() {
        let text = "First this.\n{\"name\": \"look\", \"arguments\": {\"path\": \"b\"}}\n\
                    Then this.\n{\"name\": \"run\", \"arguments\": {\"command\": \"ls\"}}\nDone.";

        let parsed = parse_native_json_tool_calls(text, &known());

        assert_eq!(names(&parsed.calls), vec!["look", "run"]);
        for (start, end) in &parsed.spans {
            assert!(text[*start..*end].starts_with('{'), "span is the payload");
        }
        // Every byte outside the two payloads is narration, and none of it is
        // one of the payloads.
        let outside: String = {
            let mut buf = String::new();
            let mut cursor = 0usize;
            for (start, end) in &parsed.spans {
                buf.push_str(&text[cursor..*start]);
                cursor = *end;
            }
            buf.push_str(&text[cursor..]);
            buf
        };
        assert!(outside.contains("First this."), "{outside}");
        assert!(outside.contains("Then this."), "{outside}");
        assert!(outside.contains("Done."), "{outside}");
        assert!(!outside.contains("\"arguments\""), "{outside}");
    }

    #[test]
    fn a_later_unknown_name_is_diagnosed_rather_than_dropped() {
        let text = "{\"name\": \"look\", \"arguments\": {\"path\": \"b\"}}\n\
                    {\"name\": \"nope\", \"arguments\": {}}\n";

        let parsed = parse_native_json_tool_calls(text, &known());

        assert_eq!(names(&parsed.calls), vec!["look"]);
        assert_eq!(parsed.errors.len(), 1, "{:?}", parsed.errors);
        assert!(parsed.errors[0].contains("nope"), "{:?}", parsed.errors);
    }

    #[test]
    fn incidental_prose_json_after_a_call_stays_prose() {
        let text = "{\"name\": \"look\", \"arguments\": {\"path\": \"b\"}}\n\
                    Config: {\"name\": \"my-pkg\", \"version\": \"1.0.0\"}";

        let parsed = parse_native_json_tool_calls(text, &known());

        assert_eq!(names(&parsed.calls), vec!["look"]);
        assert_eq!(
            parsed.spans.len(),
            1,
            "the package.json object is not a payload"
        );
    }

    #[test]
    fn narration_alone_yields_nothing() {
        let parsed = parse_native_json_tool_calls(
            "I will read the file. Config: {\"name\": \"my-pkg\"}",
            &known(),
        );

        assert!(parsed.calls.is_empty());
        assert!(parsed.errors.is_empty());
        assert!(parsed.spans.is_empty());
    }

    #[test]
    fn a_multi_byte_tail_after_a_payload_does_not_panic() {
        let text = "{\"name\": \"look\", \"arguments\": {\"path\": \"café.go\"}} done ✅ — 完了";

        let parsed = parse_native_json_tool_calls(text, &known());

        assert_eq!(paths(&parsed.calls), vec!["café.go"]);
        let (_, end) = parsed.spans[0];
        assert!(text[end..].contains('✅'));
    }
}
