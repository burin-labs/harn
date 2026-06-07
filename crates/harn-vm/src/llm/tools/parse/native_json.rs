use std::collections::BTreeSet;

use super::syntax::preview_str;

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
        let func = item
            .get("function")
            .and_then(|function| function.as_object());
        let Some(func) = func else { continue };
        let name = func
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if !known_tools.contains(name) {
            let available: Vec<_> = known_tools.iter().take(20).cloned().collect();
            errors.push(format!(
                "Unknown tool '{}'. Available tools: [{}]",
                name,
                available.join(", ")
            ));
            continue;
        }
        // OpenAI format encodes arguments as a JSON string; others as an object.
        let arguments = match func.get("arguments") {
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
/// item with a `function` field). The Deserializer stops at the value's
/// structural end, so trailing prose — including multi-byte UTF-8
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
        // skips incidental prose JSON (config snippets, examples) without a
        // `function` field and keeps scanning for the real call.
        if items.iter().any(|item| {
            item.get("function")
                .is_some_and(serde_json::Value::is_object)
        }) {
            return Some(items);
        }
    }
    None
}
