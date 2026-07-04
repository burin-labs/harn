use std::collections::BTreeSet;
use std::sync::OnceLock;

use crate::llm::tools::{
    TEXT_TOOL_CALL_CLOSE, TEXT_TOOL_CALL_CLOSE_COMPACT, TEXT_TOOL_CALL_OPEN,
    TEXT_TOOL_CALL_OPEN_COMPACT,
};
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
            r"(?s)<tool_?call>.*?</tool_?call>",
            r"(?s)<done>.*?</done>",
            r"(?s)<tool_result[^>]*>.*?</tool_result>",
            r"(?s)\[result of [^\]]+\].*?\[end of [^\]]+\]",
            r"(?m)^\s*(##DONE##|DONE|PLAN_READY)\s*$",
            r"(?s)\s*(##DONE##|PLAN_READY)\s*$",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("valid assistant sanitization regex"))
        .collect()
    })
}

fn assistant_prose_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?ms)^[ \t]*<assistant_?prose>\s*(.*?)\s*</assistant_?prose>")
            .expect("valid assistant_prose regex")
    })
}

fn user_response_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?ms)^[ \t]*<user_?response>\s*(.*?)\s*</user_?response>")
            .expect("valid user_response regex")
    })
}

fn inside_markdown_fence(text: &str, idx: usize) -> bool {
    let mut count = 0;
    let mut cursor = 0;
    while cursor < idx {
        let Some(pos) = text[cursor..idx].find("```") else {
            break;
        };
        count += 1;
        cursor += pos + 3;
    }
    count % 2 == 1
}

fn is_top_level_tag_position(text: &str, idx: usize) -> bool {
    let line_start = text[..idx].rfind('\n').map(|pos| pos + 1).unwrap_or(0);
    text[line_start..idx]
        .chars()
        .all(|ch| matches!(ch, ' ' | '\t' | '\r'))
}

fn is_protocol_tag_position(text: &str, idx: usize) -> bool {
    is_top_level_tag_position(text, idx) && !inside_markdown_fence(text, idx)
}

