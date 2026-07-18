//! The verbatim content lane for the fenced-JSON tool protocol (harn#5033).
//!
//! #5015 unified the tool-call *envelope* on ```` ```tool ```` JSON blocks and,
//! in doing so, relocated all payload fragility into JSON-string escaping. A
//! backslash/quote/newline-dense body (Zig multiline, Go raw strings, regex,
//! LaTeX, Windows paths) needs a uniform 2× backslash transform across hundreds
//! of lines that a cheap model cannot hold; the escaping lottery never
//! converges. This module adds a NON-escaped path for such a payload, WITHOUT
//! forking the grammar #5015 unified.
//!
//! ## One heredoc-scanner authority
//!
//! The verbatim body is an ordinary `<<TAG` heredoc, decoded by the shared
//! [`scan_heredoc`] authority — the SAME scanner the text lane
//! (`ts_value_parser::parse_heredoc`) uses. This module is thin wiring: it never
//! reimplements heredoc or line-count logic, it only (1) locates the trailing
//! heredoc bodies of a ```` ```tool ```` block and delegates each to
//! [`scan_heredoc`], and (2) binds each decoded body to the argument that
//! declared it. There is no second decoder, so no parallel dialect.
//!
//! ## Contract
//!
//! A ```` ```tool ```` block stays canonical #5015/#5037 JSON UNLESS an argument's
//! JSON string value is a heredoc opener (`<<TAG` or the collision-proof
//! count-anchored `<<TAG:N`). For each such argument the matching heredoc body
//! trails the JSON header:
//!
//! ```text
//! ```tool
//! { "name": "edit", "args": { "path": "x.zig", "old_string": "ANCHOR",
//!   "new_string": "<<NEW:921" } }
//! <<NEW:921
//! ...921 raw lines: ``` fences, <<EOF, </tool>, the tag itself, CRLF...
//! NEW
//! ```
//! ```
//!
//! The declaration is in-JSON (a valid string value) and the count `:N` makes the
//! close collision-proof: a bare `NEW` line, a ```` ``` ````, or any other byte
//! inside the `N` lines is content, so nothing in the payload can forge the close
//! — the exact guarantee an author-chosen `<<EOF` tag can never give, and the one
//! addition #5033 needs over #3063's escape-free channel. A block whose arguments
//! are all ordinary strings is byte-identical to before.

use super::super::syntax::scan_heredoc;
use super::BlockError;

/// One decoded verbatim heredoc: the exact opener string that declared it (e.g.
/// `<<NEW:921`) and the byte-exact body [`scan_heredoc`] returned.
pub(super) struct VerbatimSegment {
    pub opener: String,
    pub value: String,
}

/// The outcome of walking one ```` ```tool ```` block from just past its opener
/// fence line to its close fence. `header` is the block text with every trailing
/// heredoc body removed (the pure-JSON header the existing LAYER 1 parses).
/// `segments` are the decoded heredoc bodies. `error` is set when a heredoc body
/// failed to close (a bad count, a truncated body) so the block fails loud rather
/// than half-applying. `block_end` is the byte offset just past the close fence
/// line (or end of input on an implicit close).
pub(super) struct ToolBlock {
    pub header: String,
    pub segments: Vec<VerbatimSegment>,
    pub error: Option<BlockError>,
    pub block_end: usize,
}

/// True when a JSON string value is a heredoc opener the verbatim lane binds:
/// `<<`, an optional `'`/`"` quote, a `[A-Za-z0-9_]` tag, the matching close
/// quote, an optional `:N` count, and nothing else. A value that merely *starts*
/// with `<<` (a literal `<<EOF` line of file content) is not an opener, so the
/// #5015 promise that such bytes survive as content is preserved.
pub(super) fn is_heredoc_opener(value: &str) -> bool {
    let rest = match value.strip_prefix("<<") {
        Some(rest) => rest,
        None => return false,
    };
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"');
    let rest = quote.map_or(rest, |_| &rest[1..]);
    let tag_len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    if tag_len == 0 {
        return false;
    }
    let mut after = &rest[tag_len..];
    if let Some(q) = quote {
        after = match after.strip_prefix(q) {
            Some(after) => after,
            None => return false,
        };
    }
    match after.strip_prefix(':') {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => after.is_empty(),
    }
}

