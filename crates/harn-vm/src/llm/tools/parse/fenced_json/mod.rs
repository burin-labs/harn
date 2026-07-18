//! Fenced-JSON text tool-call parser (`tool_format == "json"`).
//!
//! A peer to [`super::tagged`] (heredoc/bare text protocol) and the native
//! provider channel. The grammar is deliberately delimiter-safe: a tool call
//! is a strict-JSON `{ "name": ..., "args": { ... } }` object inside a
//! ```` ```tool ```` fenced block. N calls in a turn are N such objects,
//! emitted either as N separate blocks or as N objects inside one block (the
//! natural batch shape) — both are the same ordered batch.
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
//! One deliberate exception rides on top of this invariant: the verbatim content
//! lane (harn#5033, see [`verbatim`]). #5015 relocated payload fragility into
//! JSON-string escaping, which a cheap model cannot hold across a
//! backslash/newline-dense body. An object MAY declare an argument verbatim by
//! setting its JSON string value to a heredoc opener (`<<TAG:N`); the matching
//! count-anchored `<<TAG:N ... TAG` body then trails the JSON header and is
//! decoded by the shared `scan_heredoc` authority (no second decoder). The
//! declaration is in-JSON and the `:N` count closes the body, so it does NOT
//! reopen the collision class above: a ```` ``` ````, a `<<EOF`, or the tag
//! itself inside the counted body is just content. A block whose arguments are
//! all ordinary strings is byte-identical to before.
//!
//! Downstream dispatch consumes the same `{ id, name, arguments }` record the
//! tagged parser emits, so the agent loop / feedback / history are untouched.

use super::syntax::preview_str;
use super::TextToolParseResult;

mod verbatim;
use verbatim::VerbatimSegment;

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
            BlockError::ExpectedSingleObject => "A ```tool block must contain one or more JSON \
                 objects `{ \"name\": ..., \"args\": { ... } }`, one per tool call (several calls \
                 in a turn may share one block or use several blocks). Arrays, scalars, and other \
                 non-object entries are rejected."
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

