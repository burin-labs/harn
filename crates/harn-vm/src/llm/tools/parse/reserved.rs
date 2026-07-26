//! Recovery for malformed `reserved_tool_call_token` wire openers.
//!
//! A `reserved_tool_call_token` model emits `[[CALL]]…[[/CALL]]` on the wire
//! ([`crate::llm::tool_delimiter`]); `wire_to_canonical` maps the exact markers
//! back to `<tool_call>` before parsing. A truncated `[[CALL]` opener (one bracket
//! short) is *not* rewritten, so it reaches the tagged parser as literal text and
//! would be swept into assistant prose — hiding a likely-lost action (harn#4486).
//! This module normalizes such a stub, at a genuine top-level position, into the
//! canonical opener so the scanner's existing recover-or-truncated machinery runs.

use super::super::{text_tool_call_block, TEXT_TOOL_CALL_TAG};
use super::syntax::{match_tool_call_block, render_canonical_call};
use super::tagged::{leading_call_name, parse_single_tool_call, unclosed_tool_call_open};
use crate::text_index::TextIndex;
use crate::value::VmValue;

/// One bracket short of the `[[CALL]]` reserved-token wire opener
/// (`crate::llm::tool_delimiter::WIRE_TOOL_CALL_OPEN`).
const WIRE_MALFORMED_CALL_OPENER: &str = "[[CALL]";

/// Recover a top-level malformed reserved-token call opener `[[CALL]` as either a
/// canonical tool call or a typed parse error — never assistant prose (harn#4486).
///
/// Returns `Some(new_cursor)` when the stub at `cursor` was handled, `None` to fall
/// through to normal scanning. The stub is normalized to the canonical `<tool_call>`
/// opener and routed through the same recover-or-truncated machinery as a real
/// block: a following complete known call is recovered into `calls`; a truncated one
/// becomes a typed `errors` diagnostic and consumes the remainder so the stub can
/// never reach `report_stray`. Fires only at a top-level, non-fenced line-start
/// position, and excludes both the well-formed `[[CALL]]` (whose next byte is `]`,
/// already canonicalized upstream) and unrelated bracket text such as `[[CALLER]`.
pub(super) fn recover_malformed_call_opener(
    index: &TextIndex,
    src: &str,
    cursor: usize,
    tools_val: Option<&VmValue>,
    calls: &mut Vec<serde_json::Value>,
    errors: &mut Vec<String>,
    canonical_parts: &mut Vec<String>,
) -> Option<usize> {
    let tail = src[cursor..].strip_prefix(WIRE_MALFORMED_CALL_OPENER)?;
    // Exclude the well-formed `[[CALL]]` opener (the byte after the stub is `]`);
    // upstream `wire_to_canonical` already rewrote it to `<tool_call>`.
    if tail.starts_with(']') {
        return None;
    }
    if !index.is_line_leading(src, cursor) || index.inside_markdown_fence(cursor) {
        return None;
    }
    // Normalize the stub to the canonical opener and reuse the block machinery. The
    // opener is the only rewritten span, so any byte offset past it maps back to the
    // original by a fixed length delta.
    let canonical_open = format!("<{TEXT_TOOL_CALL_TAG}>");
    let delta = canonical_open.len() - WIRE_MALFORMED_CALL_OPENER.len();
    let synthetic = format!("{canonical_open}{tail}");

    if let Some((body, after)) = match_tool_call_block(&synthetic, 0, TEXT_TOOL_CALL_TAG) {
        match parse_single_tool_call(body, tools_val) {
            Ok(call) => {
                let name = call
                    .get("name")
                    .and_then(|name| name.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = call
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                canonical_parts.push(text_tool_call_block(&render_canonical_call(&name, &args)));
                calls.push(call);
            }
            Err(msg) => errors.push(msg),
        }
        return Some(cursor + after - delta);
    }

    // No matching close: a truncated opener. Emit the same typed truncation
    // diagnostic as the canonical unclosed-`<tool_call>` branch (recovering the
    // partial call name when possible) and consume the remainder, so neither the
    // stub nor its partial body can render as assistant prose.
    let truncated_body =
        unclosed_tool_call_open(&synthetic, 0).map(|open_len| &synthetic[open_len..]);
    match truncated_body.and_then(|body| leading_call_name(body, tools_val)) {
        Some(name) => errors.push(format!(
            "TOOL CALL TRUNCATED: `<tool_call>` opening `{name}(...)` was never \
             closed — the response appears to have been cut off mid-call (likely \
             the model hit its max output token limit). Re-emit the complete \
             `<tool_call>{name}({{ ... }})</tool_call>` block; for very large \
             arguments, split the work into smaller calls."
        )),
        None => errors.push(
            "TOOL CALL TRUNCATED: a `<tool_call>` block was opened but never \
             closed — the response appears to have been cut off (likely the model \
             hit its max output token limit). Re-emit the complete \
             `<tool_call>name({ ... })</tool_call>` block, splitting very large \
             arguments into smaller calls if needed."
                .to_string(),
        ),
    }
    Some(src.len())
}
