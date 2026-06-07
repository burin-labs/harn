//! Fenced-JSON text tool-call parser (`tool_format == "json"`).
//!
//! A peer to [`super::tagged`] (heredoc/bare text protocol) and the native
//! provider channel. The grammar is deliberately delimiter-safe: a tool call
//! is one ```` ```tool ```` fenced block whose body is a single strict-JSON
//! `{ "name": ..., "args": { ... } }` object. N calls in a turn means N
//! fenced blocks.
//!
//! The load-bearing invariant: **a JSON string cannot contain a raw newline**.
//! Any ```` ``` ```` a model wants to put *inside* file content lives inside a
//! quoted JSON string, so it can never front a real source line. The LAYER 0
//! close scanner only honors a line whose trimmed content is exactly a bare
//! ```` ``` ````, so a content ```` ``` ```` never collides with the close
//! fence. This removes the entire author-chosen-delimiter collision class that
//! defeats heredoc sentinels (`<<EOF`), code-mode hash counts (`r#"..."#`), and
//! CDATA `]]>` — none of which a cheap model reliably escalates under pressure.
//! It also deletes the heredoc body channel entirely, so there is no marker for
//! a native-tuned model to copy into a JSON content string (the `line 0: <<`
//! production-failure class is unrepresentable here).
//!
//! Downstream dispatch consumes the same `{ id, name, arguments }` record the
//! tagged parser emits, so the agent loop / feedback / history are untouched.

use super::syntax::preview_str;
use super::TextToolParseResult;

/// Backtick fence used to open/close a fenced-JSON tool block.
const FENCE: &str = "```";
/// The exact info string that opens a tool block: ```` ```tool ````.
const OPEN_INFO: &str = "tool";

/// Structured reasons a single fenced block failed LAYER 1 (strict JSON,
/// one block -> one call). Each variant names the cause at the point of
/// rejection so the agent loop's `parse_guidance` feedback is actionable
/// rather than a symptom-only line-0 message.
#[derive(Debug)]
enum BlockError {
    /// The fence opened but never closed and the accumulated body is not a
    /// balanced/complete JSON object — a truncated string or mid-token cut.
    /// Naming the open-fence line mirrors the heredoc `Unterminated`
    /// discipline: a genuinely truncated body never silently dispatches.
    Unterminated { open_line: usize },
    /// The body parsed as JSON but was not a single object (an array, a
    /// scalar, or an object followed by trailing bytes).
    ExpectedSingleObject,
    /// The object is missing a non-empty string `name`.
    MissingName,
    /// `args` was present but not a JSON object.
    ArgsNotObject,
    /// The body is not valid JSON. Carries a cause-naming message with the
    /// byte offset so a lone backslash / bad `\u` / raw control char is named
    /// directly instead of surfacing as a downstream symptom.
    InvalidJson { detail: String },
}

impl BlockError {
    fn into_message(self) -> String {
        match self {
            BlockError::Unterminated { open_line } => format!(
                "Unterminated ```tool fence opened on line {} (1-based): the block reached \
                 end-of-output with an incomplete JSON object. Re-emit the whole call inside a \
                 ```tool ... ``` block and close it with a line that is exactly ```.",
                open_line + 1
            ),
            BlockError::ExpectedSingleObject => "A ```tool block must contain exactly one JSON \
                 object `{ \"name\": ..., \"args\": { ... } }`. Arrays, scalars, and trailing \
                 bytes are rejected; emit one ```tool block per tool call."
                .to_string(),
            BlockError::MissingName => "The ```tool JSON object is missing a non-empty string \
                 `name`. Shape: `{ \"name\": \"edit\", \"args\": { ... } }`."
                .to_string(),
            BlockError::ArgsNotObject => "The `args` field of a ```tool object must be a JSON \
                 object (`{ ... }`), or omitted when the tool takes no arguments."
                .to_string(),
            BlockError::InvalidJson { detail } => format!(
                "The ```tool block is not valid JSON: {detail}. Pass multi-line or code-bearing \
                 fields as ordinary JSON string values (escape newlines as \\n, quotes as \\\", \
                 backslashes as \\\\); backticks need no escaping."
            ),
        }
    }
}

