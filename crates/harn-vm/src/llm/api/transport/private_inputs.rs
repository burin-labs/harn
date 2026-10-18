use serde_json::Value;

/// Filter model-facing text without changing protocol tags, tool names, or
/// opaque media bytes that happen to contain the same characters as an input.
pub(super) fn filter_text_fields(value: &mut Value) {
    if !crate::session_inputs::has_private_filter() {
        return;
    }
    visit(value, false);
}

fn visit(value: &mut Value, text: bool) {
    match value {
        Value::String(value) if text => *value = crate::session_inputs::redact(value),
        Value::Array(values) => values.iter_mut().for_each(|value| visit(value, text)),
        Value::Object(values) => {
            let tool_input = values.get("type").and_then(Value::as_str) == Some("tool_use");
            for (name, value) in values {
                if matches!(
                    name.as_str(),
                    "arguments"
                        | "args"
                        | "enum"
                        | "const"
                        | "default"
                        | "examples"
                        | "pattern"
                        | "stop"
                        | "stop_sequences"
                ) || (tool_input && name == "input")
                {
                    filter_data(value);
                } else {
                    visit(
                        value,
                        matches!(
                            name.as_str(),
                            "text"
                                | "content"
                                | "system"
                                | "instructions"
                                | "prompt"
                                | "input"
                                | "output"
                                | "reasoning_content"
                                | "thinking"
                                | "summary"
                                | "description"
                                | "title"
                                | "$comment"
                        ),
                    );
                }
            }
        }
        _ => {}
    }
}

fn filter_data(value: &mut Value) {
    match value {
        Value::String(value) => *value = crate::session_inputs::redact(value),
        Value::Array(values) => values.iter_mut().for_each(filter_data),
        Value::Object(values) => values.values_mut().for_each(filter_data),
        _ => {}
    }
}
