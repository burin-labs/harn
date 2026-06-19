//! Provider/model compatibility cleanup for tool-call envelopes.
//!
//! Some OpenAI-compatible hosts leak model-native wrappers into otherwise
//! structured tool calls. GPT-OSS/Harmony is the common case: a call can arrive
//! as a generic `tool`/`tool.call`/`tool.exec` function whose arguments contain
//! the real `{ name, args }`, or the tool name can carry a channel suffix such
//! as `run<|channel|>commentary`. Some providers also surface marker-only
//! wrapper names such as `<|constrain|>json`; normalize those shapes before
//! policy and dispatch see them.

const HARMONY_CHANNEL_MARKERS: &[&str] =
    &["<|channel|>", "<|message|>", "<|recipient|>", "<|end|>"];

/// Namespace prefixes that cheap/OpenAI-compatible hosts (notably gpt-oss-120b)
/// prepend to otherwise-bare tool names, e.g. `tool.look`, `functions.search`.
/// These are stripped only for names that are NOT generic wrappers, so that
/// `tool.call`/`tool.exec`/`function.call` still route through
/// [`unwrap_generic_tool_arguments`] to recover their inner `{ name, args }`.
const TOOL_NAMESPACE_PREFIXES: &[&str] = &["tool.", "tools.", "functions.", "function."];

