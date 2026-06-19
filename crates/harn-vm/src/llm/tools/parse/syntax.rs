use std::collections::BTreeSet;
use std::sync::OnceLock;

use super::super::ts_value_parser::TsValueParser;

/// Identifiers a model commonly emits as raw source/test code at the start of a
/// line — `it(...)`, `expect(...)`, `describe(...)`, `assertServiceCount(...)`,
/// etc. When one of these appears where a tool call is expected, the real cause
/// is "code emitted outside a heredoc/content envelope," NOT "the model called
/// an unknown tool." Naming the wrong cause (`Unknown tool 'it'`) gives the
/// model no signal to re-wrap the body, so several eval transcripts show it
/// re-emitting the same code and re-failing. We treat a name as source code
/// when it is one of these well-known non-tool identifiers.
const SOURCE_CODE_IDENTIFIERS: &[&str] = &[
    // JS/TS test frameworks (jest, mocha, vitest, jasmine).
    "it",
    "test",
    "describe",
    "expect",
    "beforeEach",
    "afterEach",
    "beforeAll",
    "afterAll",
    "suite",
    "context",
    // Common assertion helpers (custom + library).
    "assert",
    "assertEquals",
    "assertEqual",
    "assertTrue",
    "assertFalse",
    "assertThat",
    "require",
    // Generic source-ish calls models leak as bare lines.
    "console",
    "print",
    "println",
    "printf",
    "fmt",
    "func",
    "function",
    "return",
    "if",
    "for",
    "while",
    "class",
    "def",
];

/// True when `name` is a project-specific custom assertion the model is likely
/// writing as test source (e.g. `assertServiceCount(...)`). We match the common
/// `assert*`/`expect*`/`check*`/`verify*` test-helper prefixes (camelCase, so
/// the char after the prefix is uppercase) to catch the long tail of bespoke
/// helpers without hardcoding every project's names.
fn looks_like_test_helper(name: &str) -> bool {
    for prefix in ["assert", "expect", "check", "verify", "should", "mock"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            if rest.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                return true;
            }
        }
    }
    false
}

/// True when `name` looks like the model's own source/test code rather than a
/// (mistyped) tool call. Used to route the feedback to the heredoc-envelope
/// message instead of a misleading "Unknown tool" message.
pub(super) fn looks_like_source_code(name: &str) -> bool {
    SOURCE_CODE_IDENTIFIERS.contains(&name) || looks_like_test_helper(name)
}

/// High-frequency tool-name misses where the right answer is always the same
/// canonical call. The #1 real miss across eval transcripts is `read` (271×),
/// where the answer is `look({ intent: "read" })`. Sourced here as a small
/// static table because the canonical replacement is a full call shape, not a
/// rename the live registry can express.
fn tool_alias_hint(name: &str) -> Option<&'static str> {
    match name {
        "read" | "read_file" | "readFile" | "cat" | "open" => {
            Some("Use `look({ intent: \"read\", ... })` to read a file.")
        }
        "write" | "write_file" | "writeFile" | "create_file" => {
            Some("Use `edit({ action: \"create\", ... })` to write a file.")
        }
        // Cross-harness edit aliases cheap models emit by habit (OpenAI/Codex
        // `apply_patch`, Anthropic-style `str_replace`/`str_replace_editor`,
        // generic `edit_file`). All map to Harn's single `edit` tool; the
        // specific `action` is host-defined, so we point at `edit` without
        // prescribing one rather than naming an action the host may not have.
        "apply_patch" | "str_replace" | "str_replace_editor" | "edit_file" | "editFile" => {
            Some("Use the `edit({ ... })` tool to modify a file.")
        }
        "list" | "ls" | "list_files" => {
            Some("Use `look({ intent: \"list\", ... })` to list files.")
        }
        "search" | "grep" | "find" => {
            Some("Use `look({ intent: \"search\", ... })` to search the codebase.")
        }
        _ => None,
    }
}

