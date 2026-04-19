use super::super::ts_value_parser::TsValueParser;

/// Strip leaked thinking tags from model output. Some models (Qwen, Gemma)
/// emit `</think>` or `<think>` markers in their response text when the
/// streaming transport merges thinking and content channels. These tags
/// break tool-call parsing because they appear between or before valid
/// tool invocations.
pub(super) fn strip_thinking_tags(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("<think>") && !text.contains("</think>") {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut result = text.to_string();
    while let Some(start) = result.find("<think>") {
        if let Some(end) = result[start..].find("</think>") {
            result.replace_range(start..start + end + "</think>".len(), "");
        } else {
            result.replace_range(start..start + "<think>".len(), "");
        }
    }
    while result.contains("</think>") {
        result = result.replace("</think>", "");
    }
    std::borrow::Cow::Owned(result)
}

/// Match a balanced `<tag>...</tag>` block starting at `start` in `src`.
/// Returns `(body_slice, end_cursor)` on success. Does not support nested
/// same-name tags — not needed for this grammar and attempting to support
/// them bloats the error surface for no real benefit.
pub(super) fn match_block<'a>(src: &'a str, start: usize, tag: &str) -> Option<(&'a str, usize)> {
    let open = format!("<{tag}>");
    if !src[start..].starts_with(&open) {
        return None;
    }
    let body_start = start + open.len();
    let close = format!("</{tag}>");
    let close_idx = src[body_start..].find(&close)?;
    let body_end = body_start + close_idx;
    let after = body_end + close.len();
    Some((&src[body_start..body_end], after))
}

/// Render a parsed tool call back to the bare TS syntax used inside
/// `<tool_call>` tags. Used to build the canonical history entry.
pub(super) fn render_canonical_call(name: &str, args: &serde_json::Value) -> String {
    // JSON object literals are accepted by our tool-call grammar, so
    // pretty-printed JSON is sufficient for replay.
    let rendered_args = serde_json::to_string_pretty(args).unwrap_or_else(|_| "{}".to_string());
    format!("{name}({rendered_args})")
}

pub(super) fn preview_str(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let kept: String = chars.into_iter().take(max).collect();
    format!("{kept}…")
}

pub(super) fn has_object_literal_arg_start(text: &str, open_paren_idx: usize) -> bool {
    let bytes = text.as_bytes();
    let mut idx = open_paren_idx;
    while idx < bytes.len() && (bytes[idx] == b' ' || bytes[idx] == b'\t') {
        idx += 1;
    }
    bytes.get(idx) == Some(&b'{')
}

pub(super) fn unwrap_exact_code_wrapper(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let newline = rest.find('\n')?;
        let after_opener = &rest[newline + 1..];
        let inner = after_opener.strip_suffix("```")?;
        return Some(inner.trim());
    }
    let inner = trimmed.strip_prefix('`')?.strip_suffix('`')?;
    if inner.contains('`') {
        return None;
    }
    Some(inner.trim())
}

/// Collapse runs of ≥3 consecutive newlines down to 2 (one blank line). Used
/// to tidy the `prose` output after tool-call ranges are excised, so the
/// removed bytes don't leave an ugly vertical gap between surrounding prose.
pub(super) fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newline_run = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push(ch);
            }
        } else {
            newline_run = 0;
            out.push(ch);
        }
    }
    out
}

/// Strip empty Markdown fence pairs (```lang\n``` or ```lang\n\n```) from text.
/// Models sometimes emit these as failed tool-call attempts. If left in prose
/// they accumulate in conversation history and cause duplication loops.
pub(super) fn strip_empty_fences(text: &str) -> String {
    let re = regex::Regex::new(r"(?m)^[ \t]*```[^\n]*\n\s*```[ \t]*\n?").unwrap();
    re.replace_all(text, "").to_string()
}