/// Strip provider protocol tokens that were appended to a function name.
pub(crate) fn normalize_tool_name(name: &str) -> String {
    let trimmed = name.trim();
    for marker in HARMONY_CHANNEL_MARKERS {
        if let Some(pos) = trimmed.find(marker) {
            let head = trimmed[..pos].trim();
            if !head.is_empty() {
                return head.to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Strip a leading `tool.`/`tools.`/`functions.`/`function.` namespace prefix
/// from a tool name, e.g. `tool.look` -> `look`. Returns `None` when no prefix
/// applies or when stripping would yield an empty name. Callers MUST guard
/// against generic wrapper names (see [`is_generic_wrapper_name`]) before
/// applying this, so that `tool.call`/`tool.exec`/`function.call` keep their
/// wrapper-unwrapping path instead of collapsing to `call`/`exec`.
fn strip_tool_namespace_prefix(name: &str) -> Option<String> {
    let trimmed = name.trim();
    for prefix in TOOL_NAMESPACE_PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let bare = rest.trim();
            if !bare.is_empty() {
                return Some(bare.to_string());
            }
        }
    }
    None
}

/// Recover a bare tool name from a namespace-prefixed one (`tool.look` ->
/// `look`), but only when the name is NOT a generic wrapper. The guard keeps
/// `tool.call`/`tool.exec`/`function.call` intact so they are never collapsed
/// to `call`/`exec` — those are unwrapped via [`unwrap_generic_tool_arguments`]
/// before this is reached. Used both for the top-level name and for the inner
/// name recovered from a generic wrapper.
fn recover_namespaced_name(name: String) -> String {
    if is_generic_wrapper_name(&name) {
        return name;
    }
    strip_tool_namespace_prefix(&name).unwrap_or(name)
}

/// Normalize a parsed tool-call `(name, arguments)` pair into the dispatchable
/// Harn shape.
pub(crate) fn normalize_tool_call_shape(
    name: &str,
    arguments: serde_json::Value,
) -> (String, serde_json::Value) {
    let normalized_name = normalize_tool_name(name);
    let is_marker_wrapper = is_harmony_marker_wrapper_name(&normalized_name);
    if !is_generic_wrapper_name(&normalized_name) && !is_marker_wrapper {
        return (recover_namespaced_name(normalized_name), arguments);
    }

    if let Some((inner_name, inner_arguments)) = unwrap_generic_tool_arguments(&arguments) {
        return (
            recover_namespaced_name(normalize_tool_name(&inner_name)),
            inner_arguments,
        );
    }

    if is_marker_wrapper {
        if let Some(inferred_name) = infer_tool_name_from_arguments(&arguments) {
            return (inferred_name, arguments);
        }
    }

    (normalized_name, arguments)
}

fn is_generic_wrapper_name(name: &str) -> bool {
    matches!(
        name,
        "tool" | "tool.call" | "tool.exec" | "function" | "function.call" | "call"
    )
}

fn is_harmony_marker_wrapper_name(name: &str) -> bool {
    let trimmed = name.trim();
    trimmed.starts_with("<|") && trimmed.contains("|>")
}

fn infer_tool_name_from_arguments(arguments: &serde_json::Value) -> Option<String> {
    let object = arguments.as_object()?;
    let has_command_shape = object
        .get("command")
        .or_else(|| object.get("commands"))
        .or_else(|| object.get("cmd"))
        .is_some();
    if has_command_shape {
        return Some("run".to_string());
    }

    None
}

fn unwrap_generic_tool_arguments(
    arguments: &serde_json::Value,
) -> Option<(String, serde_json::Value)> {
    let object = arguments.as_object()?;
    let inner_name = object
        .get("name")
        .or_else(|| object.get("tool"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let inner_arguments = match object
        .get("args")
        .or_else(|| object.get("arguments"))
        .or_else(|| object.get("parameters"))
    {
        Some(value @ serde_json::Value::Object(_)) => value.clone(),
        Some(serde_json::Value::String(raw)) => match serde_json::from_str(raw) {
            Ok(value @ serde_json::Value::Object(_)) => value,
            _ => return None,
        },
        Some(serde_json::Value::Null) | None => serde_json::Value::Object(Default::default()),
        Some(_) => return None,
    };

    Some((inner_name, inner_arguments))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_harmony_channel_suffix_from_tool_name() {
        assert_eq!(normalize_tool_name("run<|channel|>commentary"), "run");
    }

    #[test]
    fn unwraps_generic_tool_envelope() {
        let (name, arguments) = normalize_tool_call_shape(
            "tool.call",
            serde_json::json!({
                "name": "look",
                "args": {"intent": "read", "file": "src/lib.rs"}
            }),
        );

        assert_eq!(name, "look");
        assert_eq!(arguments["intent"], "read");
        assert_eq!(arguments["file"], "src/lib.rs");
    }

    #[test]
    fn infers_run_from_harmony_marker_wrapper_command_shape() {
        let arguments = serde_json::json!({"command": "cargo test"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("<|constrain|>json", arguments.clone());

        assert_eq!(name, "run");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn preserves_unrecognized_harmony_marker_wrapper_shape() {
        let arguments = serde_json::json!({"intent": "read", "file": "src/lib.rs"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("<|constrain|>json", arguments.clone());

        assert_eq!(name, "<|constrain|>json");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn preserves_non_wrapper_tool_call() {
        let arguments = serde_json::json!({"command": "cargo test"});
        let (name, normalized_arguments) = normalize_tool_call_shape("run", arguments.clone());

        assert_eq!(name, "run");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn strips_tool_namespace_prefix_from_native_name() {
        let arguments = serde_json::json!({"intent": "read", "file": "src/lib.rs"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("tool.look", arguments.clone());

        assert_eq!(name, "look");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn strips_functions_namespace_prefix_from_native_name() {
        let arguments = serde_json::json!({"query": "needle"});
        let (name, normalized_arguments) =
            normalize_tool_call_shape("functions.search", arguments.clone());

        assert_eq!(name, "search");
        assert_eq!(normalized_arguments, arguments);
    }

    #[test]
    fn strips_tools_and_function_namespace_prefixes() {
        let (name, _) = normalize_tool_call_shape("tools.run", serde_json::json!({}));
        assert_eq!(name, "run");
        // `function.edit` is NOT a generic wrapper (only `function.call` is), so
        // it takes the prefix-strip path.
        let (name, _) = normalize_tool_call_shape("function.edit", serde_json::json!({}));
        assert_eq!(name, "edit");
    }

    #[test]
    fn generic_tool_exec_wrapper_unwraps_inner_name_not_prefix_stripped() {
        // Collision guard: `tool.exec` shares the `tool.` prefix but is a
        // generic wrapper. It must unwrap its inner `{name, args}`, never be
        // naively stripped to `exec`.
        let (name, arguments) = normalize_tool_call_shape(
            "tool.exec",
            serde_json::json!({
                "name": "run",
                "args": {"command": "cargo test"}
            }),
        );

        assert_eq!(name, "run");
        assert_eq!(arguments["command"], "cargo test");
    }

    #[test]
    fn generic_function_call_wrapper_is_not_prefix_stripped() {
        // `function.call` shares the `function.` prefix but is a generic
        // wrapper; it must unwrap rather than collapse to `call`.
        let (name, arguments) = normalize_tool_call_shape(
            "function.call",
            serde_json::json!({
                "name": "search",
                "args": {"query": "needle"}
            }),
        );

        assert_eq!(name, "search");
        assert_eq!(arguments["query"], "needle");
    }

    #[test]
    fn strips_namespace_prefix_after_harmony_suffix() {
        // A name can carry both a namespace prefix and a harmony suffix; the
        // suffix is removed first, then the prefix.
        let (name, _) =
            normalize_tool_call_shape("tool.run<|channel|>commentary", serde_json::json!({}));
        assert_eq!(name, "run");
    }

    #[test]
    fn leaves_already_bare_name_unchanged() {
        let arguments = serde_json::json!({"intent": "read"});
        let (name, normalized_arguments) = normalize_tool_call_shape("look", arguments.clone());

        assert_eq!(name, "look");
        assert_eq!(normalized_arguments, arguments);
    }
}