/// Render the full available-tool list, sorted and never silently truncated.
/// The previous `known.iter().take(20)` cap could hide the very tool the model
/// needed (225 transcripts), so we list every tool. If the registry is ever
/// pathologically large we keep the head and append an explicit `…and N more`
/// marker rather than dropping names without a trace.
fn render_available_tools(known: &BTreeSet<String>) -> String {
    const CAP: usize = 60;
    if known.len() <= CAP {
        return known.iter().cloned().collect::<Vec<_>>().join(", ");
    }
    let head = known.iter().take(CAP).cloned().collect::<Vec<_>>();
    format!("{}, …and {} more", head.join(", "), known.len() - CAP)
}

/// Build the model-facing feedback for a name that parsed like a tool call
/// (`name({ ... })`) but is not a registered tool. Routes to the most
/// actionable message: a close-miss typo suggestion, a known-alias hint
/// (e.g. `read` → `look`), a "this is source code, wrap it in a heredoc"
/// message when the name looks like the model's own code, or a plain
/// unknown-tool listing. Shared by the bare and native-JSON parsers so both
/// surfaces give the same precise {what/why/how-to-fix} guidance.
pub(super) fn unknown_tool_feedback(name: &str, known: &BTreeSet<String>) -> String {
    let available = render_available_tools(known);

    // Genuine close-miss typo (e.g. `edt` → `edit`): keep the existing
    // suggestion behavior, which is the most likely intent.
    if let Some(suggestion) = crate::value::closest_match(name, known.iter().map(String::as_str)) {
        return format!(
            "Unknown tool '{name}'. Did you mean '{suggestion}'? \
             Tool calls must be one of: [{available}]."
        );
    }

    // High-frequency alias where the real tool is a specific call shape.
    if let Some(hint) = tool_alias_hint(name) {
        return format!("Unknown tool '{name}'. {hint} Tool calls must be one of: [{available}].");
    }

    // Not close to any real tool AND it looks like the model's own source/test
    // code: name the real cause so the model re-wraps it instead of re-emitting
    // the same code as a "tool call."
    if looks_like_source_code(name) {
        return format!(
            "`{name}(...)` looks like source code, not a tool call. If this is file \
             content, wrap it in a heredoc or a string `content` value (e.g. \
             `edit({{ action: \"create\", path: ..., content: <<EOF ... EOF }})`). \
             Tool calls must be one of: [{available}]."
        );
    }

    format!("Unknown tool '{name}'. Tool calls must be one of: [{available}].")
}

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
    // Replace each wrapper tag with a newline, but copy quoted string spans and
    // `<<TAG ... TAG` heredoc bodies through verbatim: a wrapper-tag literal
    // inside a string/heredoc argument is file content, not structure, and
    // stripping it would corrupt the value. Same content-vs-structure rule
    // `find_close_tag` applies at the block boundary, here at the
    // wrapper-stripping boundary.
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if matches!(bytes[i], b'"' | b'\'' | b'`') {
            if let Some(after) = skip_string_span(text, i) {
                out.push_str(&text[i..after]);
                i = after;
                continue;
            }
        }
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
        // Include a short preview of the offending span so the model can tell
        // which of several on-screen calls failed (the native-JSON parser
        // already shows `Raw: …`; mirror it here for object-literal failures).
        format!(
            "TOOL CALL PARSE ERROR: `{name}{{...}}` — {error}. \
             Tool arguments must be a TypeScript object literal. Raw: {}",
            preview_str(text, 200)
        )
    })?;
    match value {
        serde_json::Value::Object(map) => Ok((serde_json::Value::Object(map), parser.position())),
        other => Err(format!(
            "TOOL CALL PARSE ERROR: `{name}{{...}}` — expected an object literal argument, \
             got `{other}`. Raw: {}",
            preview_str(text, 200)
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
    /// True when the heredoc body used literal JSON/string escape sequences
    /// (`\n`, `\t`, ...) as line separators instead of real newlines — the
    /// degraded form cheap models emit when they treat the heredoc body as a
    /// one-line JSON string. The caller must unescape `content` before use.
    /// A body that used real newlines (the normal case) is always `false`.
    pub escaped: bool,
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
        // Degraded form: cheap models (e.g. qwen3.6) JSON-escape the heredoc
        // body, so the line break after the tag is the two literal bytes
        // backslash + 'n' rather than a real `\n`. Recover those calls by
        // scanning the escaped body to a literal-`\n`-delimited closing tag
        // line; the caller unescapes `content`. A genuinely-truncated opener
        // (`<<EOF` then end-of-input, a real shift operator, etc.) still hits
        // the original MissingNewline error below.
        if bytes.get(pos) == Some(&b'\\') && bytes.get(pos + 1) == Some(&b'n') {
            return scan_escaped_heredoc_body(src, pos, tag);
        }
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
                    escaped: false,
                });
            }
        }
        if bytes.get(pos) == Some(&b'\n') {
            pos += 1;
        } else {
            // Last physical line, no trailing newline, not the closing tag.
            return unterminated_or_implicit_close(src, content_start, tag);
        }
    }
    unterminated_or_implicit_close(src, content_start, tag)
}

