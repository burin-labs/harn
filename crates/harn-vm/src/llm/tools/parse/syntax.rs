use std::sync::OnceLock;

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

/// Strip `<tool_call>`/`</tool_call>` (and the compact `<toolcall>` spelling)
/// wrapper tags from a bare-mode body, replacing each with a newline.
///
/// Text-format models emit these wrappers unpredictably even when the prompt
/// asks for bare `name({ ... })` calls (OpenRouter `qwen/qwen3-coder` does this
/// on most turns). Without stripping, two failures occur in the bare scanner:
///   1. `<tool_call>run({...})</tool_call>` on one line hides the call —
///      `run(` is not at a line start, so the scanner never recognizes it and
///      the whole turn comes back with zero tool calls.
///   2. A trailing `</tool_call>` (or leading `<tool_call>`) on its own line is
///      not a call, so it leaks into the visible prose as a `</tool_call>` /
///      `_call>` fragment.
///
/// Replacing each tag token with `\n` fixes both: the inner call lands at a
/// line start and the wrapper bytes never reach `prose`. Returns a borrowed
/// `Cow` unchanged when no wrapper tags are present.
pub(super) fn strip_tool_call_wrappers(text: &str) -> std::borrow::Cow<'_, str> {
    use super::super::{
        TEXT_TOOL_CALL_CLOSE, TEXT_TOOL_CALL_CLOSE_COMPACT, TEXT_TOOL_CALL_OPEN,
        TEXT_TOOL_CALL_OPEN_COMPACT,
    };
    const TAGS: [&str; 4] = [
        TEXT_TOOL_CALL_OPEN,
        TEXT_TOOL_CALL_CLOSE,
        TEXT_TOOL_CALL_OPEN_COMPACT,
        TEXT_TOOL_CALL_CLOSE_COMPACT,
    ];
    if !TAGS.iter().any(|tag| text.contains(tag)) {
        return std::borrow::Cow::Borrowed(text);
    }
    // Replace each wrapper tag with a newline, but copy `<<TAG ... TAG` heredoc
    // bodies through verbatim: a wrapper-tag literal inside a multiline string
    // argument is file content, not structure, and stripping it would corrupt
    // the value. This is the same heredoc-blindness `find_close_tag` avoids at
    // the block boundary, applied here at the wrapper-stripping boundary.
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if bytes[i] == b'<' && bytes.get(i + 1) == Some(&b'<') {
            if let Some(after) = skip_heredoc_body(text, i) {
                out.push_str(&text[i..after]);
                i = after;
                continue;
            }
        }
        if let Some(tag) = TAGS.iter().find(|tag| text[i..].starts_with(**tag)) {
            out.push('\n');
            i += tag.len();
            continue;
        }
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    std::borrow::Cow::Owned(out)
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

/// Parse a TypeScript-ish object literal starting at the beginning of `text`.
/// Returns the parsed object and bytes consumed through the closing `}`.
pub(super) fn parse_object_literal_from(
    text: &str,
    name: &str,
) -> Result<(serde_json::Value, usize), String> {
    let mut parser = TsValueParser::new(text);
    parser.skip_ws_and_comments();
    let value = parser.parse_value().map_err(|error| {
        format!(
            "TOOL CALL PARSE ERROR: `{name}{{...}}` — {error}. \
             Tool arguments must be a TypeScript object literal."
        )
    })?;
    match value {
        serde_json::Value::Object(map) => Ok((serde_json::Value::Object(map), parser.position())),
        other => Err(format!(
            "TOOL CALL PARSE ERROR: `{name}{{...}}` — expected an object literal argument, got `{other}`."
        )),
    }
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
    static EMPTY_FENCE_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = EMPTY_FENCE_RE.get_or_init(|| {
        regex::Regex::new(r"(?m)^[ \t]*```[^\n]*\n\s*```[ \t]*\n?")
            .expect("strip_empty_fences regex is statically valid")
    });
    re.replace_all(text, "").to_string()
}

/// A located `<<TAG ... TAG` heredoc body.
pub(crate) struct HeredocSpan {
    /// Byte range of the body content, with the trailing newline excluded
    /// (matching `Parser::parse_heredoc`'s returned string).
    pub content: std::ops::Range<usize>,
    /// Byte offset immediately after the closing tag on its line.
    pub end: usize,
}

/// Why a `<<` opener is not a complete heredoc. Carries the tag where one was
/// read so the value parser can reproduce its precise model-facing diagnostics.
pub(crate) enum HeredocError {
    /// `<<` was not followed by an identifier tag (e.g. a bare shift operator).
    MissingTag,
    /// The opening `<<TAG` line was not terminated by a newline.
    MissingNewline { tag: String },
    /// End of input reached before a line opening with the closing tag.
    Unterminated { tag: String },
}