/// One LAYER 0 block: the JSON header text between the open and close fence
/// (with any verbatim segments removed) and the 0-based line index of the open
/// fence (for diagnostics).
struct FenceBlock {
    body: String,
    open_line: usize,
    /// True when the block opened with a non-`tool` info string we accepted
    /// with a warning. Surfaced as a `protocol_violation` so telemetry sees
    /// drift while the turn progresses.
    drifted_opener: Option<String>,
    /// Decoded verbatim segments (harn#5033), folded into the batch's args
    /// before dispatch. Empty for a canonical pure-JSON block.
    segments: Vec<VerbatimSegment>,
    /// A structural failure in a verbatim segment frame (bad count,
    /// unterminated). When set, the block fails loud instead of half-applying.
    verbatim_error: Option<BlockError>,
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
        // A broken verbatim frame (harn#5033) is fail-loud: never dispatch a
        // batch whose payload framing did not round-trip.
        if let Some(err) = block.verbatim_error {
            errors.push(err.into_message());
            continue;
        }
        let (block_calls, block_errors) =
            parse_block_bodies(&block.body, block.open_line, block.segments);
        for (name, arguments) in block_calls {
            calls.push(serde_json::json!({
                "id": format!("tc_{}", calls.len()),
                "name": name,
                "arguments": arguments,
            }));
        }
        for err in block_errors {
            errors.push(err.into_message());
        }
    }

    if !saw_fenced_blocks {
        let trimmed = src.trim();
        if let Some(located) = locate_and_parse_tool_envelope(src) {
            match located.result {
                Ok(template_calls) => {
                    violations.push(
                        "protocol_violation: a tool call was emitted in a chat-template tool \
                         envelope while `tool_format` is `json`; accepted this turn, but emit \
                         canonical ```tool JSON blocks next turn."
                            .to_string(),
                    );
                    for (name, arguments) in template_calls {
                        calls.push(serde_json::json!({
                            "id": format!("tc_{}", calls.len()),
                            "name": name,
                            "arguments": arguments,
                        }));
                    }
                    prose = located.prose;
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

    // Byte-offset walk (not `src.lines()`) so a verbatim segment's value can be
    // sliced byte-exact, preserving `\r` in a CRLF payload. `line_index` tracks
    // the 0-based opener line for diagnostics.
    let mut cursor = 0usize;
    let mut line_index = 0usize;
    while cursor < src.len() {
        let (line, _, next) = verbatim::line_at(src, cursor);
        match fence_open_kind(line) {
            Some(open) => {
                // The unified consumer is the single authority for the block's
                // end and its verbatim segments: a ``` (or a ```tool separator,
                // or the sentinel) INSIDE a count-anchored segment is content,
                // while a ```tool separator in the header context is an implicit
                // close + reopen (#5037), and a bare ``` closes the block.
                let consumed = verbatim::consume_tool_block(src, next, open.close_fence);
                blocks.push(FenceBlock {
                    body: consumed.header,
                    open_line: line_index,
                    drifted_opener: open.drifted_opener,
                    segments: consumed.segments,
                    verbatim_error: consumed.error,
                });
                line_index += src[cursor..consumed.block_end].matches('\n').count();
                cursor = consumed.block_end;
            }
            None => {
                prose_lines.push(line);
                line_index += 1;
                cursor = next;
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

/// Join leftover non-fence lines back into prose, trimming surrounding
/// whitespace. Empty -> empty string.
fn collapse_prose(lines: &[&str]) -> String {
    lines.join("\n").trim().to_string()
}

/// LAYER 1: parse one block body as an ORDERED BATCH of JSON `{ name, args }`
/// objects, returning the parsed calls plus any errors.
///
/// The turn contract is "N calls per turn": a model may emit N calls as N
/// separate ```tool blocks OR as N JSON objects inside ONE ```tool block (the
/// natural batch shape a model reaches for when it wants several reads at once).
/// Both mean the same ordered batch of calls. Each complete object is parsed
/// independently, so a batch is never dropped wholesale: when a later object in
/// the block is truncated or malformed, the complete objects before it are still
/// salvaged as calls and the failing tail surfaces an error for re-teaching.
///
/// Per-object rejects mirror the single-object contract: a missing/empty `name`
/// (`MissingName`), a non-object `args` (`ArgsNotObject`), and a non-object entry
/// such as an array or scalar (`ExpectedSingleObject`). A body that ends with an
/// incomplete JSON object reports `Unterminated` so a truncated call is never
/// half-applied; the complete objects before the cut are still returned.
fn parse_block_bodies(
    body: &str,
    open_line: usize,
    segments: Vec<VerbatimSegment>,
) -> (Vec<(String, serde_json::Value)>, Vec<BlockError>) {
    let trimmed = body.trim();
    let mut calls = Vec::new();
    let mut errors = Vec::new();
    // Verbatim segments (harn#5033) are matched to sentinel-valued arguments by
    // name as each object is parsed; leftovers below are a header/segment
    // mismatch that must fail loud rather than silently drop a payload.
    let mut pool = segments;
    if trimmed.is_empty() {
        errors.push(BlockError::Unterminated { open_line });
        return (calls, errors);
    }

    // Stream consecutive top-level JSON values. Each complete object becomes one
    // call; the stream stops at the first structural break (EOF mid-object, or a
    // non-JSON token such as a stray fence line), preserving every object parsed
    // before the break.
    let mut stream = serde_json::Deserializer::from_str(trimmed).into_iter::<serde_json::Value>();
    let mut saw_value = false;
    loop {
        match stream.next() {
            Some(Ok(value)) => {
                saw_value = true;
                match value {
                    serde_json::Value::Object(mut map) => {
                        inject_verbatim_segments(&mut map, &mut pool);
                        match normalize_json_tool_call_object(map, false) {
                            Ok(call) => calls.push(call),
                            Err(err) => errors.push(err),
                        }
                    }
                    _ => errors.push(BlockError::ExpectedSingleObject),
                }
            }
            Some(Err(err)) if err.is_eof() => {
                // Incomplete JSON at end of block: a truncated string or mid-token
                // cut. Never half-apply — name the open fence. Objects already
                // parsed in this block are kept.
                errors.push(BlockError::Unterminated { open_line });
                break;
            }
            Some(Err(err)) => {
                errors.push(BlockError::InvalidJson {
                    detail: format!("{} (near `{}`)", err, preview_str(trimmed, 80)),
                });
                break;
            }
            None => break,
        }
    }

    // A trailing heredoc no argument's value declared is a header/body mismatch —
    // fail loud so a stray body is never silently dropped.
    for leftover in &pool {
        errors.push(BlockError::InvalidJson {
            detail: format!(
                "a verbatim heredoc `{}` trails the block but no argument's value declared it",
                leftover.opener
            ),
        });
    }

    if !saw_value && errors.is_empty() {
        errors.push(BlockError::Unterminated { open_line });
    }
    (calls, errors)
}

/// Bind each argument in `obj`'s nested `args`/`arguments` map whose value is a
/// heredoc opener to its trailing body, claimed from `pool` (harn#5033). A no-op
/// when the object has no nested args object or declares nothing verbatim.
fn inject_verbatim_segments(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    pool: &mut Vec<VerbatimSegment>,
) {
    let args_key = if obj.contains_key("args") {
        "args"
    } else if obj.contains_key("arguments") {
        "arguments"
    } else {
        return;
    };
    if let Some(args) = obj
        .get_mut(args_key)
        .and_then(serde_json::Value::as_object_mut)
    {
        verbatim::apply_segments(args, pool);
    }
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
    // Bare (un-fenced) recovery is single-object only: require exactly one clean
    // object with no trailing/partial bytes before accepting it. No fenced block
    // means no verbatim segments.
    let (calls, errors) = parse_block_bodies(body, 0, Vec::new());
    match (calls.len(), errors.is_empty()) {
        (1, true) => Ok(calls.into_iter().next().unwrap()),
        _ => Err(BlockError::ExpectedSingleObject),
    }
}

/// A tool-call envelope located anywhere at the top level of the response.
///
/// The three openers a text tool route emits when it drops the canonical
/// ```` ```tool ```` fence. `ToolCalls` carries either the `<tool>`-marker JSON
/// dialect or the tag-per-call XML dialect; `ToolCode` and `ToolCall` wrap a
/// list of JSON tool objects directly.
// The variants mirror the three literal tag names (`<tool_calls>`, `<tool_code>`,
// `<tool_call>`), so the shared `Tool` prefix is inherent, not incidental.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy)]
enum EnvelopeKind {
    ToolCalls,
    ToolCode,
    ToolCall,
}

impl EnvelopeKind {
    fn opener(self) -> &'static str {
        match self {
            EnvelopeKind::ToolCalls => "<tool_calls>",
            EnvelopeKind::ToolCode => "<tool_code>",
            EnvelopeKind::ToolCall => "<tool_call>",
        }
    }

    fn close(self) -> &'static str {
        match self {
            EnvelopeKind::ToolCalls => "</tool_calls>",
            EnvelopeKind::ToolCode => "</tool_code>",
            EnvelopeKind::ToolCall => "</tool_call>",
        }
    }
}

/// A located envelope: the surrounding prose plus the parse outcome of its body.
struct LocatedEnvelope {
    prose: String,
    result: Result<Vec<(String, serde_json::Value)>, BlockError>,
}

/// The outcome of parsing one envelope body: the parsed calls plus the input
/// slice past the structurally-recognized close tag (empty at end-of-input), or
/// a `ChatTemplateEnvelope` violation.
type EnvelopeBody<'a> = Result<(Vec<(String, serde_json::Value)>, &'a str), BlockError>;

fn env_err(detail: String) -> BlockError {
    BlockError::ChatTemplateEnvelope { detail }
}

/// Locate the first top-level tool-call envelope and parse its body.
///
/// The opener may appear ANYWHERE at the top level of the response (outside
/// markdown code fences), not just as a prefix — prose before and after the
/// envelope survives as prose. End-of-input acts as an implicit close: the
/// missing `</tool_calls>` / `</tool_code>` / `</tool_call>` is accepted (models
/// routinely EOS before closing). An opener whose body yields zero complete
/// calls returns a `ChatTemplateEnvelope` violation (fail-loud) rather than
/// `None`, so runtime parse guidance fires instead of misrouting the turn to
/// completion/monologue feedback.
///
/// Close recognition is STRUCTURAL, never a raw substring scan: the body
/// parsers consume one complete top-level JSON object (or one complete XML call
/// block) at a time and only match the closing tag at a boundary BETWEEN those
/// units. A literal `</tool_code>` / `</tool_calls>` byte inside a JSON string
/// value or an XML argument value is therefore preserved as content — exactly
/// what the fenced-JSON contract promises for code-bearing arguments — instead
/// of truncating the envelope. Whatever survives past the recognized close is
/// returned as trailing prose.
fn locate_and_parse_tool_envelope(src: &str) -> Option<LocatedEnvelope> {
    let (opener_start, kind, content_start) = find_top_level_opener(src)?;
    let body = &src[content_start..];

    // The singular `<tool_call>` tag collides with the legacy `name({...})`
    // heredoc markup. Only claim it for the JSON dialect when its body is a JSON
    // object; otherwise leave it to the legacy-markup path (which emits its own
    // actionable protocol violation), so the existing legacy behavior is kept.
    if matches!(kind, EnvelopeKind::ToolCall) && !body.trim_start().starts_with('{') {
        return None;
    }

    let close = kind.close();
    let parsed = match kind {
        EnvelopeKind::ToolCalls => parse_tool_calls_body(body, close),
        EnvelopeKind::ToolCode | EnvelopeKind::ToolCall => parse_json_object_list(body, close),
    };
    let before = &src[..opener_start];
    Some(match parsed {
        Ok((calls, after)) => LocatedEnvelope {
            prose: collapse_prose_around(before, after),
            result: Ok(calls),
        },
        Err(error) => LocatedEnvelope {
            prose: collapse_prose_around(before, ""),
            result: Err(error),
        },
    })
}

/// Find the first `<tool_calls>` / `<tool_code>` / `<tool_call>` opener that is
/// not inside a markdown code fence. Returns its start byte, kind, and the byte
/// where its body begins (just past the opener tag).
fn find_top_level_opener(src: &str) -> Option<(usize, EnvelopeKind, usize)> {
    let mut offset = 0usize;
    let mut in_fence = false;
    let mut fence_marker = "";
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim();
        if in_fence {
            if trimmed == fence_marker {
                in_fence = false;
            }
            offset += line.len();
            continue;
        }
        if let Some(marker) = markdown_fence_marker(trimmed) {
            in_fence = true;
            fence_marker = marker;
            offset += line.len();
            continue;
        }
        if let Some((rel, kind)) = first_opener_in(line) {
            let opener_start = offset + rel;
            return Some((opener_start, kind, opener_start + kind.opener().len()));
        }
        offset += line.len();
    }
    None
}

/// The bare fence marker a line opens a markdown code fence with, if any.
fn markdown_fence_marker(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with(BACKTICK_FENCE) {
        Some(BACKTICK_FENCE)
    } else if trimmed.starts_with(TILDE_FENCE) {
        Some(TILDE_FENCE)
    } else {
        None
    }
}

/// The earliest envelope opener in one line, if any. `<tool_calls>` and
/// `<tool_call>` are unambiguous: the char after `<tool_call` is `s` in the
/// former and `>` in the latter, so neither is a substring of the other.
fn first_opener_in(line: &str) -> Option<(usize, EnvelopeKind)> {
    [
        (line.find("<tool_calls>"), EnvelopeKind::ToolCalls),
        (line.find("<tool_code>"), EnvelopeKind::ToolCode),
        (line.find("<tool_call>"), EnvelopeKind::ToolCall),
    ]
    .into_iter()
    .filter_map(|(pos, kind)| pos.map(|p| (p, kind)))
    .min_by_key(|(p, _)| *p)
}

/// Join the text before and after the envelope back into prose.
fn collapse_prose_around(before: &str, after: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let before = before.trim();
    if !before.is_empty() {
        parts.push(before);
    }
    let after = after.trim();
    if !after.is_empty() {
        parts.push(after);
    }
    parts.join("\n")
}

/// Parse the body of a `<tool_calls>` envelope, choosing the dialect from its
/// first structural token: a JSON object or a `<tool>` marker routes to the
/// `<tool>`-marker JSON dialect (preserving its marker-required rejection); any
/// other opening tag (a tag named after the tool) routes to the XML dialect.
/// Returns the parsed calls plus the slice past the structurally-recognized
/// `</tool_calls>` close (empty at end-of-input).
fn parse_tool_calls_body<'a>(body: &'a str, close: &str) -> EnvelopeBody<'a> {
    let trimmed = body.trim_start();
    if trimmed.starts_with('{') {
        return parse_tool_marker_json_body(body, close);
    }
    match first_tag_name(trimmed) {
        Some(name) if is_known_marker(&name) => parse_tool_marker_json_body(body, close),
        Some(_) => parse_xml_tool_calls(body, close),
        None => parse_tool_marker_json_body(body, close),
    }
}

/// The lowercased name of the first XML tag (open or close) in `s`, if any.
fn first_tag_name(s: &str) -> Option<String> {
    let lt = s.find('<')?;
    let after = &s[lt + 1..];
    let after = after.strip_prefix('/').unwrap_or(after);
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name.to_ascii_lowercase())
    }
}