/// One LAYER 0 block: the raw body bytes between the open and close fence and
/// the 0-based line index of the open fence (for diagnostics).
struct FenceBlock {
    body: String,
    open_line: usize,
    /// True when the block opened with a non-`tool` info string we accepted
    /// with a warning (today: ```` ```json ````). Surfaced as a
    /// `protocol_violation` so telemetry sees drift while the turn progresses.
    drifted_info: Option<String>,
}

/// Parse a model response under the fenced-JSON tool protocol.
///
/// LAYER 0 chunks the text into ```` ```tool ```` ... ```` ``` ```` blocks
/// (line-oriented, string-aware via the no-raw-newline invariant). LAYER 1
/// parses each block body as a strict single JSON `{ name, args }` object.
/// Successful calls dispatch; structural failures become `errors`; the
/// ```` ```json ```` accept-with-warning becomes a `violation`. Like the
/// tagged parser, this always runs to completion so every diagnostic surfaces.
pub(crate) fn parse_fenced_json_tool_calls(text: &str) -> TextToolParseResult {
    let cleaned = super::syntax::strip_thinking_tags(text);
    let src = cleaned.as_ref();

    let (blocks, prose, mut violations, mut errors) = chunk_fence_blocks(src);

    let mut calls: Vec<serde_json::Value> = Vec::new();
    for block in blocks {
        if let Some(info) = block.drifted_info {
            violations.push(format!(
                "protocol_violation: a tool call was emitted in a ```{info} fence; the contract \
                 requires a bare ```tool fence. Accepted this turn, but switch to ```tool."
            ));
        }
        match parse_block_body(&block.body, block.open_line) {
            Ok((name, arguments)) => {
                calls.push(serde_json::json!({
                    "id": format!("tc_{}", calls.len()),
                    "name": name,
                    "arguments": arguments,
                }));
            }
            Err(err) => errors.push(err.into_message()),
        }
    }

    TextToolParseResult {
        calls,
        errors,
        prose,
        user_response: None,
        violations,
        done_marker: None,
        canonical: src.to_string(),
    }
}

/// LAYER 0: walk lines and split into fenced-JSON blocks + leftover prose.
///
/// OPEN = a line whose trimmed content is exactly ```` ```tool ```` (the exact
/// `tool` info string). ```` ```json ```` whose body is a valid object is
/// ACCEPT-WITH-WARNING (recorded on the block as `drifted_info`); any other
/// info string (```` ```python ````, ```` ```tool_code ````, ```` ```tool x ````)
/// does NOT open and stays in prose. CLOSE = a line whose trimmed content is
/// exactly a bare ```` ``` ````. A content ```` ``` ```` lives inside a quoted
/// JSON string (which cannot hold a raw newline) so it never fronts a line and
/// never collides with the close fence.
///
/// EOF before CLOSE reuses the unterminated-or-implicit-close discipline: a
/// block whose accumulated body is a balanced/complete JSON object is accepted
/// (implicit close), otherwise LAYER 1 reports `Unterminated` and the body is
/// never half-applied.
fn chunk_fence_blocks(src: &str) -> (Vec<FenceBlock>, String, Vec<String>, Vec<String>) {
    let mut blocks: Vec<FenceBlock> = Vec::new();
    let mut prose_lines: Vec<&str> = Vec::new();
    let mut violations: Vec<String> = Vec::new();
    let errors: Vec<String> = Vec::new();

    let lines: Vec<&str> = src.lines().collect();
    let mut idx = 0usize;
    while idx < lines.len() {
        let line = lines[idx];
        match fence_open_kind(line) {
            Some(open) => {
                let open_line = idx;
                let mut body_lines: Vec<&str> = Vec::new();
                let mut closed = false;
                idx += 1;
                while idx < lines.len() {
                    if is_bare_fence(lines[idx]) {
                        closed = true;
                        idx += 1;
                        break;
                    }
                    body_lines.push(lines[idx]);
                    idx += 1;
                }
                let _ = closed; // EOF-before-close is handled by LAYER 1 balance check.
                let drifted_info = match open {
                    FenceOpen::Tool => None,
                    FenceOpen::DriftJson => Some("json".to_string()),
                };
                blocks.push(FenceBlock {
                    body: body_lines.join("\n"),
                    open_line,
                    drifted_info,
                });
            }
            None => {
                prose_lines.push(line);
                idx += 1;
            }
        }
    }

    // A ```json block that did NOT parse to a tool-call object should not be
    // silently swallowed — but we cannot know until LAYER 1. We keep the drift
    // warning attached to the block; if it fails to parse there, the block's
    // error is surfaced and the warning is noise the loop already tolerates.
    let _ = &mut violations;

    let prose = collapse_prose(&prose_lines);
    (blocks, prose, violations, errors)
}