/// Recover a heredoc whose closing tag the model botched (indented, misspelled,
/// or omitted) but which is otherwise a complete, structurally-closed call.
///
/// Value models routinely write `new_body: <<EOF\n...body...\n})` — the call's
/// own `}`/`)` close it, but the `EOF` line is missing. The corpus shows ~2,400
/// of these "sloppy terminator" shapes against ~1 genuine max-token truncation,
/// so dropping them all wastes a turn the model usually repeats verbatim.
///
/// The distinguisher is sharp: we treat the body as implicitly closed ONLY when
/// its final non-blank line is a pure structural call-tail — after leading
/// whitespace, nothing but `}` / `)` / `]` / `,` AND at least one `)`. That `)`
/// is the tool call's own closing paren, which an escape-free heredoc *body*
/// would not place on a standalone final line. A bare `}`-only final line
/// (ordinary Go/Rust code) is ambiguous and is left to error, and a body cut
/// off mid-token (no structural tail at all) still reports `Unterminated`. So a
/// genuinely truncated/unclosed body never silently dispatches.
fn unterminated_or_implicit_close(
    src: &str,
    content_start: usize,
    tag: String,
) -> Result<HeredocSpan, HeredocError> {
    let body = &src[content_start..];
    // Find the last non-blank line and where it starts within `body`.
    let mut last_line_start = body.len();
    for (offset, line) in line_starts(body) {
        if !line.trim().is_empty() {
            last_line_start = offset;
        }
    }
    if last_line_start >= body.len() {
        return Err(HeredocError::Unterminated { tag });
    }
    let last_line = body[last_line_start..].trim_end();
    let after_ws = last_line.trim_start();
    let is_call_tail = !after_ws.is_empty()
        && after_ws.contains(')')
        && after_ws
            .chars()
            .all(|ch| matches!(ch, '}' | ')' | ']' | ',' | ' ' | '\t'));
    if !is_call_tail {
        return Err(HeredocError::Unterminated { tag });
    }
    // Body content ends just before the structural call-tail line; `end` points
    // at the start of that line so the outer parser consumes the `})`/`)` close.
    let content_abs_end = content_start + last_line_start;
    let raw = &src[content_start..content_abs_end];
    let stripped = raw.strip_suffix('\n').unwrap_or(raw);
    let stripped = stripped.strip_suffix('\r').unwrap_or(stripped);
    tracing::debug!(
        target: "harn::tool_parse",
        "recovered heredoc with botched/missing closing tag (implicit close at call tail)"
    );
    Ok(HeredocSpan {
        content: content_start..content_start + stripped.len(),
        end: content_abs_end,
        escaped: false,
    })
}