/// The single authority for the `<<TAG\n...\nTAG` heredoc grammar shared by the
/// TS value parser (`Parser::parse_heredoc`) and the top-level chunker
/// (`skip_heredoc_body`). `start` must sit on the opening `<<`. The tag is any
/// run of `[A-Za-z0-9_]`, optionally wrapped in `'`/`"`; the body runs to a
/// line that — after leading whitespace — begins with the tag at a word
/// boundary. Anything after the tag on the closing line is left to the caller.
pub(crate) fn scan_heredoc(src: &str, start: usize) -> Result<HeredocSpan, HeredocError> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'<') || bytes.get(start + 1) != Some(&b'<') {
        return Err(HeredocError::MissingTag);
    }
    let mut pos = start + 2;
    let quote_char = bytes.get(pos).copied();
    let has_quote = matches!(quote_char, Some(b'\'') | Some(b'"'));
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
        return Err(HeredocError::MissingTag);
    }
    let tag = src[tag_start..pos].to_string();
    if has_quote && bytes.get(pos).copied() == quote_char {
        pos += 1;
    }
    if bytes.get(pos) == Some(&b'\r') {
        pos += 1;
    }
    if bytes.get(pos) != Some(&b'\n') {
        return Err(HeredocError::MissingNewline { tag });
    }
    pos += 1;
    let content_start = pos;
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
        if let Some(rest) = after_ws.strip_prefix(&tag) {
            let at_word_boundary = rest
                .chars()
                .next()
                .is_none_or(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'));
            if at_word_boundary {
                let raw = &src[content_start..line_start];
                let stripped = raw.strip_suffix('\n').unwrap_or(raw);
                let stripped = stripped.strip_suffix('\r').unwrap_or(stripped);
                return Ok(HeredocSpan {
                    content: content_start..content_start + stripped.len(),
                    end: line_start + leading_ws_len + tag.len(),
                });
            }
        }
        if bytes.get(pos) == Some(&b'\n') {
            pos += 1;
        } else {
            return Err(HeredocError::Unterminated { tag });
        }
    }
    Err(HeredocError::Unterminated { tag })
}

/// Skip past a `<<TAG\n...\nTAG` heredoc body starting at `start` in `src`.
/// Returns the byte position immediately after the closing tag, or `None` when
/// the heredoc is malformed or unterminated. Used by the top-level scanner so a
/// stray-bytes chunker doesn't truncate bare `name({ key: <<EOF\n...\nEOF })`
/// tool calls at the `<<` opener.
pub(super) fn skip_heredoc_body(src: &str, start: usize) -> Option<usize> {
    scan_heredoc(src, start).ok().map(|span| span.end)
}

/// Outcome of searching for a tag while stepping over heredoc bodies.
pub(super) enum CloseScan {
    /// The tag begins at this byte offset, outside any heredoc body.
    Found(usize),
    /// A `<<TAG` heredoc opened but its closing tag line hasn't arrived yet —
    /// the streaming caller must wait for more input; a buffered caller treats
    /// this as "no usable close" (the block is truncated mid-heredoc).
    NeedMore,
    /// Scanned to the end without finding the tag outside a heredoc.
    NotFound,
}

/// Find `needle` in `src[from..]`, stepping over complete `<<TAG ... TAG`
/// heredoc bodies so a literal occurrence inside one (the protocol asks models
/// to write multiline string arguments as heredocs) is ignored. A heredoc whose
/// body is still incomplete yields [`CloseScan::NeedMore`]. This is the one
/// place that knows "where does a tagged block really end", shared by the
/// buffered matcher, the truncation detector, and the streaming scanner.
pub(super) fn find_close_tag(src: &str, from: usize, needle: &str) -> CloseScan {
    let bytes = src.as_bytes();
    let mut i = from;
    while i < src.len() {
        if bytes[i] == b'<' && bytes.get(i + 1) == Some(&b'<') {
            match scan_heredoc(src, i) {
                Ok(span) => {
                    i = span.end;
                    continue;
                }
                Err(HeredocError::MissingNewline { .. })
                | Err(HeredocError::Unterminated { .. }) => {
                    return CloseScan::NeedMore;
                }
                // Not a heredoc (bare `<<`); fall through and treat as content.
                Err(HeredocError::MissingTag) => {}
            }
        }
        if src[i..].starts_with(needle) {
            return CloseScan::Found(i);
        }
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
    CloseScan::NotFound
}

/// Heredoc-aware variant of [`match_block`] for the `<tool_call>` tags: it skips
/// `<<TAG ... TAG` bodies when locating `</tool_call>`, so a literal close tag
/// inside a heredoc argument doesn't shred the call. Scoped to the tool-call
/// tags — `match_block` stays a cheap `find` for the prose/done blocks that
/// never carry heredocs.
pub(super) fn match_tool_call_block<'a>(
    src: &'a str,
    start: usize,
    tag: &str,
) -> Option<(&'a str, usize)> {
    let open = format!("<{tag}>");
    if !src[start..].starts_with(&open) {
        return None;
    }
    let body_start = start + open.len();
    let close = format!("</{tag}>");
    match find_close_tag(src, body_start, &close) {
        CloseScan::Found(idx) => Some((&src[body_start..idx], idx + close.len())),
        CloseScan::NeedMore | CloseScan::NotFound => None,
    }
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
pub(crate) fn parse_ts_call_from(
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
             got `{other}`. Wrap the value in braces: `{name}({{ key: value }})`."
        )),
    }
}