/// True for the structural markers that never name a tool call.
fn is_known_marker(name: &str) -> bool {
    matches!(name, "tool" | "tool_call" | "tool_calls" | "tool_code")
}

/// The `<tool>`-marker JSON dialect body (the classic chat-template shape):
///
/// ```text
/// <tool>
/// {"name":"look","file":"src/lib.rs"}
/// {"name":"run","command":"cargo test"}
/// ```
///
/// Every `<tool>` marker must be followed by a complete JSON object before
/// another marker, a `</tool>` close, or the end of the body. Partial markup is
/// fail-loud rather than dispatching a guessed action. The `</tool_calls>`
/// `close` is matched only at a boundary (never inside a JSON object), so a
/// literal `</tool_calls>` inside a JSON string is preserved; the slice past the
/// close is returned as trailing prose.
fn parse_tool_marker_json_body<'a>(body: &'a str, close: &str) -> EnvelopeBody<'a> {
    const TOOL_OPEN: &str = "<tool>";
    const TOOL_CLOSE: &str = "</tool>";

    let mut body = body.trim_start();
    let mut saw_tool_marker = false;
    let mut tool_marker_active = false;
    let mut pending_tool_object = false;
    let mut calls = Vec::new();
    let after = loop {
        if let Some(after) = body.strip_prefix(close) {
            break after;
        }
        if body.is_empty() {
            break "";
        }
        if let Some(rest) = body.strip_prefix(TOOL_OPEN) {
            if pending_tool_object {
                return Err(env_err(
                    "a `<tool>` marker must be followed by a JSON object before another `<tool>` marker"
                        .to_string(),
                ));
            }
            saw_tool_marker = true;
            tool_marker_active = true;
            pending_tool_object = true;
            body = rest.trim_start();
            continue;
        }
        if let Some(rest) = body.strip_prefix(TOOL_CLOSE) {
            if !tool_marker_active {
                return Err(env_err(
                    "found an unmatched `</tool>` close without a preceding `<tool>` marker"
                        .to_string(),
                ));
            }
            if pending_tool_object {
                return Err(env_err(
                    "a `<tool>` marker must be followed by a JSON object before `</tool>`"
                        .to_string(),
                ));
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
            return Err(env_err(detail.to_string()));
        }
        if !body.starts_with('{') {
            return Err(env_err(format!(
                "expected a `{TOOL_OPEN}` marker or a JSON object, found `{}`",
                preview_str(body, 80)
            )));
        }

        let mut values = serde_json::Deserializer::from_str(body).into_iter::<serde_json::Value>();
        let value = match values.next() {
            Some(Ok(value)) => value,
            Some(Err(error)) if error.is_eof() => {
                return Err(env_err(
                    "a JSON tool object ended before its closing `}`".to_string(),
                ));
            }
            Some(Err(error)) => {
                return Err(env_err(format!("invalid JSON tool object: {error}")));
            }
            None => {
                return Err(env_err(
                    "expected a JSON tool object after `<tool>`".to_string(),
                ));
            }
        };
        let consumed = values.byte_offset();
        let obj = match value {
            serde_json::Value::Object(obj) => obj,
            _ => {
                return Err(env_err(
                    "each chat-template tool entry must be a JSON object".to_string(),
                ));
            }
        };
        match normalize_json_tool_call_object(obj, true) {
            Ok(call) => calls.push(call),
            Err(error) => return Err(env_err(error.into_message())),
        }
        pending_tool_object = false;
        body = body[consumed..].trim_start();
    };

    if pending_tool_object {
        return Err(env_err(
            "a `<tool>` marker ended without a complete JSON object".to_string(),
        ));
    }
    if !saw_tool_marker {
        return Err(env_err(
            "the envelope contained no `<tool>` marker".to_string(),
        ));
    }
    if calls.is_empty() {
        return Err(env_err(
            "the envelope contained no JSON tool object".to_string(),
        ));
    }
    Ok((calls, after))
}