fn extract_user_response(text: &str) -> Option<String> {
    let sections: Vec<String> = user_response_regex()
        .captures_iter(text)
        .filter(|caps| {
            caps.get(0)
                .is_some_and(|m| is_protocol_tag_position(text, m.start()))
        })
        .filter_map(|caps| caps.get(1).map(|m| m.as_str().trim().to_string()))
        .filter(|section| !section.is_empty())
        .collect();
    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

fn unwrap_assistant_prose(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in assistant_prose_regex().captures_iter(text) {
        let Some(block) = caps.get(0) else {
            continue;
        };
        if !is_protocol_tag_position(text, block.start()) {
            continue;
        }
        out.push_str(&text[last..block.start()]);
        if let Some(body) = caps.get(1) {
            out.push_str(body.as_str().trim());
        }
        last = block.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Strip the wrapper tags around `<assistant_prose>` blocks so the
/// surfaced visible text reads as plain narration. When a
/// `<user_response>` block is present, it becomes the authoritative
/// host-facing surface and supersedes generic assistant prose.
fn extract_visible_prose(text: &str) -> String {
    if let Some(user_response) = extract_user_response(text) {
        return user_response;
    }
    unwrap_assistant_prose(text)
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

    if let Some(open_idx) = text.rfind(TEXT_TOOL_CALL_OPEN) {
        let close_idx = text.rfind(TEXT_TOOL_CALL_CLOSE);
        if is_protocol_tag_position(text, open_idx) && close_idx.is_none_or(|idx| idx < open_idx) {
            return text[..open_idx].to_string();
        }
    }

    if let Some(open_idx) = text.rfind(TEXT_TOOL_CALL_OPEN_COMPACT) {
        let close_idx = text.rfind(TEXT_TOOL_CALL_CLOSE_COMPACT);
        if is_protocol_tag_position(text, open_idx) && close_idx.is_none_or(|idx| idx < open_idx) {
            return text[..open_idx].to_string();
        }
    }

    if let Some(open_idx) = text.rfind("<done>") {
        let close_idx = text.rfind("</done>");
        if is_protocol_tag_position(text, open_idx) && close_idx.is_none_or(|idx| idx < open_idx) {
            return text[..open_idx].to_string();
        }
    }

    if let Some(open_idx) = text.rfind("<user_response>") {
        let close_idx = text.rfind("</user_response>");
        if is_protocol_tag_position(text, open_idx) && close_idx.is_none_or(|idx| idx < open_idx) {
            return text[..open_idx].to_string();
        }
    }

    if let Some(open_idx) = text.rfind("<userresponse>") {
        let close_idx = text.rfind("</userresponse>");
        if is_protocol_tag_position(text, open_idx) && close_idx.is_none_or(|idx| idx < open_idx) {
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
        if is_protocol_tag_position(text, open_idx) && close_idx.is_none_or(|idx| idx < open_idx) {
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

fn protocol_residue_regex() -> &'static Regex {
    // Orphan / truncated protocol-tag litter that the well-formed block
    // patterns above cannot match: a closing tag with no surviving opener, and
    // the right-anchored `</tool_call>` truncations (`tool_call>`, `ol_call>`,
    // `l_call>`, `_call>`) plus `</assistant_prose>` / `_prose>` / `</done>` /
    // `/done>` fragments that weak open-weight models (incl. the GLM default)
    // emit mid-stream. These are control-token residue, never legitimate
    // narration, so they are stripped unconditionally — including from the
    // FINAL transcript, which the partial-only strippers below never see.
    // Bounds are tight (anchored on `_call>` / explicit tag names) to avoid
    // touching ordinary prose like "x > y" or words ending in "e".
    // Scope is deliberately limited to the UNAMBIGUOUS corruption families that
    // never occur in real prose: right-anchored `</tool_call>` truncations
    // (`</tool_call>`, `tool_call>`, `ol_call>`, `l_call>`, `_call>`, with the
    // `<|tool_call|>` channel variant) and the `<assistant_prose>` close-tag
    // truncations (`</assistant_prose>`, `assistant_prose>`, `nt_prose>`,
    // `_prose>`). We do NOT blanket-strip `<user_response>`/`<done>`/
    // `<tool_result>` here — those are owned by the position/fence-aware logic
    // above and have legitimate inline-mention forms (see the placeholder/fence
    // tests), so touching them regresses those guarantees.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"<?/?\|?(?:t?o?o?l?)_call\|?>|<?/?\|?[a-z]*_prose>")
            .expect("valid protocol residue regex")
    })
}

fn strip_protocol_residue(text: &str) -> String {
    // Fence-aware, matching the rest of this module: a fenced code block may
    // legitimately show `</tool_call>` as an example, so residue inside a
    // markdown fence is preserved; only standalone litter is removed.
    protocol_residue_regex()
        .replace_all(text, |caps: &regex::Captures| {
            let matched = caps.get(0).expect("capture group 0 always present");
            if inside_markdown_fence(text, matched.start()) {
                matched.as_str().to_string()
            } else {
                String::new()
            }
        })
        .to_string()
}

fn looks_like_internal_verdict_object(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    let Some(verdict) = map
        .get("verdict")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };

    let verdict = verdict.to_ascii_lowercase();
    let has_completion_explanation = map.contains_key("reasoning")
        || map.contains_key("reason")
        || map.contains_key("next_step")
        || map.contains_key("nextStep");
    let has_judge_metadata = map.contains_key("critique")
        || map.contains_key("confidence")
        || map.contains_key("category")
        || map.contains_key("error");

    let known_internal_verdict = matches!(verdict.as_str(), "done" | "continue")
        && has_completion_explanation
        || matches!(verdict.as_str(), "revise" | "pass" | "fail" | "unclear") && has_judge_metadata
        || matches!(verdict.as_str(), "allow" | "warn" | "block") && has_judge_metadata;
    if !known_internal_verdict {
        return false;
    }

    map.keys().all(|key| {
        matches!(
            key.as_str(),
            "verdict"
                | "reasoning"
                | "reason"
                | "next_step"
                | "nextStep"
                | "critique"
                | "confidence"
                | "category"
                | "error"
        )
    })
}

fn looks_like_bare_internal_verdict_json(source: &str) -> bool {
    let trimmed = source.trim();
    if !trimmed.starts_with('{') {
        return false;
    }

    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(trimmed)
    else {
        return false;
    };

    looks_like_internal_verdict_object(&map)
}

fn internal_verdict_json_prefix_len(source: &str) -> Option<usize> {
    let trimmed = source.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let leading_ws = source.len() - trimmed.len();
    let mut stream = serde_json::Deserializer::from_str(trimmed).into_iter::<serde_json::Value>();
    let parsed = stream.next()?.ok()?;
    let serde_json::Value::Object(map) = parsed else {
        return None;
    };
    if !looks_like_internal_verdict_object(&map) {
        return None;
    }
    Some(leading_ws + stream.byte_offset())
}

fn strip_bare_internal_json(text: &str) -> String {
    // A finalized turn whose entire visible body is an internal control object
    // — e.g. the completion judge's `{"verdict":...,"reasoning":...}` — must
    // never surface as the agent's message. The fenced/inline planner strips
    // above only catch ```json fences and `{"mode":...}`; a bare top-level
    // verdict/reasoning blob slips through. Keep this narrower than
    // `looks_like_internal_planning_json`: user-facing JSON-only answers can
    // legitimately contain keys like `tasks`, `steps`, or `reasoning`, and the
    // visible-text sanitizer must not blank those whole messages.
    if looks_like_bare_internal_verdict_json(text) {
        return String::new();
    }
    text.to_string()
}

fn strip_leading_done_marker_control(text: &str) -> String {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    for marker in ["</done>", "<done>", "/done>", "done>"] {
        if let Some(after_marker) = trimmed.strip_prefix(marker) {
            let after_marker = after_marker.trim_start();
            let visible_start = internal_verdict_json_prefix_len(after_marker).unwrap_or(0);
            return text[..leading_ws].to_string() + after_marker[visible_start..].trim_start();
        }
    }
    text.to_string()
}

fn strip_trailing_internal_json(text: &str) -> String {
    let trimmed = text.trim_end();
    for (idx, ch) in trimmed.char_indices().rev() {
        if ch == '{' && looks_like_bare_internal_verdict_json(&trimmed[idx..]) {
            return trimmed[..idx].trim_end().to_string();
        }
    }
    text.to_string()
}

fn strip_partial_marker_suffix(text: &str) -> String {
    const MARKERS: [&str; 13] = [
        "<|tool_call|>",
        TEXT_TOOL_CALL_OPEN,
        TEXT_TOOL_CALL_OPEN_COMPACT,
        "<assistant_prose>",
        "<assistantprose>",
        "<user_response>",
        "<userresponse>",
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
                if is_protocol_tag_position(text, stripped.len()) {
                    return stripped.to_string();
                }
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
    // After runtime tags are stripped, surface only the explicit
    // user-facing response when one exists; otherwise unwrap
    // <assistant_prose> into plain narration.
    sanitized = extract_visible_prose(&sanitized);
    sanitized = strip_internal_json_fences(&sanitized);
    sanitized = strip_inline_internal_planning_json(&sanitized, partial);
    // Unconditional: orphan/truncated control-token residue and bare internal
    // control JSON leak into FINAL transcripts too, where the partial-only
    // strippers below never run. Bare-JSON check runs on the trimmed body so a
    // verdict blob surrounded by whitespace is still recognized.
    sanitized = strip_protocol_residue(&sanitized);
    sanitized = strip_leading_done_marker_control(&sanitized);
    sanitized = strip_trailing_internal_json(&sanitized);
    sanitized = strip_bare_internal_json(sanitized.trim());
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

    #[test]
    fn sanitize_strips_orphan_tool_call_residue_and_truncations() {
        // Real leak: weak/GLM models emit truncated `</tool_call>` fragments as
        // standalone visible text. None match the well-formed block patterns.
        assert_eq!(sanitize_visible_assistant_text("_call>", false), "");
        assert_eq!(sanitize_visible_assistant_text("l_call>l_call>", false), "");
        assert_eq!(
            sanitize_visible_assistant_text("Done.\n})\n</tool_call>_call>", false),
            "Done.\n})"
        );
        assert_eq!(
            sanitize_visible_assistant_text("Implemented.</assistant_prose>", false),
            "Implemented."
        );
        // `_prose>` close-tag truncation (no opening tag) is also litter.
        assert_eq!(
            sanitize_visible_assistant_text("Implemented.\nnt_prose>", false),
            "Implemented."
        );
        // Fence-aware: a fenced example showing the tag is preserved verbatim.
        let fenced = "```\n</tool_call>\n```\nDone.";
        assert_eq!(sanitize_visible_assistant_text(fenced, false), fenced);
    }

    #[test]
    fn sanitize_does_not_touch_ordinary_prose_or_inequalities() {
        // Guard against over-eager residue stripping.
        let raw = "Use a_call> only as— wait, compare x > y and y > z here.";
        // `a_call>` IS residue-shaped (`_call>` truncation); ensure the rest survives.
        let out = sanitize_visible_assistant_text(raw, false);
        assert!(out.contains("compare x > y and y > z here."), "got: {out}");
        assert_eq!(
            sanitize_visible_assistant_text("The phrase tool call is normal prose.", false),
            "The phrase tool call is normal prose."
        );
    }

    #[test]
    fn sanitize_drops_bare_completion_judge_verdict_json() {
        let raw = r#"{"verdict":"done","reasoning":"All tests pass.","next_step":""}"#;
        assert_eq!(sanitize_visible_assistant_text(raw, false), "");
        // A bare verdict blob surrounded by whitespace is still recognized.
        let padded = "\n  {\"verdict\":\"continue\",\"reasoning\":\"does not compile\"}  \n";
        assert_eq!(sanitize_visible_assistant_text(padded, false), "");
        // Legitimate non-internal JSON is preserved (consistent with existing behavior).
        let keep = r#"{"status":"ok","message":"hello"}"#;
        assert_eq!(sanitize_visible_assistant_text(keep, false), keep);
        // Guard against blanking legitimate JSON-only answers that happen to
        // use broad planning-ish keys. The bare verdict sanitizer is scoped to
        // small internal control envelopes, not arbitrary structured answers.
        let visible_answer =
            r#"{"tasks":["ship"],"steps":["test"],"reasoning":"user-visible rationale"}"#;
        assert_eq!(
            sanitize_visible_assistant_text(visible_answer, false),
            visible_answer
        );
        let visible_verdict = r#"{"verdict":"pass","summary":"public result"}"#;
        assert_eq!(
            sanitize_visible_assistant_text(visible_verdict, false),
            visible_verdict
        );
        let visible_verdict_rationale = r#"{"verdict":"pass","reasoning":"public rationale"}"#;
        assert_eq!(
            sanitize_visible_assistant_text(visible_verdict_rationale, false),
            visible_verdict_rationale
        );
    }

    #[test]
    fn sanitize_drops_appended_completion_judge_verdict_json() {
        let raw = r#"What can I help with today?{"verdict":"done","reasoning":"greeting","next_step":""}"#;
        assert_eq!(
            sanitize_visible_assistant_text(raw, false),
            "What can I help with today?"
        );
        let visible_json = r#"Visible answer {"status":"ok","message":"hello"}"#;
        assert_eq!(
            sanitize_visible_assistant_text(visible_json, false),
            visible_json
        );
    }

    #[test]
    fn sanitize_drops_done_marker_prefixed_internal_control() {
        let raw = r#"/done>{"verdict":"continue","reasoning":"needs final"}Visible answer."#;
        assert_eq!(
            sanitize_visible_assistant_text(raw, false),
            "Visible answer."
        );
        assert_eq!(
            sanitize_visible_assistant_text(r#"done>{"verdict":"done","reasoning":"done"}"#, false),
            ""
        );
        let inline = "The literal /done> marker can be mentioned inline.";
        assert_eq!(sanitize_visible_assistant_text(inline, false), inline);
    }

    #[test]
    fn sanitize_prefers_user_response_blocks_over_other_prose() {
        let raw = "Working...\n<assistant_prose>internal narration</assistant_prose>\n<user_response>Visible answer.</user_response>\n##DONE##";
        assert_eq!(
            sanitize_visible_assistant_text(raw, false),
            "Visible answer."
        );
    }

    #[test]
    fn sanitize_strips_trailing_runtime_sentinel_after_answer_text() {
        assert_eq!(
            sanitize_visible_assistant_text("HARN_LOCAL_TOOL_OK##DONE##", false),
            "HARN_LOCAL_TOOL_OK"
        );
        assert_eq!(
            sanitize_visible_assistant_text("Done.\nPLAN_READY", false),
            "Done."
        );
    }

    #[test]
    fn sanitize_accepts_compact_protocol_tag_aliases_without_hiding_plain_words() {
        let raw = "The phrase tool call is normal prose.\n<assistantprose>hidden</assistantprose>\n<toolcall>\nrun({ command: \"git status\" })\n</toolcall>\n<userresponse>Visible answer.</userresponse>\n<done>##DONE##</done>";
        assert_eq!(
            sanitize_visible_assistant_text(raw, false),
            "Visible answer."
        );

        assert_eq!(
            sanitize_visible_assistant_text("A tool call summary is fine.", false),
            "A tool call summary is fine."
        );
    }

    #[test]
    fn sanitize_ignores_inline_user_response_placeholder() {
        let raw = "Wrap final answers in `<user_response>...</user_response>`.\nAudit: real answer";
        assert_eq!(sanitize_visible_assistant_text(raw, false), raw);
    }

    #[test]
    fn sanitize_prefers_top_level_user_response_over_inline_placeholder() {
        let raw =
            "Remember `<user_response>...</user_response>` is the wrapper.\n<user_response>Visible answer.</user_response>";
        assert_eq!(
            sanitize_visible_assistant_text(raw, false),
            "Visible answer."
        );
    }

    #[test]
    fn sanitize_ignores_user_response_inside_markdown_fence() {
        let raw = "```xml\n<user_response>example only</user_response>\n```\nFinal plain answer.";
        assert_eq!(sanitize_visible_assistant_text(raw, false), raw);
    }

    #[test]
    fn sanitize_partial_keeps_inline_protocol_prefixes() {
        let raw = "Mention `<user_resp";
        assert_eq!(sanitize_visible_assistant_text(raw, true), raw);
    }

    #[test]
    fn sanitize_partial_hides_top_level_protocol_prefixes() {
        assert_eq!(sanitize_visible_assistant_text("<user_resp", true), "");
    }
}
