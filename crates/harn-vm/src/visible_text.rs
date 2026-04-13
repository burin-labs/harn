use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct VisibleTextState {
    raw_text: String,
    last_visible_text: String,
}

impl VisibleTextState {
    pub fn push(&mut self, delta: &str, partial: bool) -> (String, String) {
        self.raw_text.push_str(delta);
        let visible_text = sanitize_visible_assistant_text(&self.raw_text, partial);
        let visible_delta = visible_text
            .strip_prefix(&self.last_visible_text)
            .unwrap_or(visible_text.as_str())
            .to_string();
        self.last_visible_text = visible_text.clone();
        (visible_text, visible_delta)
    }

    pub fn clear(&mut self) {
        self.raw_text.clear();
        self.last_visible_text.clear();
    }
}

fn internal_block_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?s)<think>.*?</think>",
            r"(?s)<think>.*$",
            r"(?s)<\|tool_call\|>.*?</\|tool_call\|>",
            // Tagged response protocol: hide tool-call bodies (executed as
            // structured data, never surfaced as narration) and done
            // blocks (runtime signal, not user-facing).
            r"(?s)<tool_call>.*?</tool_call>",
            r"(?s)<done>.*?</done>",
            r"(?s)<tool_result[^>]*>.*?</tool_result>",
            r"(?s)\[result of [^\]]+\].*?\[end of [^\]]+\]",
            r"(?m)^\s*(##DONE##|DONE|PLAN_READY)\s*$",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("valid assistant sanitization regex"))
        .collect()
    })
}

/// Strip the wrapper tags around `<assistant_prose>` blocks so the
/// surfaced visible text reads as plain narration. Matched tags that
/// are unclosed (model still streaming) are held back until the next
/// chunk resolves them.
fn unwrap_assistant_prose(text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?s)<assistant_prose>\s*(.*?)\s*</assistant_prose>")
            .expect("valid assistant_prose regex")
    });
    re.replace_all(text, "$1").to_string()
}

fn json_fence_regex() -> &'static Regex {
    static JSON_FENCE: OnceLock<Regex> = OnceLock::new();
    JSON_FENCE
        .get_or_init(|| Regex::new(r"(?s)```json[^\n]*\n(.*?)```").expect("valid json fence regex"))
}

fn inline_planner_json_regex() -> &'static Regex {
    static INLINE_PLANNER_JSON: OnceLock<Regex> = OnceLock::new();
    INLINE_PLANNER_JSON.get_or_init(|| {
        Regex::new(r#"(?s)\{\s*"mode"\s*:\s*"(?:fast_execute|plan_then_execute|ask_user)".*?\}"#)
            .expect("valid inline planner json regex")
    })
}

fn partial_inline_planner_json_regex() -> &'static Regex {
    static PARTIAL_INLINE_PLANNER_JSON: OnceLock<Regex> = OnceLock::new();
    PARTIAL_INLINE_PLANNER_JSON.get_or_init(|| {
        Regex::new(r#"(?s)\{\s*"mode"\s*:\s*"(?:fast_execute|plan_then_execute|ask_user)".*$"#)
            .expect("valid partial inline planner json regex")
    })
}

fn looks_like_internal_planning_json(source: &str) -> bool {
    let trimmed = source.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }

    fn collect_keys(value: &serde_json::Value, keys: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    keys.insert(key.clone());
                    collect_keys(child, keys);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_keys(item, keys);
                }
            }
            _ => {}
        }
    }

    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let mut keys = BTreeSet::new();
        collect_keys(&parsed, &mut keys);
        let has_planner_mode = match &parsed {
            serde_json::Value::Object(map) => map
                .get("mode")
                .and_then(|value| value.as_str())
                .is_some_and(|mode| {
                    matches!(mode, "fast_execute" | "plan_then_execute" | "ask_user")
                }),
            _ => false,
        };
        let has_internal_keys = [
            "plan",
            "steps",
            "tool_calls",
            "tool_name",
            "verification",
            "execution_mode",
            "required_outputs",
            "files_to_edit",
            "next_action",
            "reasoning",
            "direction",
            "targets",
            "tasks",
            "unknowns",
        ]
        .into_iter()
        .any(|key| keys.contains(key));
        return has_planner_mode || has_internal_keys;
    }

    false
}

fn strip_internal_json_fences(text: &str) -> String {
    json_fence_regex()
        .replace_all(text, |caps: &regex::Captures| {
            let body = caps
                .get(1)
                .map(|match_| match_.as_str())
                .unwrap_or_default();
            if looks_like_internal_planning_json(body) {
                String::new()
            } else {
                caps.get(0)
                    .map(|match_| match_.as_str().to_string())
                    .unwrap_or_default()
            }
        })
        .to_string()
}