/// Yield `(byte_offset_within_s, line_without_newline)` for each `\n`-delimited
/// line of `s`, including a trailing line with no terminating newline.
fn line_starts(s: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut start = 0usize;
    let bytes = s.as_bytes();
    std::iter::from_fn(move || {
        if start > s.len() {
            return None;
        }
        if start == s.len() {
            // Emit nothing more once we've passed the end; a trailing newline
            // means the final "line" is empty and is handled by the loop below.
            return None;
        }
        let mut end = start;
        while end < bytes.len() && bytes[end] != b'\n' {
            end += 1;
        }
        let line = &s[start..end];
        let offset = start;
        start = end + 1; // skip the '\n'
        Some((offset, line))
    })
}

/// Scan a JSON/string-escaped heredoc body whose line breaks are the two
/// literal bytes `\` + `n` instead of real newlines. `esc_nl_start` must sit on
/// the `\` of the `\n` that immediately follows the opening `<<TAG`. The closing
/// tag is found on a literal-`\n`-delimited "line" that — after optional literal
/// leading whitespace — begins with `tag` at a word boundary, mirroring the
/// real-newline grammar. The returned `content` range is the still-escaped body
/// (callers unescape it via [`unescape_heredoc_body`]); `escaped` is `true`.
fn scan_escaped_heredoc_body(
    src: &str,
    esc_nl_start: usize,
    tag: String,
) -> Result<HeredocSpan, HeredocError> {
    let bytes = src.as_bytes();
    // Body content starts after the leading literal `\n`.
    let content_start = esc_nl_start + 2;
    let mut pos = content_start;
    // `line_start` tracks the first content byte of the current escaped "line".
    let mut line_start = content_start;
    while pos < bytes.len() {
        // An escaped backslash `\\` is one decoded `\` — consume both bytes so a
        // following `n` (e.g. a Go source `"...\n"`, on the wire `\\n`) is NOT
        // misread as the escaped line separator. Keeps splitting consistent with
        // `unescape_heredoc_body`.
        if bytes.get(pos) == Some(&b'\\') && bytes.get(pos + 1) == Some(&b'\\') {
            pos += 2;
            continue;
        }
        // A literal `\n` (backslash + 'n') is the escaped line separator.
        if bytes.get(pos) == Some(&b'\\') && bytes.get(pos + 1) == Some(&b'n') {
            if let Some(span) = escaped_close_at(src, content_start, line_start, pos, &tag) {
                return Ok(span);
            }
            pos += 2;
            line_start = pos;
            continue;
        }
        pos += src[pos..].chars().next().map_or(1, char::len_utf8);
    }
    // The closing tag may sit on the final escaped line with no trailing `\n`
    // (e.g. `...\nEOF` at end of the string value, just before the closing
    // quote/paren). Check the trailing line.
    if let Some(span) = escaped_close_at(src, content_start, line_start, bytes.len(), &tag) {
        return Ok(span);
    }
    Err(HeredocError::Unterminated { tag })
}

