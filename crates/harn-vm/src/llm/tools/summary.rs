//! Shorten a tool description to the summary a `compact` tool is served.
//!
//! A tool entry that declares `compact: true` keeps its full description in
//! the registry and in the transcript sidecar; only the copy the model is
//! served is shortened. Every Rust renderer goes through [`tool_summary`] —
//! the native wire payload and the training-corpus projection — so a corpus
//! cannot record a description the model was never served.
//!
//! Harn's text catalog calls this same owner through `tool_description_summary`.

use crate::value::{VmError, VmValue};
use harn_builtin_macros::harn_builtin;

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "tool_description_summary(description: string) -> string",
    category = "tools",
    doc = "Return the compact description served by native and text tool catalogs."
)]
fn tool_description_summary_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let description = crate::value::args::Args::thrown("tool_description_summary", args)
        .string(0, "description")?;
    Ok(VmValue::String(tool_summary(description).into()))
}

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

/// Abbreviations whose trailing period never ends a sentence, lowercase and
/// without the final period.
const ABBREVIATIONS: &[&str] = &["e.g", "i.e", "etc", "vs"];

/// Whether the period at `index` closes one of [`ABBREVIATIONS`]. The token is
/// the run of letters and periods directly before it, so `(e.g` and `, i.e`
/// match while `file.rs` or `Done` do not.
fn closes_abbreviation(text: &str, index: usize) -> bool {
    let before = text.get(..index).unwrap_or("");
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, ch)| ch.is_alphabetic() || *ch == '.')
        .last()
        .map_or(index, |(offset, _)| offset);
    let token = before.get(start..).unwrap_or("").to_lowercase();
    ABBREVIATIONS.contains(&token.as_str())
}

/// Byte offsets just past each sentence-ending punctuation run in `text`.
///
/// A period is only a boundary when whitespace follows, which keeps dotted
/// tokens such as `.gitignore`, `v1.2`, and `tool_schema({ name })` whole.
/// Nothing inside parentheses or backticks is a boundary, and neither is the
/// period of `e.g.`, `i.e.`, `etc.`, or `vs.`: a summary cut there would end
/// mid-clause, for example at `(e.g.`.
fn sentence_ends(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut ends = Vec::new();
    let mut paren_depth = 0usize;
    // Length of the backtick run that opened the current code span. A span
    // closes only on a run of the same length, so ``a ` b`` stays one span.
    let mut code_fence: Option<usize> = None;
    let mut run_end = 0usize;
    for (index, ch) in text.char_indices() {
        if index < run_end {
            continue;
        }
        let in_code = code_fence.is_some();
        match ch {
            '`' => {
                let run = text
                    .get(index..)
                    .unwrap_or("")
                    .chars()
                    .take_while(|c| *c == '`')
                    .count();
                run_end = index + run;
                match code_fence {
                    None => code_fence = Some(run),
                    Some(open) if open == run => code_fence = None,
                    Some(_) => {}
                }
                continue;
            }
            '(' if !in_code => paren_depth += 1,
            ')' if !in_code => paren_depth = paren_depth.saturating_sub(1),
            _ => {}
        }
        if !matches!(ch, '.' | '!' | '?') || in_code || paren_depth > 0 {
            continue;
        }
        if ch == '.' && closes_abbreviation(text, index) {
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

    #[test]
    fn an_abbreviation_inside_parentheses_is_not_a_boundary() {
        // The shipped failure: a summary that ended at `(e.g.` once the floor
        // was met inside the parenthetical.
        let description = "Scan the workspace for files that match the request and report \
                           their layout (e.g. languages, build files, and test roots) before \
                           you edit. Results are capped.";
        assert_eq!(
            tool_summary(description),
            "Scan the workspace for files that match the request and report their layout \
             (e.g. languages, build files, and test roots) before you edit."
        );
    }

    #[test]
    fn abbreviations_and_backticked_text_never_end_a_summary() {
        for abbreviation in ["e.g.", "i.e.", "etc.", "vs.", "E.G."] {
            let description = format!(
                "Search the workspace for a symbol across many languages, {abbreviation} \
                 the closest definition. Extra detail follows."
            );
            let summary = tool_summary(&description);
            assert!(
                summary.ends_with("the closest definition."),
                "{abbreviation} must not end the summary, got {summary:?}"
            );
        }
        let summary = tool_summary(
            "Return every line of the requested range together with its number, or fall back \
             to `Range. Full` when no range is given. Done.",
        );
        assert!(
            summary.ends_with("no range is given."),
            "a period inside backticks is not a boundary, got {summary:?}"
        );
    }

    #[test]
    fn a_trailing_period_after_an_ordinary_word_still_ends_the_sentence() {
        let summary = tool_summary(
            "Run the configured verifier against the changed files and report the structured \
             result back to the caller. Use it after edits.",
        );
        assert_eq!(
            summary,
            "Run the configured verifier against the changed files and report the structured \
             result back to the caller."
        );
    }

    #[test]
    fn a_multi_backtick_code_span_closes_only_on_a_matching_run() {
        let summary = tool_summary(
            "Return every line of the requested range together with its number, or fall back \
             to ``Range. ` Full`` when no range is given. Done.",
        );
        assert!(
            summary.ends_with("no range is given."),
            "a lone backtick inside a double-backtick span must not close it, got {summary:?}"
        );
    }
}
