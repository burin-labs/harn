//! The structural token stream: one linear scan that delimits the top-level
//! units of a model response.
//!
//! This is the Rust half of the cut line drawn in `parse/PORT_PLAN.md`. Rust
//! does everything proportional to **bytes** — find where each unit ends, slice
//! it, and report the couple of facts it had to compute along the way. Harn
//! does everything proportional to **candidates** — which tags are tool calls,
//! the recovery ladders, argument policy, violation classification, and the
//! composition of passes. Nothing in this module decides what a unit *means*.
//!
//! Delimiting still needs vocabulary, so the vocabulary arrives as data in
//! [`ScanSpec`] and is never restated here. See `spec.rs`.
//!
//! ## The drop invariant, structurally
//!
//! Every branch computes an `end` and hands it to [`Scan::emit`], which is the
//! only thing that moves the cursor. A unit's `start` is *always* the cursor at
//! the moment of the call, so a branch cannot consume a span wider than the
//! unit it reports — the gap that lets a byte vanish unrecorded does not exist
//! to be forgotten. The one other advance is [`Scan::skip_whitespace`], which
//! by construction passes over whitespace only.
//!
//! That is the structural form of "the drop record precedes the cursor
//! advance". The per-branch spelling of the same discipline is what produced
//! harn#35389641b: eleven scanner branches recorded a dropped fragment and the
//! twelfth did not, and a detector three layers away went quiet on a real
//! dropped dialect. `units_tile_the_source` in `tests.rs` pins it empirically.

mod frames;
mod head;
mod spec;
#[cfg(test)]
mod tests;
mod unit;

use super::syntax::{
    find_close_tag, ident_length, match_block, match_tool_call_block, parse_ts_call_from,
    skip_heredoc_body, strip_thinking_tags, CloseScan,
};
use crate::text_index::TextIndex;

pub(crate) use head::call_head;
pub(crate) use spec::ScanSpec;
pub(crate) use unit::{Unit, UnitPayload};

/// The scanned source and the units delimited within it.
///
/// `source` is the post-thinking-strip text. Every unit offset indexes it,
/// never the caller's original string — stripping rewrites the input before
/// scanning, which is why a dropped fragment carries text rather than an
/// offset into the caller's bytes.
pub(crate) struct ScanOutput {
    pub(crate) source: String,
    pub(crate) units: Vec<Unit>,
}

impl ScanOutput {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "source": self.source,
            "units": self.units.iter().map(Unit::to_json).collect::<Vec<_>>(),
        })
    }
}

/// Delimit the top-level units of `text` under `spec`.
pub(crate) fn scan_units(text: &str, spec: &ScanSpec) -> ScanOutput {
    let source = if spec.strip_thinking {
        strip_thinking_tags(text).into_owned()
    } else {
        text.to_string()
    };
    let units = Scan::new(&source, spec).run();
    ScanOutput { source, units }
}

/// The scan state. `cursor` is private to the two advancing methods; every
/// branch works with local offsets and hands the end to [`Scan::emit`].
struct Scan<'a> {
    src: &'a str,
    spec: &'a ScanSpec,
    index: TextIndex,
    /// `<tag` for every call tag, precomputed: the Harmony header tail and the
    /// unknown-tag branch both ask where the next call block starts.
    call_tag_opens: Vec<String>,
    cursor: usize,
    /// Byte position just past the most recently consumed top-level block. A
    /// tag that follows a consumed block with only whitespace between them is
    /// structurally top-level even mid-line: models chain blocks as
    /// `…</tool_call><tool_call>…` on one line.
    last_block_end: usize,
    units: Vec<Unit>,
}

impl<'a> Scan<'a> {
    fn new(src: &'a str, spec: &'a ScanSpec) -> Self {
        Self {
            src,
            spec,
            index: TextIndex::build(src),
            call_tag_opens: spec.call_tags.iter().map(|tag| format!("<{tag}")).collect(),
            cursor: 0,
            last_block_end: 0,
            units: Vec::new(),
        }
    }