/// Test whether the escaped "line" `src[line_start..line_end]` is the closing
/// tag line. `line_end` is the offset of the separating literal `\n` (or the end
/// of the body). On a match, returns a [`HeredocSpan`] whose `content` runs from
/// `content_start` to the start of the closing line's leading whitespace and
/// whose `end` is just past the tag. Returns `None` when the line is body text.
fn escaped_close_at(
    src: &str,
    content_start: usize,
    line_start: usize,
    line_end: usize,
    tag: &str,
) -> Option<HeredocSpan> {
    let line = &src[line_start..line_end];
    let leading_ws_len = line.len() - line.trim_start().len();
    let after_ws = &line[leading_ws_len..];
    let rest = after_ws.strip_prefix(tag)?;
    let at_word_boundary = rest
        .chars()
        .next()
        .is_none_or(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'));
    if !at_word_boundary {
        return None;
    }
    // Exclude the closing line (and the literal `\n` that introduced it) from
    // the body content, matching the real-newline grammar which excludes the
    // trailing newline before the close tag. The first body line shares its
    // start with `content_start`, so clamp to avoid an inverted range.
    let content_end = line_start.saturating_sub(2).max(content_start);
    // Real-newline closes leave the trailing newline + any tail (`EOF\n})`) for
    // the outer parser's `skip_ws_and_comments`. In the escaped form that
    // separator is the two literal bytes `\` + `n`, which the outer parser does
    // NOT treat as whitespace — so consume one optional trailing literal `\n`
    // here, leaving `end` on the structural tail (`})`/`,`).
    let bytes = src.as_bytes();
    let mut end = line_start + leading_ws_len + tag.len();
    if bytes.get(end) == Some(&b'\\') && bytes.get(end + 1) == Some(&b'n') {
        end += 2;
    }
    Some(HeredocSpan {
        content: content_start..content_end,
        end,
        escaped: true,
    })
}

/// Unescape a JSON/string-escaped heredoc body recovered from the degraded
/// literal-`\n` form. Decodes `\n`, `\t`, `\r`, `\"`, and `\\`; any other escape
/// is left verbatim (both the backslash and the following byte) so unrecognized
/// sequences in code survive unchanged. A trailing lone backslash is preserved.
pub(crate) fn unescape_heredoc_body(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
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

/// Skip a `"..."`, `'...'`, or `` `...` `` string span starting at `start`
/// (which must sit on the opening quote). Returns the byte offset just past the
/// closing quote, honoring `\`-escapes, or `None` when the string is
/// unterminated. Lets the close-tag scan treat a `<<TAG` or a `</tool_call>`
/// *inside a quoted argument* as content, not structure.
fn skip_string_span(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let quote = *bytes.get(start)?;
    if !matches!(quote, b'"' | b'\'' | b'`') {
        return None;
    }
    let mut i = start + 1;
    while i < src.len() {
        match bytes[i] {
            b'\\' => {
                i += 1;
                if i < src.len() {
                    i += src[i..].chars().next().map_or(1, char::len_utf8);
                }
            }
            byte if byte == quote => return Some(i + 1),
            _ => i += src[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    None
}

/// Find `needle` in `src[from..]`, stepping over quoted string spans and
/// complete `<<TAG ... TAG` heredoc bodies so an occurrence inside either —
/// a `</tool_call>` a model wrote as file content, or a bash `<<EOF` inside a
/// `command` string — is treated as content, not as the structural close. A
/// string or heredoc that is still incomplete yields [`CloseScan::NeedMore`].
/// This is the one place that knows "where does a tagged block really end",
/// shared by the buffered matcher, the truncation detector, and the streaming
/// scanner.
pub(super) fn find_close_tag(src: &str, from: usize, needle: &str) -> CloseScan {
    let bytes = src.as_bytes();
    let mut i = from;
    while i < src.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => match skip_string_span(src, i) {
                Some(after) => {
                    i = after;
                    continue;
                }
                // Unterminated string: streaming waits, a buffered caller treats
                // the block as truncated mid-string.
                None => return CloseScan::NeedMore,
            },
            b'<' if bytes.get(i + 1) == Some(&b'<') => match scan_heredoc(src, i) {
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
            },
            _ => {}
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
            // Preview the offending arg span so the model can tell which call
            // failed when several appear on screen (mirrors the native-JSON
            // `Raw: …` snippet).
            format!(
                "TOOL CALL PARSE ERROR: `{name}(...)` — {error}. \
                 Tool arguments must be a TypeScript object literal: `{{ key: value, key: value }}`. \
                 Raw: {}",
                preview_str(&text[paren_open + 1..], 200)
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
