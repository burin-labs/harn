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

/// Canonical backtick fence used to open/close a fenced-JSON tool block.
const BACKTICK_FENCE: &str = "```";
/// CommonMark tilde fence. Tool-shaped tilde fences are recoverable drift.
const TILDE_FENCE: &str = "~~~";
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
    /// A recognized chat-template envelope was incomplete or structurally
    /// invalid, so no partial action may be dispatched.
    ChatTemplateEnvelope { detail: String },
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
            BlockError::ChatTemplateEnvelope { detail } => format!(
                "The `<tool_calls>` chat-template envelope is malformed: {detail}. Re-emit \
                 complete calls as canonical ```tool JSON blocks; incomplete entries were not \
                 executed."
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
    /// with a warning. Surfaced as a `protocol_violation` so telemetry sees
    /// drift while the turn progresses.
    drifted_opener: Option<String>,
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

    let (blocks, mut prose, mut violations, mut errors) = chunk_fence_blocks(src);

    let mut calls: Vec<serde_json::Value> = Vec::new();
    let saw_fenced_blocks = !blocks.is_empty();
    for block in blocks {
        if let Some(opener) = block.drifted_opener {
            violations.push(format!(
                "protocol_violation: a tool call was emitted in a {opener} fence; the contract \
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

    if !saw_fenced_blocks {
        let trimmed = src.trim();
        if let Some(template_calls) = parse_chat_template_json_tool_calls(trimmed) {
            match template_calls {
                Ok(template_calls) => {
                    violations.push(
                        "protocol_violation: a tool call was emitted in a chat-template \
                         `<tool_calls>` envelope while `tool_format` is `json`; accepted this \
                         turn, but emit canonical ```tool JSON blocks next turn."
                            .to_string(),
                    );
                    for (name, arguments) in template_calls {
                        calls.push(serde_json::json!({
                            "id": format!("tc_{}", calls.len()),
                            "name": name,
                            "arguments": arguments,
                        }));
                    }
                    prose.clear();
                }
                Err(error) => errors.push(error.into_message()),
            }
        } else if let Ok((name, arguments)) = parse_bare_json_tool_call(trimmed) {
            violations.push(
                "protocol_violation: a tool call was emitted as a bare JSON object; the contract \
                 requires wrapping each `{ \"name\": ..., \"args\": { ... } }` object in a \
                 ```tool fence. Accepted this turn, but switch to ```tool."
                    .to_string(),
            );
            calls.push(serde_json::json!({
                "id": "tc_0",
                "name": name,
                "arguments": arguments,
            }));
            prose.clear();
        } else if contains_legacy_tool_call_markup(src) {
            violations.push(
                "protocol_violation: `<tool_call>...</tool_call>` markup was emitted while \
                 `tool_format` is `json`; wrap each call as a strict JSON object inside a \
                 ```tool fence instead."
                    .to_string(),
            );
        }
    }

    TextToolParseResult {
        calls,
        errors,
        prose,
        user_response: None,
        violations,
        recovered_from_stray_count: 0,
        done_marker: None,
        canonical: src.to_string(),
    }
}

/// LAYER 0: walk lines and split into fenced-JSON blocks + leftover prose.
///
/// OPEN = a line whose trimmed content is exactly ```` ```tool ```` (the exact
/// `tool` info string). Recoverable drift such as ```` ```json ````,
/// ```` ```tool_code ````, ```` ```tool python ````, or ```` ~~~tool ```` also
/// opens with a protocol warning; unrelated code fences stay in prose. CLOSE =
/// a line whose trimmed content is exactly the matching bare fence marker. A
/// content ```` ``` ```` lives inside a quoted JSON string (which cannot hold a
/// raw newline) so it never fronts a line and never collides with the close
/// fence.
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
                    if is_bare_fence(lines[idx], open.close_fence) {
                        closed = true;
                        idx += 1;
                        break;
                    }
                    body_lines.push(lines[idx]);
                    idx += 1;
                }
                let _ = closed; // EOF-before-close is handled by LAYER 1 balance check.
                blocks.push(FenceBlock {
                    body: body_lines.join("\n"),
                    open_line,
                    drifted_opener: open.drifted_opener,
                });
            }
            None => {
                prose_lines.push(line);
                idx += 1;
            }
        }
    }

    // A drift block that did NOT parse to a tool-call object should not be
    // silently swallowed. We keep the warning attached to the block; if it
    // fails to parse there, the block's error is surfaced and the warning is
    // still useful protocol guidance.
    let _ = &mut violations;

    let prose = collapse_prose(&prose_lines);
    (blocks, prose, violations, errors)
}