/// Counted openers are unambiguous framing declarations. Unlike legacy
/// `<<TAG`, they must never survive as literal arguments when a body is absent.
fn is_counted_heredoc_opener(value: &str) -> bool {
    is_heredoc_opener(value)
        && value.rsplit_once(':').is_some_and(|(_, count)| {
            !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Walk one ```` ```tool ```` block, delegating each trailing heredoc body to the
/// shared [`scan_heredoc`] authority, and return its header, decoded heredoc
/// bodies, and the offset past its close fence.
///
/// This is the single authority for the block's end AND its heredoc bodies, so
/// close-fence detection and body extraction never diverge. `body_start` is the
/// byte offset of the first line after the opener fence; `close_fence` is the
/// matching bare fence marker.
///
/// The header runs until the first line that opens a heredoc (`<<...`); from
/// there the trailing lines are heredoc bodies, each decoded by [`scan_heredoc`]
/// — so a ```` ``` ```` (or the tag itself) inside a count-anchored body is
/// content, never an early close. A ```` ```tool ```` separator in header context
/// is #5037's implicit close + reopen. End of input is an implicit close.
pub(super) fn consume_tool_block(src: &str, body_start: usize, close_fence: &str) -> ToolBlock {
    let mut header = String::new();
    let mut segments: Vec<VerbatimSegment> = Vec::new();
    let mut error: Option<BlockError> = None;
    let mut in_bodies = false;

    let mut cursor = body_start;
    let block_end = loop {
        if cursor >= src.len() {
            break src.len();
        }
        let (line, line_end, next) = line_at(src, cursor);
        if line.trim() == close_fence {
            break next;
        }
        let indent = line.len() - line.trim_start().len();
        let trimmed = &line[indent..];
        if trimmed.starts_with("<<") {
            // The trailing heredoc body region. Delegate the whole body — count
            // close, byte-exact slice, `\r` preservation — to scan_heredoc; this
            // module never re-derives any of it.
            in_bodies = true;
            let opener_off = cursor + indent;
            match scan_heredoc(src, opener_off) {
                Ok(span) => {
                    segments.push(VerbatimSegment {
                        opener: trimmed.trim_end().to_string(),
                        value: src[span.content].to_string(),
                    });
                    cursor = advance_past_line(src, span.end);
                }
                Err(_) => {
                    error.get_or_insert_with(|| BlockError::InvalidJson {
                        detail: format!(
                            "verbatim heredoc `{}` did not close on the line its `:N` count \
                             declared; recount the body lines and re-emit the whole call",
                            trimmed.trim_end()
                        ),
                    });
                    // Bounded failure: skip this opener line and let the next bare
                    // close fence (or EOF) end the block.
                    cursor = next;
                }
            }
            continue;
        }
        if !in_bodies && super::fence_open_kind(line).is_some() {
            // #5037 double-fence separator: implicit close + reopen. Do not
            // consume the line.
            break cursor;
        }
        header.push_str(&src[cursor..line_end]);
        header.push('\n');
        cursor = next;
    };

    ToolBlock {
        header,
        segments,
        error,
        block_end,
    }
}

/// The byte offset of the start of the line after the one containing `pos`
/// (scan_heredoc leaves the cursor just past the closing tag; the body region
/// resumes on the next physical line).
fn advance_past_line(src: &str, pos: usize) -> usize {
    match src[pos..].find('\n') {
        Some(rel) => pos + rel + 1,
        None => src.len(),
    }
}

/// The line beginning at `start`: its content without the terminator, the byte
/// offset of the terminator, and the byte offset of the next line.
pub(super) fn line_at(src: &str, start: usize) -> (&str, usize, usize) {
    match src[start..].find('\n') {
        Some(rel) => {
            let nl = start + rel;
            (&src[start..nl], nl, nl + 1)
        }
        None => (&src[start..], src.len(), src.len()),
    }
}

/// Bind each argument in `args` whose value is a heredoc opener to the trailing
/// heredoc body that declared the same opener, claiming it from `pool`.
///
/// An ambiguous uncounted opener with no matching body is left unchanged so a
/// literal `<<EOF` survives. A counted opener is an explicit framing declaration
/// and fails closed when no matching body exists.
pub(super) fn apply_segments(
    args: &mut serde_json::Map<String, serde_json::Value>,
    pool: &mut Vec<VerbatimSegment>,
) -> Result<(), BlockError> {
    let openers: Vec<(String, String)> = args
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .filter(|value| is_heredoc_opener(value))
                .map(|value| (key.clone(), value.to_string()))
        })
        .collect();

    for (_, opener) in &openers {
        if !is_counted_heredoc_opener(opener) {
            continue;
        }
        let required = openers
            .iter()
            .filter(|(_, candidate)| candidate == opener)
            .count();
        let available = pool
            .iter()
            .filter(|segment| &segment.opener == opener)
            .count();
        if available < required {
            return Err(BlockError::InvalidJson {
                detail: format!(
                    "counted verbatim declaration `{opener}` has {available} matching bodies, expected {required}"
                ),
            });
        }
    }

    for (key, opener) in openers {
        if let Some(index) = pool.iter().position(|segment| segment.opener == opener) {
            let segment = pool.remove(index);
            args.insert(key, serde_json::Value::String(segment.value));
        }
    }
    Ok(())
}
