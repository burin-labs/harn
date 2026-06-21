//! Shared, dependency-free tokenization for the default embedders.
//!
//! Both the lexical and static backends need the *same* notion of "words"
//! so a query and a corpus entry are projected into the same space. We
//! deliberately keep this tiny and identifier-aware (camelCase / snake_case
//! splitting) because the primary inputs are code-ish: symbol names, task
//! descriptions, skill/canon snippets. This mirrors
//! `SymbolRelevance.splitIdentifier` on the Swift side so the cross-platform
//! behavior is consistent.

/// Lowercase word tokens, splitting on non-alphanumerics AND on
/// camelCase / snake_case / kebab-case identifier boundaries.
///
/// `"getUserByID get_user_by_id"` -> `["get","user","by","id","get","user","by","id"]`.
/// Single-character tokens are kept (e.g. the `i` in a loop) because they
/// can still carry char-ngram signal, but pure punctuation is dropped.
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

/// Character n-grams over the *normalized* (lowercased, whitespace-collapsed)
/// text, padded with a boundary marker so prefixes/suffixes are captured.
///
/// Char-ngrams give robustness to typos, plurals, and shared roots
/// (`subscription` ~ `subscriptions` ~ `subscribe`) that pure word tokens
/// miss — the lexical backend mixes both signals.
pub fn char_ngrams(text: &str, n: usize) -> Vec<String> {
    let normalized: String = {
        let mut s = String::with_capacity(text.len() + 2);
        s.push(' ');
        let mut last_space = true;
        for ch in text.chars() {
            if ch.is_whitespace() {
                if !last_space {
                    s.push(' ');
                    last_space = true;
                }
            } else {
                for lc in ch.to_lowercase() {
                    s.push(lc);
                }
                last_space = false;
            }
        }
        if !last_space {
            s.push(' ');
        }
        s
    };
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() < n || n == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(chars.len().saturating_sub(n) + 1);
    for window in chars.windows(n) {
        out.push(window.iter().collect());
    }
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

    #[test]
    fn char_ngrams_capture_boundaries() {
        let grams = char_ngrams("cat", 3);
        // " ca", "cat", "at " over " cat "
        assert!(grams.contains(&" ca".to_string()));
        assert!(grams.contains(&"cat".to_string()));
        assert!(grams.contains(&"at ".to_string()));
    }

    #[test]
    fn char_ngrams_short_input_empty() {
        assert!(char_ngrams("a", 4).is_empty());
    }
}