    /// The one cursor-advancing funnel. The emitted unit spans exactly the
    /// bytes consumed, so no branch can advance past text it did not report.
    fn emit(&mut self, end: usize, payload: UnitPayload) {
        assert!(
            end > self.cursor && end <= self.src.len(),
            "scan unit must advance the cursor within the source: {}..{end} of {}",
            self.cursor,
            self.src.len()
        );
        self.units.push(Unit {
            start: self.cursor,
            end,
            payload,
        });
        self.cursor = end;
    }

    /// Emit a unit that also counts as a consumed top-level block, so a tag
    /// abutting it on the same line still reads as top-level.
    fn emit_block(&mut self, end: usize, payload: UnitPayload) {
        self.emit(end, payload);
        self.last_block_end = self.cursor;
    }

    fn skip_whitespace(&mut self) {
        let bytes = self.src.as_bytes();
        while self.cursor < bytes.len() && bytes[self.cursor].is_ascii_whitespace() {
            self.cursor += 1;
        }
    }

    fn run(mut self) -> Vec<Unit> {
        while self.cursor < self.src.len() {
            self.skip_whitespace();
            if self.cursor >= self.src.len() {
                break;
            }
            self.step();
        }
        self.units
    }

    /// One turn of the scan, in the same branch order as the Rust parser this
    /// stream replaces (`tagged/mod.rs::parse_text_tool_calls_with_tools`).
    fn step(&mut self) {
        let cursor = self.cursor;
        let bytes = self.src.as_bytes();

        // 1. A reserved wire opener standing in for a call tag.
        if self.step_reserved_opener() {
            return;
        }

        // 2. A run with no structural opener, heredoc-aware so a bare
        //    `name({ key: <<EOF … EOF })` survives the chunker.
        if bytes[cursor] != b'<' {
            let end = self.text_run_end(cursor);
            self.emit(
                end,
                UnitPayload::Text {
                    text: self.src[cursor..end].to_string(),
                },
            );
            return;
        }

        // 3. A Harmony tool-call header line.
        let harmony = &self.spec.harmony;
        if let Some(end) = frames::tool_call_line(harmony, self.src, cursor) {
            self.emit_block(
                end,
                UnitPayload::HarmonyLine {
                    text: self.src[cursor..end].to_string(),
                },
            );
            return;
        }

        // 4. A corrupted wrapper open/close or a bare frame marker.
        if let Some(end) = frames::corrupted_opener(harmony, &self.call_tag_opens, self.src, cursor)
            .or_else(|| frames::frame_marker(harmony, &self.call_tag_opens, self.src, cursor))
        {
            self.emit_block(end, UnitPayload::HarmonySkip);
            return;
        }

        // 5. Inside a fence, or not line-leading and not abutting a consumed
        //    block. One line. Fencing does not make a call inert downstream.
        let adjacent_to_block = self.last_block_end > 0
            && cursor >= self.last_block_end
            && self.src[self.last_block_end..cursor]
                .chars()
                .all(char::is_whitespace);
        if (!adjacent_to_block && !self.index.is_line_leading(self.src, cursor))
            || self.index.inside_markdown_fence(cursor)
        {
            let end = self.src[cursor..]
                .find('\n')
                .map_or(self.src.len(), |offset| cursor + offset);
            self.emit(
                end,
                UnitPayload::FencedLine {
                    text: self.src[cursor..end].to_string(),
                },
            );
            return;
        }

        // 6/7. A call block, closed or open-to-EOF.
        if self.step_call_tag(cursor) {
            return;
        }

        // 8. A plain `<tag>body</tag>` block.
        for tag in &self.spec.block_tags {
            if let Some((body, end)) = match_block(self.src, cursor, tag) {
                let payload = UnitPayload::Block {
                    tag: tag.clone(),
                    body: body.to_string(),
                    head: call_head(body),
                    reserved: false,
                };
                self.emit_block(end, payload);
                return;
            }
        }

        // 9. `<name(…)>` for a known tool. The extent depends on parsing the
        //    expression, so Rust parses it — the argument value comes along
        //    because it was already computed, not because Rust judged it.
        if let Some((name, arguments, end)) = self.angle_wrapped_call(cursor) {
            self.emit_block(end, UnitPayload::AngleCall { name, arguments });
            return;
        }

        // 10. Top-level chat-template function markup.
        for markup in &self.spec.markup_openers {
            if self.src[cursor..].starts_with(&markup.opener) {
                let end = self.src[cursor..]
                    .find(&markup.close)
                    .map_or(self.src.len(), |offset| {
                        cursor + offset + markup.close.len()
                    });
                let payload = UnitPayload::Markup {
                    opener: markup.opener.clone(),
                    text: self.src[cursor..end].to_string(),
                };
                self.emit_block(end, payload);
                return;
            }
        }

        // 11. An orphan close for a call tag.
        for tag in &self.spec.call_tags {
            let close = format!("</{tag}>");
            if self.src[cursor..].starts_with(&close) {
                let end = cursor + close.len();
                self.emit_block(end, UnitPayload::StrayClose { tag: tag.clone() });
                return;
            }
        }

        // 12. A contentless wrapper tag, either direction.
        for tag in &self.spec.wrapper_tags {
            for form in [format!("<{tag}>"), format!("</{tag}>")] {
                if self.src[cursor..].starts_with(&form) {
                    let end = cursor + form.len();
                    self.emit_block(end, UnitPayload::Wrapper { tag: tag.clone() });
                    return;
                }
            }
        }

        // 13. An unknown or unclosed tag fragment, to `>` or end of line.
        let mut end = cursor + 1;
        while end < bytes.len() && bytes[end] != b'>' && bytes[end] != b'\n' {
            end += 1;
        }
        if end < bytes.len() && bytes[end] == b'>' {
            end += 1;
        }
        let raw = self.src[cursor..end].to_string();
        let name = tag_fragment_name(&raw);
        self.emit(end, UnitPayload::Tag { raw, name });
    }

