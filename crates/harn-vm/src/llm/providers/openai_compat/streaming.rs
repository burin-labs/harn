//! Live-delta canonicalization for reserved-tool-call-token models.
//!
//! Those models are prompted with the non-special `[[CALL]]` wire delimiter
//! (see [`crate::llm::tool_delimiter`]); the streamed deltas are mapped back
//! to the canonical `<tool_call>` form here so the live display and ACP text
//! stream never show the wire spelling.

use crate::llm::api::DeltaSender;

/// Stateful canonicalizer for a streamed completion. The wire delimiter
/// (`[[CALL]]`) usually arrives split across token deltas (`[[`, `CALL`, `]]`),
/// so a per-chunk replace would miss it and leak the wire form into the
/// transcript / live display. `push` returns the text safe to emit now —
/// everything except a trailing tail that could still be the start of a
/// delimiter — and `flush` returns the remainder at end-of-stream.
#[derive(Default)]
pub(super) struct DeltaCanonicalizer {
    buf: String,
}

impl DeltaCanonicalizer {
    pub(super) fn push(&mut self, chunk: &str) -> String {
        use crate::llm::tool_delimiter::{
            wire_to_canonical, WIRE_TOOL_CALL_CLOSE, WIRE_TOOL_CALL_OPEN,
        };
        self.buf.push_str(chunk);
        let max = WIRE_TOOL_CALL_OPEN.len().max(WIRE_TOOL_CALL_CLOSE.len());
        let blen = self.buf.len();
        // The wire delimiters are pure ASCII, so any partial-delimiter tail is
        // ASCII and lands on a char boundary — byte slicing is safe here.
        let mut hold = 0;
        for k in (1..=max.min(blen)).rev() {
            let tail = &self.buf.as_bytes()[blen - k..];
            if WIRE_TOOL_CALL_OPEN.as_bytes().starts_with(tail)
                || WIRE_TOOL_CALL_CLOSE.as_bytes().starts_with(tail)
            {
                hold = k;
                break;
            }
        }
        let safe_end = blen - hold;
        if safe_end == 0 {
            return String::new();
        }
        let emit = wire_to_canonical(&self.buf[..safe_end]);
        self.buf.drain(..safe_end);
        emit
    }

    pub(super) fn flush(&mut self) -> String {
        if self.buf.is_empty() {
            return String::new();
        }
        let out = crate::llm::tool_delimiter::wire_to_canonical(&self.buf);
        self.buf.clear();
        out
    }
}

/// Wrap a delta sender so streamed chunks are mapped from the wire tool-call
/// delimiter back to canonical (across chunk boundaries) before reaching the
/// live display / ACP text stream.
pub(super) fn canonicalizing_delta_tx(orig: DeltaSender) -> DeltaSender {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut canon = DeltaCanonicalizer::default();
        while let Some(chunk) = rx.recv().await {
            let emit = canon.push(&chunk);
            if !emit.is_empty() {
                let _ = orig.send(emit);
            }
        }
        let tail = canon.flush();
        if !tail.is_empty() {
            let _ = orig.send(tail);
        }
    });
    tx
}
