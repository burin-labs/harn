//! Budget-respecting truncation with an ellipsis marker.
//!
//! Every helper here treats `max` as a hard ceiling: the ellipsis is charged
//! against the budget rather than appended on top of it, so the result is
//! never longer than the caller asked for. Callers that used to append the
//! marker after taking `max` units silently exceeded their own stated limit.

/// The single ellipsis marker used by every truncating surface. Keeping one
/// constant is what makes the "ellipsis counts toward the budget" arithmetic
/// below the same in all three variants.
const ELLIPSIS: char = '…';

/// Keep the head of `text`, replacing the dropped tail with an ellipsis.
///
/// The result is at most `max_chars` Unicode scalars.
pub fn truncate_end(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut chars = text.chars();
    let mut head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_none() {
        return head;
    }
    // `head` currently holds the full budget, so make room for the marker.
    head.pop();
    head.push(ELLIPSIS);
    head
}

/// Keep the tail of `text`, replacing the dropped head with an ellipsis.
///
/// The result is at most `max_chars` Unicode scalars. Use this when the tail
/// carries the signal (file names under a shared directory prefix, for
/// example) and the head is the redundant part.
pub fn truncate_start(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let mut out = String::with_capacity(max_chars);
    out.push(ELLIPSIS);
    out.extend(text.chars().skip(total - (max_chars - 1)));
    out
}

/// Keep the head of `text`, replacing the dropped tail with an ellipsis, with
/// the budget measured in UTF-8 bytes rather than characters.
///
/// The result is at most `max_bytes` bytes and always a valid `str`: the cut
/// backs off to the nearest character boundary. When the budget cannot even
/// hold the marker the result is empty, because emitting the marker would
/// break the ceiling this function exists to enforce.
pub fn truncate_end_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let Some(budget) = max_bytes.checked_sub(ELLIPSIS.len_utf8()) else {
        return String::new();
    };
    let mut end = budget;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + ELLIPSIS.len_utf8());
    out.push_str(&text[..end]);
    out.push(ELLIPSIS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_is_returned_unchanged() {
        assert_eq!(truncate_end("short", 60), "short");
        assert_eq!(truncate_start("short", 60), "short");
        assert_eq!(truncate_end_bytes("short", 60), "short");
        // Exactly at the budget is not truncation.
        assert_eq!(truncate_end("abcde", 5), "abcde");
        assert_eq!(truncate_start("abcde", 5), "abcde");
        assert_eq!(truncate_end_bytes("abcde", 5), "abcde");
    }

    #[test]
    fn truncation_keeps_the_intended_side() {
        assert_eq!(truncate_end("abcdefgh", 5), "abcd…");
        assert_eq!(truncate_start("abcdefgh", 5), "…efgh");
        assert_eq!(truncate_end_bytes("abcdefgh", 7), "abcd…");
    }

    #[test]
    fn ellipsis_is_charged_against_the_budget() {
        // The bug this module exists to kill: appending the marker after
        // taking `max` units overruns the caller's stated ceiling.
        for max in 0..12usize {
            for input in [
                "",
                "a",
                "abcdefghijklmnopqrstuvwxyz",
                "héllo wörld with wide ünicode",
                "日本語のテキストを切り詰める",
            ] {
                let end = truncate_end(input, max);
                assert!(
                    end.chars().count() <= max,
                    "truncate_end({input:?}, {max}) = {end:?} exceeds budget"
                );
                let start = truncate_start(input, max);
                assert!(
                    start.chars().count() <= max,
                    "truncate_start({input:?}, {max}) = {start:?} exceeds budget"
                );
                let bytes = truncate_end_bytes(input, max);
                assert!(
                    bytes.len() <= max,
                    "truncate_end_bytes({input:?}, {max}) = {bytes:?} exceeds budget"
                );
            }
        }
    }

    #[test]
    fn byte_budget_never_splits_a_character() {
        // "日" is three bytes; a 7-byte budget leaves 4 bytes for content,
        // which fits one character and must not slice into the second.
        assert_eq!(truncate_end_bytes("日本語", 7), "日…");
        // Too small for even the marker: empty rather than over budget.
        assert_eq!(truncate_end_bytes("日本語", 2), "");
    }

    #[test]
    fn zero_budget_yields_empty_output() {
        assert_eq!(truncate_end("anything", 0), "");
        assert_eq!(truncate_start("anything", 0), "");
        assert_eq!(truncate_end_bytes("anything", 0), "");
    }
}
