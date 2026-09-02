//! Shorten a tool description to the summary a `compact` tool is served.
//!
//! A tool entry that declares `compact: true` keeps its full description in
//! the registry and in the transcript sidecar; only the copy the model is
//! served is shortened. Every Rust renderer goes through [`tool_summary`] —
//! the native wire payload and the training-corpus projection — so a corpus
//! cannot record a description the model was never served.
//!
//! The text tool catalog is rendered in Harn, by `__agent_tool_summary` in
//! `stdlib/agent/preflight.harn`, against the same [`MIN_SUMMARY_CHARS`] floor
//! and the same whole-sentence rule. That is a second implementation of one
//! policy and a real drift seam: the two are held in parity by their tests
//! (`tests/compact_tools.rs` here, `agent_tool_compact_listing.harn` there)
//! rather than by a shared owner, because the stdlib has no builtin to call
//! into this module. Changing the rule means changing both.

/// Below this length a summary is not worth its own paragraph, so the next
/// sentence is pulled in. Without a floor the rule degenerates: a description
/// opening with "Eval-only stop cord." would serve four words and drop the
/// clause that says when to call it.
const MIN_SUMMARY_CHARS: usize = 80;

/// Whether a tool entry asks to be served a summary instead of its full
/// description. The wire key is spelled here and nowhere else, so the catalog
/// collector and the native renderer cannot drift into disagreeing about
/// which tools are compact.
pub(crate) fn entry_is_summary_only(entry: &crate::value::DictMap) -> bool {
    matches!(
        entry.get("compact"),
        Some(crate::value::VmValue::Bool(true))
    )
}

/// The leading sentences of `description`, as served to a compact tool's
/// caller. Returns the description unchanged when it is already short enough
/// or has no sentence boundary to cut on — a renderer must never serve less
/// than a whole sentence.
pub(crate) fn tool_summary(description: &str) -> String {
    let head = description
        .split("\n\n")
        .next()
        .unwrap_or(description)
        .trim();
    if head.is_empty() {
        return description.trim().to_string();
    }

    // `str::get` rather than `head[..end]` throughout: the workspace denies
    // `clippy::string_slice`, and the fallback is the honest one either way —
    // an offset that is somehow not a character boundary serves the whole
    // sentence rather than panicking or cutting a character in half.
    let mut taken = 0usize;
    for end in sentence_ends(head) {
        taken = end;
        let prefix = head.get(..end).unwrap_or(head);
        if prefix.trim().chars().count() >= MIN_SUMMARY_CHARS {
            break;
        }
    }
    if taken == 0 {
        // No sentence boundary at all: the whole leading paragraph is one
        // sentence, and half of it is worse than all of it.
        return head.to_string();
    }
    head.get(..taken).unwrap_or(head).trim().to_string()
}

/// Byte offsets just past each sentence-ending punctuation run in `text`.
///
/// A period is only a boundary when whitespace follows, which keeps dotted
/// tokens such as `.gitignore`, `v1.2`, and `tool_schema({ name })` whole.
///
/// Known limitation: an abbreviation that ends a word — `e.g.`, `etc.` — does
/// read as a boundary here. [`MIN_SUMMARY_CHARS`] carries the summary past it
/// rather than serving four words, so the cheap rule is left cheap instead of
/// growing an abbreviation table.
fn sentence_ends(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut ends = Vec::new();
    for (index, ch) in text.char_indices() {
        if !matches!(ch, '.' | '!' | '?') {
            continue;
        }
        let next = index + ch.len_utf8();
        // Absorb a run such as `?!` or a closing quote/paren so the boundary
        // lands after the punctuation, not inside it.
        let mut end = next;
        while end < bytes.len() && matches!(bytes[end], b'.' | b'!' | b'?' | b'"' | b')' | b'\'') {
            end += 1;
        }
        // `end` only ever walks forward over ASCII punctuation from a
        // character boundary, so this lookup succeeds. Treating a failure as
        // "not a boundary" keeps the fallback conservative if that changes:
        // one sentence too many beats a summary cut inside a character.
        let Some(rest) = text.get(end..) else {
            continue;
        };
        match rest.chars().next() {
            None => ends.push(end),
            Some(following) if following.is_whitespace() => ends.push(end),
            Some(_) => {}
        }
    }
    ends
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_enough_sentences_to_say_when_to_call_the_tool() {
        // The regression the floor exists for: a four-word opener alone tells
        // the model nothing about when the tool applies.
        let summary = tool_summary(
            "Eval-only stop cord. Call this instead of continuing when the eval \
             fixture, harness, or provided context is broken. Do not use it for \
             ordinary uncertainty.",
        );
        assert!(
            summary.starts_with("Eval-only stop cord. Call this"),
            "a sub-floor first sentence must pull in the next one, got {summary:?}"
        );
        assert!(
            !summary.contains("ordinary uncertainty"),
            "the floor must stop once it is satisfied, got {summary:?}"
        );
    }

    #[test]
    fn stops_at_one_sentence_when_that_sentence_carries_the_contract() {
        let description = "If a tool result confused you, an error message was \
                           unhelpful, or a tool you needed did not exist, report \
                           it here. This is telemetry-only and never changes task \
                           success.";
        assert_eq!(
            tool_summary(description),
            "If a tool result confused you, an error message was unhelpful, or a \
             tool you needed did not exist, report it here."
        );
    }

    #[test]
    fn never_serves_half_a_sentence() {
        // One long sentence with no boundary: all of it, or the model reads a
        // truncated clause as the whole contract.
        let one = "Read a slice of a file and return it with line numbers so \
                   later edits can quote exact lines without re-reading";
        assert_eq!(tool_summary(one), one);
    }

    #[test]
    fn keeps_a_dotted_token_intact_inside_the_summary() {
        let summary = tool_summary(
            "Match a pattern across the workspace, e.g. a symbol or an import \
             path, honouring .gitignore. Results are capped.",
        );
        assert!(
            summary.contains("honouring .gitignore"),
            "`e.g.` and `.gitignore` are not sentence boundaries, got {summary:?}"
        );
    }

    #[test]
    fn cuts_at_a_paragraph_break_before_any_sentence_floor() {
        let summary = tool_summary("Run a shell command.\n\nLong usage notes follow.");
        assert_eq!(summary, "Run a shell command.");
    }

    #[test]
    fn an_empty_description_stays_empty() {
        assert_eq!(tool_summary("   "), "");
    }

    #[test]
    fn multi_byte_characters_survive_the_cut() {
        // The hazard `clippy::string_slice` names: every offset here has to
        // land on a character boundary, including one past a multi-byte
        // character and one inside the sentence that gets kept.
        let summary = tool_summary(
            "Résumé a paused run — pick up where the agent stopped, naïvely. \
             Prefer it over restarting when the transcript is intact. \
             SENTINEL: this sentence must be dropped.",
        );
        assert!(
            summary.starts_with("Résumé a paused run — pick up"),
            "a multi-byte opener must survive intact, got {summary:?}"
        );
        assert!(
            !summary.contains("SENTINEL"),
            "the tail must still be dropped, got {summary:?}"
        );
        assert!(
            summary.is_char_boundary(summary.len()),
            "the summary must be valid UTF-8 through its end"
        );
    }
}