/// The kind of fence an OPEN line carries.
enum FenceOpen {
    /// Canonical ```` ```tool ````.
    Tool,
    /// Drift: ```` ```json ````. Accepted with a protocol_violation warning.
    DriftJson,
}

/// Classify a line as a fence-open of a known info string, or `None`.
///
/// Only an exact ```` ```tool ```` opens canonically. ```` ```json ```` opens
/// with drift (accept-with-warning). Every other info string is left in prose
/// so a model that fences real code (```` ```python ````) does not get its
/// snippet eaten as a tool call.
fn fence_open_kind(line: &str) -> Option<FenceOpen> {
    let trimmed = line.trim();
    let info = trimmed.strip_prefix(FENCE)?;
    // The info string is everything after the opening fence, trimmed. A bare
    // ``` (empty info) is a close, not an open.
    let info = info.trim();
    match info {
        OPEN_INFO => Some(FenceOpen::Tool),
        "json" => Some(FenceOpen::DriftJson),
        _ => None,
    }
}

/// True when `line` is exactly a bare ```` ``` ```` close fence.
fn is_bare_fence(line: &str) -> bool {
    line.trim() == FENCE
}

/// Join leftover non-fence lines back into prose, trimming surrounding
/// whitespace. Empty -> empty string.
fn collapse_prose(lines: &[&str]) -> String {
    lines.join("\n").trim().to_string()
}