    /// Branch 1. A `[[CALL]`-style stub is one bracket short of a wire opener
    /// that upstream would have rewritten, so it survives as literal text and,
    /// starting with `[`, would otherwise be swept into the text run above —
    /// hiding a likely-lost action (harn#4486). Normalize it to the canonical
    /// call tag and emit the ordinary block unit, flagged `reserved` so Harn
    /// can say so in diagnostics.
    fn step_reserved_opener(&mut self) -> bool {
        let cursor = self.cursor;
        let Some(opener) = self
            .spec
            .reserved_openers
            .iter()
            .find(|opener| is_truncated_reserved_opener(&self.src[cursor..], opener))
        else {
            return false;
        };
        // A stub inside a fence or mid-line is an example, not a lost action.
        if !self.index.is_line_leading(self.src, cursor) || self.index.inside_markdown_fence(cursor)
        {
            return false;
        }
        let Some(canonical_tag) = self.spec.call_tags.first() else {
            return false;
        };
        let tail = &self.src[cursor + opener.len()..];
        let canonical_open = format!("<{canonical_tag}>");
        let delta = canonical_open.len() - opener.len();
        let synthetic = format!("{canonical_open}{tail}");

        for tag in &self.spec.call_tags {
            if let Some((body, after)) = match_tool_call_block(&synthetic, 0, tag) {
                let payload = UnitPayload::Block {
                    tag: tag.clone(),
                    body: body.to_string(),
                    head: call_head(body),
                    reserved: true,
                };
                self.emit_block(cursor + after - delta, payload);
                return true;
            }
        }
        let payload = UnitPayload::UnclosedBlock {
            tag: canonical_tag.clone(),
            body: tail.to_string(),
            head: call_head(tail),
            reserved: true,
        };
        self.emit_block(self.src.len(), payload);
        true
    }

