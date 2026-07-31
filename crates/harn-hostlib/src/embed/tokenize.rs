//! Shared, dependency-free tokenization for the default embedders.
//!
//! The static backend uses the same notion of "words" as the canonical
//! lexical floor in `harn-session-store`, so a query and a corpus entry are
//! projected into the same space. We
//! deliberately keep this tiny and identifier-aware (camelCase / snake_case
//! splitting) because the primary inputs are code-ish: symbol names, task
//! descriptions, skill/canon snippets. This mirrors
//! `SymbolRelevance.splitIdentifier` on the Swift side so the cross-platform
//! behavior is consistent.

/// Lowercase word tokens, splitting on non-alphanumerics AND on
/// camelCase / snake_case / kebab-case identifier boundaries.
///
/// `"getUserByID get_user_by_id"` -> `["get","user","by","id","get","user","by","id"]`.
/// Single-character tokens are kept (e.g. the `i` in a loop), but pure
/// punctuation is dropped.
pub fn word_tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;

    let flush = |buf: &mut String, out: &mut Vec<String>| {
        if !buf.is_empty() {
            out.push(std::mem::take(buf));
        }
    };

    for ch in text.chars() {
        if ch.is_alphanumeric() {
            // camelCase boundary: lower/digit -> Upper starts a new word.
            if ch.is_uppercase() && prev_lower {
                flush(&mut current, &mut out);
            }
            for lc in ch.to_lowercase() {
                current.push(lc);
            }
            prev_lower = ch.is_lowercase() || ch.is_numeric();
        } else {
            // separator (`_`, `-`, space, punctuation, ...)
            flush(&mut current, &mut out);
            prev_lower = false;
        }
    }
    flush(&mut current, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_camel_and_snake() {
        assert_eq!(
            word_tokens("getUserByID get_user_by_id"),
            vec!["get", "user", "by", "id", "get", "user", "by", "id"]
        );
    }

    #[test]
    fn drops_punctuation_keeps_words() {
        assert_eq!(
            word_tokens("rate-limit middleware (retry)"),
            vec!["rate", "limit", "middleware", "retry"]
        );
    }
}