/// Parse a body that is a bare list of consecutive JSON tool objects (the
/// `<tool_code>` / `<tool_call>` tag-wrapping-JSON dialect). Inline arguments
/// beside `name` are accepted, matching the chat-template policy. The `close`
/// tag is matched only at a boundary between complete objects (never inside
/// one), so a literal `</tool_code>` / `</tool_call>` inside a JSON string value
/// survives as content; the slice past the close is returned as trailing prose.
fn parse_json_object_list<'a>(body: &'a str, close: &str) -> EnvelopeBody<'a> {
    let mut body = body.trim_start();
    let mut calls = Vec::new();
    let after = loop {
        if let Some(after) = body.strip_prefix(close) {
            break after;
        }
        if body.is_empty() {
            break "";
        }
        if !body.starts_with('{') {
            return Err(env_err(format!(
                "expected a JSON tool object, found `{}`",
                preview_str(body, 80)
            )));
        }
        let mut values = serde_json::Deserializer::from_str(body).into_iter::<serde_json::Value>();
        let value = match values.next() {
            Some(Ok(value)) => value,
            Some(Err(error)) if error.is_eof() => {
                return Err(env_err(
                    "a JSON tool object ended before its closing `}`".to_string(),
                ));
            }
            Some(Err(error)) => {
                return Err(env_err(format!("invalid JSON tool object: {error}")));
            }
            None => break "",
        };
        let consumed = values.byte_offset();
        let obj = match value {
            serde_json::Value::Object(obj) => obj,
            _ => {
                return Err(env_err(
                    "each chat-template tool entry must be a JSON object".to_string(),
                ));
            }
        };
        match normalize_json_tool_call_object(obj, true) {
            Ok(call) => calls.push(call),
            Err(error) => return Err(env_err(error.into_message())),
        }
        body = body[consumed..].trim_start();
    };
    if calls.is_empty() {
        return Err(env_err(
            "the envelope contained no JSON tool object".to_string(),
        ));
    }
    Ok((calls, after))
}