/// Skip past a `<<TAG\n...\nTAG` heredoc body starting at `start` in `src`.
/// Returns the byte position immediately after the closing tag (mirroring
/// `Parser::parse_heredoc`'s rewind), or `None` when the heredoc is malformed
/// or unterminated. Used by the top-level scanner so a stray-bytes chunker
/// doesn't truncate bare `name({ key: <<EOF\n...\nEOF })` tool calls at the
/// `<<` opener.
pub(super) fn skip_heredoc_body(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'<') || bytes.get(start + 1) != Some(&b'<') {
        return None;
    }
    let mut pos = start + 2;
    let has_quote = matches!(bytes.get(pos), Some(b'\'') | Some(b'"'));
    let quote_char = bytes.get(pos).copied();
    if has_quote {
        pos += 1;
    }
    let tag_start = pos;
    while let Some(byte) = bytes.get(pos) {
        if byte.is_ascii_alphanumeric() || *byte == b'_' {
            pos += 1;
        } else {
            break;
        }
    }
    if pos == tag_start {
        return None;
    }
    let tag = &src[tag_start..pos];
    if has_quote && bytes.get(pos).copied() == quote_char {
        pos += 1;
    }
    if bytes.get(pos) == Some(&b'\r') {
        pos += 1;
    }
    if bytes.get(pos) != Some(&b'\n') {
        return None;
    }
    pos += 1;
    while pos < bytes.len() {
        let line_start = pos;
        while let Some(byte) = bytes.get(pos) {
            if *byte == b'\n' {
                break;
            }
            pos += 1;
        }
        let line = &src[line_start..pos];
        let leading_ws_len = line.len() - line.trim_start().len();
        let after_ws = &line[leading_ws_len..];
        if let Some(rest) = after_ws.strip_prefix(tag) {
            let at_word_boundary = rest
                .chars()
                .next()
                .is_none_or(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'));
            if at_word_boundary {
                return Some(line_start + leading_ws_len + tag.len());
            }
        }
        if bytes.get(pos) == Some(&b'\n') {
            pos += 1;
        } else {
            return None;
        }
    }
    None
}

/// Length of a JavaScript-ish identifier starting at bytes[0]. Returns None
/// if the first byte is not a valid identifier start.
pub(crate) fn ident_length(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() {
        return None;
    }
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' {
            i += 1;
        } else {
            break;
        }
    }
    Some(i)
}

/// Parse a full `name(args)` TS call expression starting at the beginning of
/// `text`. Returns the parsed argument JSON and the number of bytes consumed
/// (from the start of the name through the closing paren), or an error with
/// a diagnostic suitable to show the model.
pub(super) fn parse_ts_call_from(
    text: &str,
    name: String,
) -> Result<(serde_json::Value, usize), String> {
    let bytes = text.as_bytes();
    let paren_open = name.len();
    if bytes.get(paren_open) != Some(&b'(') {
        return Err(format!(
            "TOOL CALL PARSE ERROR: `{name}(` expected immediately after the tool name."
        ));
    }
    let mut parser = TsValueParser::new(&text[paren_open + 1..]);
    parser.skip_ws_and_comments();
    // An empty arg list `name()` is legal and produces an empty object.
    let args_value = if parser.peek() == Some(b')') {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        parser.parse_value().map_err(|error| {
            format!(
                "TOOL CALL PARSE ERROR: `{name}(...)` — {error}. \
                 Tool arguments must be a TypeScript object literal: `{{ key: value, key: value }}`."
            )
        })?
    };
    parser.skip_ws_and_comments();
    if parser.peek() != Some(b')') {
        return Err(format!(
            "TOOL CALL PARSE ERROR: `{name}(...)` — missing closing `)`. \
             Every tool call must be a complete TypeScript expression."
        ));
    }
    let consumed_in_parser = parser.position();
    let total_consumed = paren_open + 1 + consumed_in_parser + 1; // +1 for the ')'

    // Tool contract: every call takes a single object literal. Bare
    // positional scalars error precisely rather than being promoted.
    match args_value {
        serde_json::Value::Object(map) => Ok((serde_json::Value::Object(map), total_consumed)),
        other => Err(format!(
            "TOOL CALL PARSE ERROR: `{name}(...)` — expected an object literal argument, \
             got `{}`. Wrap the value in braces: `{name}({{ key: value }})`.",
            other
        )),
    }
}
