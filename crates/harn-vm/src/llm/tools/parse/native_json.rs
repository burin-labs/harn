use std::collections::BTreeSet;

use super::syntax::{preview_str, unknown_tool_feedback};

/// Detect and parse OpenAI-style native function calling JSON that a model
/// emitted as raw text. Matches `[{"id":...,"function":{"name":"...",
/// "arguments":"..."}}]` patterns (array or single object) embedded anywhere
/// in the text — whitespace-tolerant and id-agnostic, so pretty-printed
/// payloads and non-`call_` ids (common from local vLLM/llama.cpp templates)
/// parse instead of silently vanishing. See [`find_native_json_items`].
pub(crate) fn parse_native_json_tool_calls(
    text: &str,
    known_tools: &BTreeSet<String>,
) -> (Vec<serde_json::Value>, Vec<String>) {
    let mut results = Vec::new();
    let mut errors = Vec::new();

    let Some(items) = find_native_json_items(text) else {
        return (results, errors);
    };

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
        if !known_tools.contains(name) {
            errors.push(unknown_tool_feedback(name, known_tools));
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

    (results, errors)
}

/// Locate a native-JSON tool-call payload anywhere in `text` and return its
/// items (a single object is wrapped in a one-element vec).
///
/// Detection is whitespace- and id-agnostic: we no longer match brittle
/// `[{"id":` / `{"id":"call_` prefixes (those silently dropped pretty-printed
/// arrays like `[{ "id": "0", "function": {...} }]` and any non-`call_` id).
/// Instead we walk every position where a JSON value can begin (`[` or `{`),
/// let the boundary-safe `serde_json::Deserializer` attempt a parse, and accept
/// the first candidate whose decoded value actually carries a tool call (an
/// item with a `function` object, or a flat `name` + `arguments`/`parameters`
/// envelope — see the acceptance check below). The Deserializer stops at the
/// value's structural end, so trailing prose — including multi-byte UTF-8
/// (emoji/accents/CJK) — is ignored without the old O(n^2) backward byte scan
/// that panicked on mid-codepoint slicing.
fn find_native_json_items(text: &str) -> Option<Vec<serde_json::Value>> {
    let bytes = text.as_bytes();
    for (offset, &byte) in bytes.iter().enumerate() {
        if byte != b'[' && byte != b'{' {
            continue;
        }
        let parsed = serde_json::Deserializer::from_str(&text[offset..])
            .into_iter::<serde_json::Value>()
            .next()
            .and_then(|result| result.ok())
            .map(|value| match value {
                serde_json::Value::Array(items) => items,
                other => vec![other],
            });
        let Some(items) = parsed else {
            continue;
        };
        // Only accept JSON that looks like a native tool-call payload. This
        // skips incidental prose JSON (config snippets, examples) and keeps
        // scanning for the real call. Two payload shapes qualify:
        //   - OpenAI native: an item with an object `function` field.
        //   - flat JSON-RPC/MCP: an item with a string `name` AND an
        //     `arguments`/`parameters` slot that is an object OR a string —
        //     the generic function-call envelope value models reach for when
        //     ignoring the text format. The slot is a JSON STRING in OpenAI's
        //     on-the-wire shape (`{"name":"read","arguments":"{\"path\":..}"}`),
        //     which local llama.cpp/vLLM/Ollama OpenAI-mimic templates commonly
        //     emit; the downstream extractor already decodes that string, so the
        //     gate must accept it too or the call silently vanishes.
        // Requiring the args slot (not just a bare `name`) keeps prose JSON
        // that merely has a `name` key (config, package.json) from matching.
        let args_slot_present = |item: &serde_json::Value, key: &str| {
            item.get(key)
                .is_some_and(|slot| slot.is_object() || slot.is_string())
        };
        // `tool` is accepted as a `name` alias for the gpt-oss / Harmony
        // `{"tool":..,"arguments":..}` channel-leak dialect (mirrors the
        // extractor and the fenced-JSON parser).
        let name_slot_present = |item: &serde_json::Value| {
            item.get("name").is_some_and(serde_json::Value::is_string)
                || item.get("tool").is_some_and(serde_json::Value::is_string)
        };
        if items.iter().any(|item| {
            item.get("function")
                .is_some_and(serde_json::Value::is_object)
                || (name_slot_present(item)
                    && (args_slot_present(item, "arguments")
                        || args_slot_present(item, "parameters")))
        }) {
            return Some(items);
        }
    }
    None
}