/// Parse the tag-per-call XML dialect inside `<tool_calls>`:
///
/// ```text
/// <look>
/// <file>
/// src/writer.zig
/// </file>
/// </look>
/// ```
///
/// Each top-level block is a call named after its tag; each nested tag is a
/// string argument (trimmed, multi-line values kept verbatim). Any tag whose
/// name is a known structural marker is rejected, and an inner tag left unclosed
/// at end of input is a violation — a truncated/guessed call never dispatches.
/// The `</tool_calls>` `close` is matched only at a boundary between complete
/// call blocks (never inside an argument value), so a literal `</tool_calls>`
/// inside an argument survives; the slice past the close is returned as prose.
fn parse_xml_tool_calls<'a>(body: &'a str, close: &str) -> EnvelopeBody<'a> {
    let mut rest = body.trim_start();
    let mut calls = Vec::new();
    let after = loop {
        if let Some(after) = rest.strip_prefix(close) {
            break after;
        }
        if rest.is_empty() {
            break "";
        }
        if !rest.starts_with('<') {
            return Err(env_err(format!(
                "expected a tool-call tag inside `<tool_calls>`, found `{}`",
                preview_str(rest, 60)
            )));
        }
        let (tag_name, after_open) = parse_open_tag(rest)?;
        if is_known_marker(&tag_name.to_ascii_lowercase()) {
            return Err(env_err(format!(
                "`<{tag_name}>` is a structural marker, not a tool-call tag"
            )));
        }
        let call_close = format!("</{tag_name}>");
        let (args, after_close) = parse_xml_call_args(after_open, &call_close, &tag_name)?;
        calls.push(super::super::normalize_tool_call_shape(
            &tag_name,
            serde_json::Value::Object(args),
        ));
        rest = after_close.trim_start();
    };
    if calls.is_empty() {
        return Err(env_err(
            "the `<tool_calls>` envelope contained no tool-call tags".to_string(),
        ));
    }
    Ok((calls, after))
}