    /// Branches 6 and 7. A closed call block, else an open one whose body runs
    /// to EOF — the signature of an output truncated mid-call, which Harn
    /// turns into a recovery ladder or a truncation diagnostic.
    fn step_call_tag(&mut self, cursor: usize) -> bool {
        for tag in &self.spec.call_tags {
            if let Some((body, end)) = match_tool_call_block(self.src, cursor, tag) {
                let payload = UnitPayload::Block {
                    tag: tag.clone(),
                    body: body.to_string(),
                    head: call_head(body),
                    reserved: false,
                };
                self.emit_block(end, payload);
                return true;
            }
        }
        for tag in &self.spec.call_tags {
            let open = format!("<{tag}>");
            if !self.src[cursor..].starts_with(&open) {
                continue;
            }
            // A close buried in a heredoc body is not a real close, so the
            // scan is heredoc-aware: only `Found` means properly terminated,
            // and `match_tool_call_block` above already consumed that case.
            let close = format!("</{tag}>");
            if matches!(
                find_close_tag(self.src, cursor + open.len(), &close),
                CloseScan::Found(_)
            ) {
                continue;
            }
            let body = &self.src[cursor + open.len()..];
            let payload = UnitPayload::UnclosedBlock {
                tag: tag.clone(),
                body: body.to_string(),
                head: call_head(body),
                reserved: false,
            };
            self.emit_block(self.src.len(), payload);
            return true;
        }
        false
    }

    /// Branch 2's extent: everything up to the next `<` that does not open a
    /// heredoc body, with a Harmony message marker closing a tool-call header
    /// treated as part of the run rather than as a structural opener.
    fn text_run_end(&self, start: usize) -> usize {
        let bytes = self.src.as_bytes();
        let mut probe = start;
        loop {
            while probe < bytes.len() && bytes[probe] != b'<' {
                probe += 1;
            }
            if probe + 1 < bytes.len() && bytes[probe] == b'<' && bytes[probe + 1] == b'<' {
                if let Some(after) = skip_heredoc_body(self.src, probe) {
                    probe = after;
                    continue;
                }
            }
            if let Some(after) = frames::message_marker_after_tool_call_header(
                &self.spec.harmony,
                self.src,
                start,
                probe,
            ) {
                probe = after;
                continue;
            }
            return probe;
        }
    }

    /// Branch 9's extent. `<name(…)>` is a unit only when `name` is a known
    /// tool — otherwise `<notes>…` would be swallowed as a call — and only
    /// when the expression parses, because its `)` is what ends the unit.
    fn angle_wrapped_call(&self, cursor: usize) -> Option<(String, serde_json::Value, usize)> {
        let bytes = self.src.as_bytes();
        if bytes.get(cursor) != Some(&b'<') {
            return None;
        }
        let name_start = cursor + 1;
        let name_len = ident_length(&bytes[name_start..])?;
        if bytes.get(name_start + name_len) != Some(&b'(') {
            return None;
        }
        let name = &self.src[name_start..name_start + name_len];
        if !self.spec.known_tools.contains(name) {
            return None;
        }
        let (arguments, consumed) =
            parse_ts_call_from(&self.src[name_start..], name.to_string()).ok()?;
        let mut end = name_start + consumed;
        while matches!(bytes.get(end), Some(b' ') | Some(b'\t')) {
            end += 1;
        }
        if bytes.get(end) == Some(&b'>') {
            end += 1;
        }
        Some((name.to_string(), arguments, end))
    }
}

/// True when `rest` opens with the truncated form of a reserved wire opener.
///
/// A reserved opener in the spec is a stub one character short of the marker
/// the model meant to write, so the well-formed marker is the stub with its
/// final character repeated. That form is rewritten to a canonical call tag
/// upstream and must not be recovered a second time here.
fn is_truncated_reserved_opener(rest: &str, opener: &str) -> bool {
    let Some(tail) = rest.strip_prefix(opener) else {
        return false;
    };
    match opener.chars().next_back() {
        Some(last) => !tail.starts_with(last),
        None => false,
    }
}

/// The tag name inside an unknown or unclosed fragment, for the violation
/// Harn writes. Empty when the fragment carries no identifier.
fn tag_fragment_name(fragment: &str) -> String {
    let inner = fragment.trim_start().trim_start_matches('<');
    let inner = inner.strip_prefix('/').unwrap_or(inner);
    match ident_length(inner.as_bytes()) {
        Some(len) => inner[..len].to_string(),
        None => String::new(),
    }
}
