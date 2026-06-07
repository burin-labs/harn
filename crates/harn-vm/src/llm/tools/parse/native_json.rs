use std::collections::BTreeSet;

/// Detect and parse OpenAI-style native function calling JSON that a model
/// emitted as raw text. Looks for `[{"id":"call_...","function":{"name":"...",
/// "arguments":"..."}}]` patterns (array or single object) embedded anywhere
/// in the text.
pub(crate) fn parse_native_json_tool_calls(
    text: &str,
    known_tools: &BTreeSet<String>,
) -> (Vec<serde_json::Value>, Vec<String>) {
    let mut results = Vec::new();
    let mut errors = Vec::new();

    let json_start = text
        .find("[{\"id\":")
        .or_else(|| text.find("[{\"id\":"))
        .or_else(|| text.find("{\"id\":\"call_"));

    let Some(start) = json_start else {
        return (results, errors);
    };

    let json_text = &text[start..];
    // Parse the first JSON value at `start`, stopping at its structural end so
    // trailing prose (including multi-byte UTF-8 — emoji/accents/CJK) is simply
    // ignored. This is the same boundary-safe streaming technique used by
    // `tagged.rs::parse_mistral_json_payload`; the old O(n^2) backward byte
    // scan (`&text[start..=end]`) panicked the instant `end + 1` landed inside a
    // multi-byte codepoint, aborting the turn before it reached the valid `]`.
    let parsed: Option<Vec<serde_json::Value>> = serde_json::Deserializer::from_str(json_text)
        .into_iter::<serde_json::Value>()
        .next()
        .and_then(|result| result.ok())
        .map(|value| match value {
            serde_json::Value::Array(items) => items,
            other => vec![other],
        });

    let Some(items) = parsed else {
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
                        &raw[..raw.len().min(200)]
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