/// Parse one `<NAME ...>` opening tag. Returns the tag name and the slice just
/// past `>`. An unexpected close tag or a tag not closed with `>` before end of
/// input is a violation.
fn parse_open_tag(s: &str) -> Result<(String, &str), BlockError> {
    let after_lt = &s[1..];
    if after_lt.starts_with('/') {
        return Err(env_err(format!(
            "unexpected close tag `{}`",
            preview_str(s, 40)
        )));
    }
    let gt = after_lt.find('>').ok_or_else(|| {
        env_err("an XML tool tag was not closed with `>` before end of output".to_string())
    })?;
    let name: String = after_lt[..gt]
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if name.is_empty() {
        return Err(env_err("an XML tool tag had an empty name".to_string()));
    }
    Ok((name, &after_lt[gt + 1..]))
}

/// Parse the `<arg>value</arg>` sequence inside one XML call tag, up to `close`
/// (`</NAME>`). Values are trimmed; an unclosed call tag or argument tag at end
/// of input is a violation.
fn parse_xml_call_args<'a>(
    mut s: &'a str,
    close: &str,
    tag_name: &str,
) -> Result<(serde_json::Map<String, serde_json::Value>, &'a str), BlockError> {
    let mut args = serde_json::Map::new();
    loop {
        let t = s.trim_start();
        if let Some(after) = t.strip_prefix(close) {
            return Ok((args, after));
        }
        if t.is_empty() {
            return Err(env_err(format!(
                "the `<{tag_name}>` call tag was not closed with `{close}` before end of output"
            )));
        }
        if !t.starts_with('<') {
            return Err(env_err(format!(
                "expected an argument tag or `{close}` inside `<{tag_name}>`, found `{}`",
                preview_str(t, 60)
            )));
        }
        let (arg_name, after_open) = parse_open_tag(t)?;
        // A repeated argument tag makes the call's value for that argument
        // ambiguous. Fail loud rather than silently taking the last write, so a
        // guessed/ambiguous call is never dispatched.
        if args.contains_key(&arg_name) {
            return Err(env_err(format!(
                "the `<{tag_name}>` call repeated the `<{arg_name}>` argument; a call with an ambiguous duplicate argument is not executed"
            )));
        }
        let arg_close = format!("</{arg_name}>");
        let end = after_open.find(&arg_close).ok_or_else(|| {
            env_err(format!(
                "the `<{arg_name}>` argument tag was not closed with `{arg_close}` before end of output"
            ))
        })?;
        let value = after_open[..end].trim().to_string();
        args.insert(arg_name, serde_json::Value::String(value));
        s = &after_open[end + arg_close.len()..];
    }
}

fn contains_legacy_tool_call_markup(src: &str) -> bool {
    src.contains("<tool_call>")
        || src.contains("<tool_call ")
        || src.contains("<toolcall>")
        || src.contains("<toolcall ")
}

#[cfg(test)]
mod tests;
