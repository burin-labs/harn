//! Identifier case conversion.
//!
//! The model is deliberately two-phase — split an identifier into lowercase
//! words, then render those words in the target case — so the nine `strings`
//! builtins and the in-tree Rust callers (lint fixits, codegen discriminators,
//! import-path guessing) all agree on where a word boundary is. In particular
//! the splitter treats runs of capitals as a single acronym word, so
//! `HTTPServer` snake-cases to `http_server` rather than `h_t_t_p_server`.
//!
//! ## Why not `heck`?
//!
//! `heck` is the standard crate for this and would replace most of the code
//! below — but it is deliberately not used here. The nine `snake_to_camel` /
//! `pascal_to_snake` / … builtins are Harn *language surface*: their output is
//! pinned by conformance fixtures, so the exact word-boundary rules
//! (acronym-as-one-word above, digit handling in `split_camel`, empty-segment
//! dropping) are part of the language contract, not an implementation detail.
//! Delegating to `heck` would place those semantics behind a third-party
//! dependency where a routine semver bump could silently shift a language-level
//! result — the same reason the duration grammar and other spec-pinned surfaces
//! stay in-tree. This is a pinning decision, not NIH: the moment these rules
//! were shared across the builtins, lint fixits, and codegen, one owner whose
//! behavior only changes when *we* change it became the point. (`heck` was not
//! benchmarked for edge-case parity here; the argument is ownership of pinned
//! semantics, so a match today would not change the decision.)

/// Split a snake_case identifier into its words. Empty segments are dropped,
/// so leading/trailing/doubled underscores do not produce empty words.
pub fn split_snake(s: &str) -> Vec<&str> {
    s.split('_').filter(|p| !p.is_empty()).collect()
}

/// Split a kebab-case identifier into its words.
pub fn split_kebab(s: &str) -> Vec<&str> {
    s.split('-').filter(|p| !p.is_empty()).collect()
}

/// Splits a camelCase or PascalCase string into lowercase words.
/// `"HTTPServer"` → `["http", "server"]`.
/// `"testFilePatterns"` → `["test", "file", "patterns"]`.
pub fn split_camel(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut words = Vec::new();
    let mut cur = String::new();
    for i in 0..chars.len() {
        let c = chars[i];
        if i > 0 && c.is_uppercase() {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            let prev_lower_or_digit = prev.is_lowercase() || prev.is_ascii_digit();
            let acronym_end = prev.is_uppercase() && next.is_some_and(|n| n.is_lowercase());
            if (prev_lower_or_digit || acronym_end) && !cur.is_empty() {
                words.push(cur.clone());
                cur.clear();
            }
        }
        for lc in c.to_lowercase() {
            cur.push(lc);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Split an identifier written in any of the supported cases. Underscores and
/// hyphens are word separators; within each separated run, capital-letter
/// boundaries split further. Use this when the input case is not known up
/// front (a user-authored identifier being offered a rename, say).
pub fn split_identifier(s: &str) -> Vec<String> {
    s.split(['_', '-'])
        .filter(|part| !part.is_empty())
        .flat_map(split_camel)
        .collect()
}

/// Uppercase the first character, leaving the rest untouched.
pub fn uppercase_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Lowercase the first character, leaving the rest untouched.
pub fn lowercase_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Render words as `camelCase`.
pub fn words_to_camel<S: AsRef<str>>(words: &[S]) -> String {
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        let lower = w.as_ref().to_lowercase();
        if i == 0 {
            out.push_str(&lower);
        } else {
            out.push_str(&uppercase_first(&lower));
        }
    }
    out
}

/// Render words as `PascalCase`.
pub fn words_to_pascal<S: AsRef<str>>(words: &[S]) -> String {
    words
        .iter()
        .map(|w| uppercase_first(&w.as_ref().to_lowercase()))
        .collect()
}

/// Render words as `snake_case`.
pub fn words_to_snake<S: AsRef<str>>(words: &[S]) -> String {
    join_lowercase(words, '_')
}

/// Render words as `kebab-case`.
pub fn words_to_kebab<S: AsRef<str>>(words: &[S]) -> String {
    join_lowercase(words, '-')
}

fn join_lowercase<S: AsRef<str>>(words: &[S], separator: char) -> String {
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push(separator);
        }
        out.push_str(&w.as_ref().to_lowercase());
    }
    out
}

/// Convert a camelCase or PascalCase identifier to `snake_case`.
pub fn to_snake_case(s: &str) -> String {
    words_to_snake(&split_identifier(s))
}

/// Convert an identifier written in any supported case to `PascalCase`.
pub fn to_pascal_case(s: &str) -> String {
    words_to_pascal(&split_identifier(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acronyms_stay_one_word() {
        assert_eq!(to_snake_case("HTTPServer"), "http_server");
        assert_eq!(to_snake_case("PRReview"), "pr_review");
        assert_eq!(to_pascal_case("http_server"), "HttpServer");
    }

    #[test]
    fn conversion_is_idempotent_on_already_converted_input() {
        assert_eq!(to_snake_case("foo_bar"), "foo_bar");
        assert_eq!(to_pascal_case("FooBar"), "FooBar");
    }

    #[test]
    fn split_identifier_accepts_mixed_separators() {
        assert_eq!(
            split_identifier("some-mixedCase_name"),
            vec!["some", "mixed", "case", "name"]
        );
        // Empty segments from doubled or edge separators are dropped.
        assert_eq!(split_identifier("__leading"), vec!["leading"]);
        assert!(split_identifier("").is_empty());
    }

    #[test]
    fn digits_start_a_new_word_only_at_a_capital() {
        assert_eq!(to_snake_case("v2Endpoint"), "v2_endpoint");
    }
}