fn strip_unclosed_internal_blocks(text: &str) -> String {
    if let Some(open_idx) = text.rfind("<|tool_call|>") {
        let close_idx = text.rfind("</|tool_call|>");
        if close_idx.is_none_or(|idx| idx < open_idx) {
            return text[..open_idx].to_string();
        }
    }

    if let Some(open_idx) = text.rfind("<tool_call>") {
        let close_idx = text.rfind("</tool_call>");
        if close_idx.is_none_or(|idx| idx < open_idx) {
            return text[..open_idx].to_string();
        }
    }

    if let Some(open_idx) = text.rfind("<done>") {
        let close_idx = text.rfind("</done>");
        if close_idx.is_none_or(|idx| idx < open_idx) {
            return text[..open_idx].to_string();
        }
    }

    if let Some(open_idx) = text.rfind("[result of ") {
        let close_idx = text.rfind("[end of ");
        if close_idx.is_none_or(|idx| idx < open_idx) {
            return text[..open_idx].to_string();
        }
    }

    if let Some(open_idx) = text.rfind("<tool_result") {
        let close_idx = text.rfind("</tool_result>");
        if close_idx.is_none_or(|idx| idx < open_idx) {
            return text[..open_idx].to_string();
        }
    }

    text.to_string()
}

fn strip_inline_internal_planning_json(text: &str, partial: bool) -> String {
    let mut stripped = inline_planner_json_regex()
        .replace_all(text, "")
        .to_string();
    if partial {
        stripped = partial_inline_planner_json_regex()
            .replace_all(&stripped, "")
            .to_string();
    }
    stripped
}

fn strip_partial_marker_suffix(text: &str) -> String {
    const MARKERS: [&str; 9] = [
        "<|tool_call|>",
        "<tool_call>",
        "<assistant_prose>",
        "<done>",
        "<tool_result",
        "[result of ",
        "##DONE##",
        "DONE",
        "PLAN_READY",
    ];
    for marker in MARKERS {
        for len in (1..marker.len()).rev() {
            let prefix = &marker[..len];
            if let Some(stripped) = text.strip_suffix(prefix) {
                return stripped.to_string();
            }
        }
    }
    text.to_string()
}

fn normalize_visible_whitespace(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace("\n\n\n", "\n\n")
        .trim()
        .to_string()
}

pub fn sanitize_visible_assistant_text(text: &str, partial: bool) -> String {
    let mut sanitized = text.to_string();
    for pattern in internal_block_patterns() {
        sanitized = pattern.replace_all(&sanitized, "").to_string();
    }
    // After runtime tags are stripped, unwrap the <assistant_prose> wrapper
    // so the user-visible stream reads as plain narration.
    sanitized = unwrap_assistant_prose(&sanitized);
    sanitized = strip_internal_json_fences(&sanitized);
    sanitized = strip_inline_internal_planning_json(&sanitized, partial);
    if partial {
        sanitized = strip_unclosed_internal_blocks(&sanitized);
        sanitized = strip_partial_marker_suffix(&sanitized);
    }
    normalize_visible_whitespace(&sanitized)
}

#[cfg(test)]
mod tests {
    use super::{sanitize_visible_assistant_text, VisibleTextState};

    #[test]
    fn push_emits_incremental_visible_delta_for_plain_chunks() {
        let mut state = VisibleTextState::default();
        let (visible, delta) = state.push("Hello", true);
        assert_eq!(visible, "Hello");
        assert_eq!(delta, "Hello");

        let (visible, delta) = state.push(" world", true);
        assert_eq!(visible, "Hello world");
        assert_eq!(delta, " world");
    }

    #[test]
    fn push_hides_open_think_block_until_closed() {
        let mut state = VisibleTextState::default();
        let (visible, delta) = state.push("Hi <think>secret", true);
        assert_eq!(visible, "Hi");
        assert_eq!(delta, "Hi");

        let (visible, delta) = state.push(" plan</think> bye", true);
        assert_eq!(visible, "Hi  bye");
        assert_eq!(delta, "  bye");
    }

    #[test]
    fn push_emits_full_visible_text_when_sanitization_shrinks_output() {
        let mut state = VisibleTextState::default();
        let (visible, _) = state.push("ok", true);
        assert_eq!(visible, "ok");

        let (visible, delta) = state.push(" <think>", true);
        assert_eq!(visible, "ok");
        // No prefix change so delta is empty.
        assert_eq!(delta, "");
    }

    #[test]
    fn push_partial_marker_suffix_is_held_back_until_resolved() {
        let mut state = VisibleTextState::default();
        let (visible, delta) = state.push("Hello\n##DON", true);
        assert_eq!(visible, "Hello");
        assert_eq!(delta, "Hello");

        let (visible, delta) = state.push("E##\nmore", true);
        assert_eq!(visible, "Hello\n\nmore");
        assert_eq!(delta, "\n\nmore");
    }

    #[test]
    fn clear_resets_streaming_state() {
        let mut state = VisibleTextState::default();
        let _ = state.push("Hello world", true);
        state.clear();
        let (visible, delta) = state.push("fresh", true);
        assert_eq!(visible, "fresh");
        assert_eq!(delta, "fresh");
    }

    #[test]
    fn sanitize_drops_inline_planner_json_only_with_planner_mode() {
        let raw = r#"{"mode":"plan_then_execute","plan":[]}"#;
        assert_eq!(sanitize_visible_assistant_text(raw, false), "");
        let raw = r#"{"status":"ok","message":"hello"}"#;
        assert_eq!(sanitize_visible_assistant_text(raw, false), raw);
    }
}