/// A recognized OPEN line.
struct FenceOpen {
    close_fence: &'static str,
    drifted_opener: Option<String>,
}

/// Classify a line as a fence-open of a known info string, or `None`.
///
/// Only an exact ```` ```tool ```` opens canonically. Recognizable tool-call
/// drift opens with a warning. Every other info string is left in prose so a
/// model that fences real code (```` ```python ````) does not get its snippet
/// eaten as a tool call.
fn fence_open_kind(line: &str) -> Option<FenceOpen> {
    let trimmed = line.trim();
    let (fence, info) = if let Some(info) = trimmed.strip_prefix(BACKTICK_FENCE) {
        (BACKTICK_FENCE, info)
    } else if let Some(info) = trimmed.strip_prefix(TILDE_FENCE) {
        (TILDE_FENCE, info)
    } else {
        return None;
    };
    // The info string is everything after the opening fence, trimmed. A bare
    // ``` (empty info) is a close, not an open.
    let info = info.trim();
    if fence == BACKTICK_FENCE && info == OPEN_INFO {
        return Some(FenceOpen {
            close_fence: BACKTICK_FENCE,
            drifted_opener: None,
        });
    }
    if is_recoverable_tool_fence_drift(fence, info) {
        return Some(FenceOpen {
            close_fence: fence,
            drifted_opener: Some(format!("{fence}{info}")),
        });
    }
    None
}

fn is_recoverable_tool_fence_drift(fence: &str, info: &str) -> bool {
    if info == "json" || info == "tool_code" || info == "tool_call" || info == "function_call" {
        return true;
    }
    if fence == TILDE_FENCE && info == OPEN_INFO {
        return true;
    }
    info.strip_prefix(OPEN_INFO)
        .and_then(|rest| rest.chars().next())
        .is_some_and(|ch| ch.is_ascii_whitespace() || ch == '_' || ch == '-')
}

/// True when `line` is exactly a bare close fence matching the opener.
fn is_bare_fence(line: &str, fence: &str) -> bool {
    line.trim() == fence
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

    normalize_json_tool_call_object(obj, false)
}