/// LAYER 1: parse one block body as a strict single JSON `{ name, args }`
/// object and return `(name, arguments)`.
///
/// Rejects arrays/scalars/trailing bytes (`ExpectedSingleObject`), a missing
/// or empty `name` (`MissingName`), and a non-object `args` (`ArgsNotObject`).
/// Absent `args` is treated as `{}` (downstream `validate_tool_args` enforces
/// required params, identical to the tagged path). A body that is incomplete
/// JSON at EOF reports `Unterminated` so a truncated call is never half-applied.
fn parse_block_body(
    body: &str,
    open_line: usize,
) -> Result<(String, serde_json::Value), BlockError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(BlockError::Unterminated { open_line });
    }

    // Strict single-object parse: a Deserializer stream that yields exactly one
    // object value and nothing but whitespace after it. Trailing non-whitespace
    // bytes -> ExpectedSingleObject; a second value -> ExpectedSingleObject.
    let mut stream = serde_json::Deserializer::from_str(trimmed).into_iter::<serde_json::Value>();
    let first = match stream.next() {
        Some(Ok(value)) => value,
        Some(Err(err)) => {
            if err.is_eof() {
                // Incomplete JSON at end of block: a truncated string or
                // mid-token cut. Never half-apply — name the open fence.
                return Err(BlockError::Unterminated { open_line });
            }
            return Err(BlockError::InvalidJson {
                detail: format!("{} (near `{}`)", err, preview_str(trimmed, 80)),
            });
        }
        None => return Err(BlockError::Unterminated { open_line }),
    };
    // Reject any trailing value/bytes after the single object.
    let consumed = stream.byte_offset();
    if !trimmed[consumed..].trim().is_empty() {
        return Err(BlockError::ExpectedSingleObject);
    }

    let obj = match first {
        serde_json::Value::Object(map) => map,
        _ => return Err(BlockError::ExpectedSingleObject),
    };

    let name = match obj.get("name") {
        Some(serde_json::Value::String(name)) if !name.trim().is_empty() => name.trim().to_string(),
        _ => return Err(BlockError::MissingName),
    };

    let arguments = match obj.get("args") {
        Some(serde_json::Value::Object(_)) => obj.get("args").cloned().unwrap_or_else(empty_object),
        Some(serde_json::Value::Null) | None => empty_object(),
        Some(_) => return Err(BlockError::ArgsNotObject),
    };

    Ok((name, arguments))
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> TextToolParseResult {
        parse_fenced_json_tool_calls(text)
    }

    fn arg<'a>(call: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
        call.get("arguments")?.get(key)
    }

    // S1: trivial single call.
    #[test]
    fn parses_a_single_clean_call() {
        let out = parse("```tool\n{\"name\": \"read_file\", \"args\": {\"path\": \"a.rs\"}}\n```");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0]["name"], "read_file");
        assert_eq!(arg(&out.calls[0], "path").unwrap(), "a.rs");
    }

    // S3: delimiter soup — content contains ```, <<EOF, a bare }, and </tool>.
    // All survive as \n-escaped JSON bytes; nothing fronts a real line.
    #[test]
    fn content_with_backticks_heredoc_brace_and_tag_survives() {
        let content = "```\nx := `raw`\n<<EOF\n}\n</tool>\n```";
        let json_content = serde_json::to_string(content).unwrap();
        let src = format!(
            "```tool\n{{\"name\": \"write_file\", \"args\": {{\"path\": \"f.go\", \"content\": {json_content}}}}}\n```"
        );
        let out = parse(&src);
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(arg(&out.calls[0], "content").unwrap(), content);
    }

    // S4: N fences for N calls.
    #[test]
    fn multiple_fences_yield_multiple_calls() {
        let src = "```tool\n{\"name\": \"a\", \"args\": {}}\n```\nsome prose\n```tool\n{\"name\": \"b\", \"args\": {\"k\": 1}}\n```";
        let out = parse(src);
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 2);
        assert_eq!(out.calls[0]["name"], "a");
        assert_eq!(out.calls[1]["name"], "b");
        assert!(out.prose.contains("some prose"));
    }

    // A body whose literal first line is `<<EOF` round-trips as JSON content —
    // no heredoc marker exists to leak into the file.
    #[test]
    fn content_starting_with_heredoc_opener_is_just_a_string() {
        let content = "<<EOF\npackage main\n";
        let json_content = serde_json::to_string(content).unwrap();
        let src = format!(
            "```tool\n{{\"name\": \"write_file\", \"args\": {{\"content\": {json_content}}}}}\n```"
        );
        let out = parse(&src);
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(arg(&out.calls[0], "content").unwrap(), content);
    }

    // ExpectedSingleObject: an array in the fence.
    #[test]
    fn array_body_is_expected_single_object() {
        let out = parse("```tool\n[{\"name\": \"a\", \"args\": {}}]\n```");
        assert!(out.calls.is_empty());
        assert_eq!(out.errors.len(), 1);
        assert!(
            out.errors[0].contains("exactly one JSON object"),
            "got: {}",
            out.errors[0]
        );
    }

    // ExpectedSingleObject: trailing bytes after the object.
    #[test]
    fn trailing_bytes_after_object_rejected() {
        let out = parse("```tool\n{\"name\": \"a\", \"args\": {}} trailing\n```");
        assert!(out.calls.is_empty());
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("exactly one JSON object"));
    }

    // MissingName: object without a name.
    #[test]
    fn missing_name_rejected() {
        let out = parse("```tool\n{\"args\": {\"path\": \"a\"}}\n```");
        assert!(out.calls.is_empty());
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("missing a non-empty string `name`"));
    }

    // MissingName: empty-string name.
    #[test]
    fn empty_name_rejected() {
        let out = parse("```tool\n{\"name\": \"  \", \"args\": {}}\n```");
        assert!(out.calls.is_empty());
        assert!(out.errors[0].contains("`name`"));
    }

    // ArgsNotObject: args is a scalar.
    #[test]
    fn args_not_object_rejected() {
        let out = parse("```tool\n{\"name\": \"a\", \"args\": \"oops\"}\n```");
        assert!(out.calls.is_empty());
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("must be a JSON object"));
    }

    // Absent args -> {} (downstream validates required params).
    #[test]
    fn absent_args_is_empty_object() {
        let out = parse("```tool\n{\"name\": \"list_dir\"}\n```");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert!(out.calls[0]["arguments"].is_object());
        assert_eq!(out.calls[0]["arguments"].as_object().unwrap().len(), 0);
    }

    // Unterminated: fence opens, JSON string is truncated, no close fence.
    // Must be rejected, never half-applied.
    #[test]
    fn truncated_string_is_unterminated_not_half_applied() {
        let out = parse("```tool\n{\"name\": \"write_file\", \"args\": {\"content\": \"half a str");
        assert!(out.calls.is_empty(), "must not dispatch a truncated call");
        assert_eq!(out.errors.len(), 1);
        assert!(
            out.errors[0].contains("Unterminated"),
            "got: {}",
            out.errors[0]
        );
    }

    // Implicit close: fence opens, a complete object, but EOF before the close
    // fence. A balanced/complete object is accepted (implicit close).
    #[test]
    fn complete_object_without_close_fence_is_accepted() {
        let out = parse("```tool\n{\"name\": \"a\", \"args\": {\"k\": 1}}");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0]["name"], "a");
    }

    // ```json accept-with-warning: a valid object in a ```json fence parses,
    // and emits a protocol_violation so telemetry sees the drift.
    #[test]
    fn json_fence_accepts_with_protocol_violation() {
        let out = parse("```json\n{\"name\": \"a\", \"args\": {}}\n```");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0]["name"], "a");
        assert!(
            out.violations
                .iter()
                .any(|v| v.contains("protocol_violation")),
            "violations: {:?}",
            out.violations
        );
    }

    // A non-tool fence (```python) is left in prose, not eaten as a call.
    #[test]
    fn unrelated_fence_stays_in_prose() {
        let out = parse("```python\nprint('hi')\n```");
        assert!(out.calls.is_empty());
        assert!(out.errors.is_empty());
        assert!(out.prose.contains("print('hi')"));
    }

    // Content containing a bare ``` line INSIDE the JSON string still parses —
    // because the JSON string cannot hold a raw newline, the embedded ``` is on
    // its own \n-escaped segment, never fronting a real source line.
    #[test]
    fn embedded_backtick_fence_does_not_close_early() {
        // Whole object on ONE source line; the ``` is inside the JSON string.
        let content = "before\n```\nafter";
        let json_content = serde_json::to_string(content).unwrap();
        let src = format!("```tool\n{{\"name\": \"w\", \"args\": {{\"c\": {json_content}}}}}\n```");
        let out = parse(&src);
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(arg(&out.calls[0], "c").unwrap(), content);
    }

    // Content containing `</tool>` survives (no XML/tag channel here).
    #[test]
    fn content_with_close_tool_tag_survives() {
        let content = "x </tool> y";
        let json_content = serde_json::to_string(content).unwrap();
        let src = format!("```tool\n{{\"name\": \"w\", \"args\": {{\"c\": {json_content}}}}}\n```");
        let out = parse(&src);
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(arg(&out.calls[0], "c").unwrap(), content);
    }
}