/// Normalize one JSON tool-call object into Harn's `{ name, arguments }`
/// dispatch contract. Canonical fenced JSON requires a nested `args` object;
/// recognized chat-template envelopes may carry their arguments inline beside
/// `name`, because that is the only unambiguous shape they emit.
fn normalize_json_tool_call_object(
    obj: serde_json::Map<String, serde_json::Value>,
    allow_inline_arguments: bool,
) -> Result<(String, serde_json::Value), BlockError> {
    // Canonical keys are `name`/`args`. Accept `tool`/`arguments` as aliases so
    // the bare `{"tool":..,"arguments":..}` dialect emitted by some models on
    // text/json channels (MiniMax, qwen, gpt-oss on text-only routes) parses
    // instead of being dropped. Canonical wins when both are present.
    let name = match obj.get("name").or_else(|| obj.get("tool")) {
        Some(serde_json::Value::String(name)) if !name.trim().is_empty() => name.trim().to_string(),
        _ => return Err(BlockError::MissingName),
    };

    let args_value = obj.get("args").or_else(|| obj.get("arguments"));
    let arguments = match args_value {
        Some(value @ serde_json::Value::Object(_)) => value.clone(),
        Some(serde_json::Value::Null) => empty_object(),
        None if allow_inline_arguments => serde_json::Value::Object(
            obj.into_iter()
                .filter(|(key, _)| key != "name" && key != "tool")
                .collect(),
        ),
        None => empty_object(),
        Some(_) => return Err(BlockError::ArgsNotObject),
    };

    Ok(super::super::normalize_tool_call_shape(&name, arguments))
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn parse_bare_json_tool_call(body: &str) -> Result<(String, serde_json::Value), BlockError> {
    if body.is_empty()
        || !body.starts_with('{')
        || !(body.contains("\"name\"") || body.contains("\"tool\""))
        || !(body.contains("\"args\"") || body.contains("\"arguments\""))
    {
        return Err(BlockError::ExpectedSingleObject);
    }
    parse_block_body(body, 0)
}

/// Parse the common chat-template JSON dialect used by text-only tool routes:
///
/// ```text
/// <tool_calls>
/// <tool>
/// {"name":"look","file":"src/lib.rs"}
/// {"name":"run","command":"cargo test"}
/// </tool_calls>
/// ```
///
/// The grammar is intentionally whole-response only. A `<tool_calls>` opener
/// must have the matching close, and every `<tool>` marker must be followed by
/// a complete JSON object before another marker, a `</tool>` close, or the
/// wrapper close. This makes the recovery unambiguous while keeping partial
/// markup fail-loud rather than dispatching a guessed action.
fn parse_chat_template_json_tool_calls(
    src: &str,
) -> Option<Result<Vec<(String, serde_json::Value)>, BlockError>> {
    const OPEN: &str = "<tool_calls>";
    const CLOSE: &str = "</tool_calls>";
    const TOOL_OPEN: &str = "<tool>";
    const TOOL_CLOSE: &str = "</tool>";

    let body = src.strip_prefix(OPEN)?;
    let Some(mut body) = body.strip_suffix(CLOSE).map(str::trim) else {
        return Some(Err(BlockError::ChatTemplateEnvelope {
            detail: "the opening `<tool_calls>` tag has no matching `</tool_calls>` close"
                .to_string(),
        }));
    };

    let mut saw_tool_marker = false;
    let mut tool_marker_active = false;
    let mut pending_tool_object = false;
    let mut calls = Vec::new();
    while !body.is_empty() {
        if let Some(rest) = body.strip_prefix(TOOL_OPEN) {
            if pending_tool_object {
                return Some(Err(BlockError::ChatTemplateEnvelope {
                    detail: "a `<tool>` marker must be followed by a JSON object before another `<tool>` marker"
                        .to_string(),
                }));
            }
            saw_tool_marker = true;
            tool_marker_active = true;
            pending_tool_object = true;
            body = rest.trim_start();
            continue;
        }
        if let Some(rest) = body.strip_prefix(TOOL_CLOSE) {
            if !tool_marker_active {
                return Some(Err(BlockError::ChatTemplateEnvelope {
                    detail:
                        "found an unmatched `</tool>` close without a preceding `<tool>` marker"
                            .to_string(),
                }));
            }
            if pending_tool_object {
                return Some(Err(BlockError::ChatTemplateEnvelope {
                    detail: "a `<tool>` marker must be followed by a JSON object before `</tool>`"
                        .to_string(),
                }));
            }
            tool_marker_active = false;
            body = rest.trim_start();
            continue;
        }
        if !tool_marker_active {
            let detail = if saw_tool_marker {
                "expected a `<tool>` marker before this JSON object"
            } else {
                "the envelope contained no `<tool>` marker"
            };
            return Some(Err(BlockError::ChatTemplateEnvelope {
                detail: detail.to_string(),
            }));
        }
        if !body.starts_with('{') {
            return Some(Err(BlockError::ChatTemplateEnvelope {
                detail: format!(
                    "expected a `{TOOL_OPEN}` marker or a JSON object, found `{}`",
                    preview_str(body, 80)
                ),
            }));
        }

        let mut values = serde_json::Deserializer::from_str(body).into_iter::<serde_json::Value>();
        let value = match values.next() {
            Some(Ok(value)) => value,
            Some(Err(error)) if error.is_eof() => {
                return Some(Err(BlockError::ChatTemplateEnvelope {
                    detail: "a JSON tool object ended before its closing `}`".to_string(),
                }));
            }
            Some(Err(error)) => {
                return Some(Err(BlockError::ChatTemplateEnvelope {
                    detail: format!("invalid JSON tool object: {error}"),
                }));
            }
            None => {
                return Some(Err(BlockError::ChatTemplateEnvelope {
                    detail: "expected a JSON tool object after `<tool>`".to_string(),
                }));
            }
        };
        let consumed = values.byte_offset();
        let obj = match value {
            serde_json::Value::Object(obj) => obj,
            _ => {
                return Some(Err(BlockError::ChatTemplateEnvelope {
                    detail: "each chat-template tool entry must be a JSON object".to_string(),
                }));
            }
        };
        match normalize_json_tool_call_object(obj, true) {
            Ok(call) => calls.push(call),
            Err(error) => {
                return Some(Err(BlockError::ChatTemplateEnvelope {
                    detail: error.into_message(),
                }));
            }
        }
        pending_tool_object = false;
        body = body[consumed..].trim_start();
    }

    if pending_tool_object {
        return Some(Err(BlockError::ChatTemplateEnvelope {
            detail: "a `<tool>` marker ended without a complete JSON object".to_string(),
        }));
    }
    if !saw_tool_marker {
        return Some(Err(BlockError::ChatTemplateEnvelope {
            detail: "the envelope contained no `<tool>` marker".to_string(),
        }));
    }
    if calls.is_empty() {
        return Some(Err(BlockError::ChatTemplateEnvelope {
            detail: "the envelope contained no JSON tool object".to_string(),
        }));
    }
    Some(Ok(calls))
}

fn contains_legacy_tool_call_markup(src: &str) -> bool {
    src.contains("<tool_call>")
        || src.contains("<tool_call ")
        || src.contains("<toolcall>")
        || src.contains("<toolcall ")
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

    // S1b: tool/arguments dialect aliases (MiniMax / qwen / gpt-oss on text).
    #[test]
    fn parses_tool_arguments_dialect_aliases() {
        let out =
            parse("```tool\n{\"tool\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}\n```");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0]["name"], "read_file");
        assert_eq!(arg(&out.calls[0], "path").unwrap(), "a.rs");
    }

    #[test]
    fn unwraps_generic_tool_wrapper_envelope() {
        let out = parse(
            "```tool\n{\"name\":\"tool\",\"args\":{\"name\":\"look\",\"args\":{\"intent\":\"read\",\"file\":\"src/lib.rs\"}}}\n```",
        );
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0]["name"], "look");
        assert_eq!(arg(&out.calls[0], "intent").unwrap(), "read");
        assert_eq!(arg(&out.calls[0], "file").unwrap(), "src/lib.rs");
    }

    #[test]
    fn strips_harmony_channel_suffix_from_tool_name() {
        let out = parse(
            "```tool\n{\"name\":\"run<|channel|>commentary\",\"args\":{\"command\":\"cargo test\"}}\n```",
        );
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0]["name"], "run");
        assert_eq!(arg(&out.calls[0], "command").unwrap(), "cargo test");
    }

    // S1c: canonical name/args win when both canonical and alias are present.
    #[test]
    fn canonical_keys_win_over_aliases() {
        let out = parse(
            "```tool\n{\"name\": \"canonical\", \"tool\": \"alias\", \"args\": {\"k\": 1}, \"arguments\": {\"k\": 2}}\n```",
        );
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0]["name"], "canonical");
        assert_eq!(arg(&out.calls[0], "k").unwrap(), 1);
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

    #[test]
    fn tool_like_fence_drift_accepts_with_protocol_violation() {
        for (src, opener) in [
            (
                "```tool_code\n{\"name\": \"a\", \"args\": {\"k\": 1}}\n```",
                "```tool_code",
            ),
            (
                "```tool python\n{\"name\": \"a\", \"args\": {\"k\": 1}}\n```",
                "```tool python",
            ),
            (
                "```function_call\n{\"name\": \"a\", \"args\": {\"k\": 1}}\n```",
                "```function_call",
            ),
            (
                "~~~tool\n{\"name\": \"a\", \"args\": {\"k\": 1}}\n~~~",
                "~~~tool",
            ),
        ] {
            let out = parse(src);
            assert!(
                out.errors.is_empty(),
                "errors for {opener}: {:?}",
                out.errors
            );
            assert_eq!(out.calls.len(), 1, "calls for {opener}: {:?}", out.calls);
            assert_eq!(out.calls[0]["name"], "a");
            assert_eq!(arg(&out.calls[0], "k").unwrap(), 1);
            assert!(
                out.violations.iter().any(|v| v.contains(opener)),
                "violations for {opener}: {:?}",
                out.violations
            );
        }
    }

    #[test]
    fn invalid_tool_like_fence_drift_reports_error_and_violation() {
        let out = parse("```tool_code\nnot json\n```");
        assert!(out.calls.is_empty());
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("not valid JSON"));
        assert!(
            out.violations.iter().any(|v| v.contains("```tool_code")),
            "violations: {:?}",
            out.violations
        );
    }

    #[test]
    fn bare_json_tool_call_accepts_with_protocol_violation() {
        let out = parse("{\"name\": \"a\", \"args\": {\"k\": 1}}");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.prose.is_empty(), "prose: {:?}", out.prose);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0]["name"], "a");
        assert_eq!(arg(&out.calls[0], "k").unwrap(), 1);
        assert!(
            out.violations
                .iter()
                .any(|v| v.contains("bare JSON object")),
            "violations: {:?}",
            out.violations
        );
    }

    #[test]
    fn chat_template_envelope_recovers_multiple_inline_argument_calls() {
        // Exact production shape from the Qwen chat template: one `<tool>`
        // marker introduces consecutive objects, and the template leaves its
        // arguments inline instead of nesting them under `args`.
        let out = parse(
            "<tool_calls>\n<tool>\n{\"name\":\"look\",\"file\":\"src/writer.zig\"}\n{\"name\":\"look\",\"file\":\"src/parser.zig\"}\n</tool_calls>",
        );
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.prose.is_empty(), "prose: {:?}", out.prose);
        assert_eq!(out.calls.len(), 2);
        assert_eq!(out.calls[0]["name"], "look");
        assert_eq!(
            arg(&out.calls[0], "file"),
            Some(&serde_json::json!("src/writer.zig"))
        );
        assert_eq!(out.calls[1]["name"], "look");
        assert_eq!(
            arg(&out.calls[1], "file"),
            Some(&serde_json::json!("src/parser.zig"))
        );
        assert!(
            out.violations
                .iter()
                .any(|violation| violation.contains("chat-template")),
            "violations: {:?}",
            out.violations
        );
    }

    #[test]
    fn chat_template_envelope_accepts_optional_per_call_close_markers() {
        let out = parse(
            "<tool_calls><tool>{\"name\":\"a\",\"args\":{\"k\":1}}</tool><tool>{\"tool\":\"b\",\"arguments\":{\"v\":2}}</tool></tool_calls>",
        );
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(out.calls.len(), 2);
        assert_eq!(out.calls[0]["name"], "a");
        assert_eq!(arg(&out.calls[0], "k"), Some(&serde_json::json!(1)));
        assert_eq!(out.calls[1]["name"], "b");
        assert_eq!(arg(&out.calls[1], "v"), Some(&serde_json::json!(2)));
    }

    #[test]
    fn malformed_chat_template_envelope_never_dispatches_partial_calls() {
        let out = parse("<tool_calls>\n<tool>\n{\"name\":\"look\",\"file\":\"src/writer.zig\"");
        assert!(
            out.calls.is_empty(),
            "partial calls must not dispatch: {:?}",
            out.calls
        );
        assert_eq!(out.errors.len(), 1);
        assert!(
            out.errors[0].contains("<tool_calls>") && out.errors[0].contains("not executed"),
            "error: {}",
            out.errors[0]
        );
    }

    #[test]
    fn truncated_chat_template_second_marker_never_dispatches_first_call() {
        let out = parse(
            "<tool_calls><tool>{\"name\":\"look\",\"file\":\"src/lib.rs\"}<tool></tool_calls>",
        );
        assert!(
            out.calls.is_empty(),
            "partial calls must not dispatch: {:?}",
            out.calls
        );
        assert_eq!(out.errors.len(), 1);
        assert!(
            out.errors[0].contains("ended without a complete JSON object")
                && out.errors[0].contains("not executed"),
            "error: {}",
            out.errors[0]
        );
    }

    #[test]
    fn unmatched_chat_template_tool_close_is_rejected() {
        let out = parse("<tool_calls></tool></tool_calls>");
        assert!(out.calls.is_empty());
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("unmatched `</tool>`"));
    }

    #[test]
    fn chat_template_without_tool_marker_is_rejected() {
        let out = parse("<tool_calls>{\"name\":\"look\",\"file\":\"src/writer.zig\"}</tool_calls>");
        assert!(out.calls.is_empty());
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("no `<tool>` marker"));
    }

    #[test]
    fn legacy_tagged_markup_under_json_reports_protocol_violation() {
        let out = parse("<tool_call>\na({})\n</tool_call>");
        assert!(out.calls.is_empty());
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(
            out.violations.iter().any(|v| v.contains("<tool_call>")),
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

    // A non-tool tilde fence is also left in prose, not eaten as a call.
    #[test]
    fn unrelated_tilde_fence_stays_in_prose() {
        let out = parse("~~~python\nprint('hi')\n~~~");
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
