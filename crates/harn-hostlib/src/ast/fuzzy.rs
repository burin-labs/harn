//! Fuzzy string matching for "did you mean?" suggestions.
//!
//! Direct port of `Sources/ASTEngine/FuzzyMatcher.swift` so the
//! `not_found` payload returned by symbol-mutation builtins surfaces the
//! same suggestion list burin-code's Swift fallback produced. Matches
//! characters in order (not necessarily contiguous): `cmpHndlr` matches
//! `completionHandler`.

/// Find the best fuzzy matches for `query` in `candidates`. Returns up to
/// `limit` candidates sorted by descending score.
pub(super) fn best_matches(query: &str, candidates: &[String], limit: usize) -> Vec<String> {
    let mut scored: Vec<(String, i32)> = Vec::new();
    for candidate in candidates {
        if let Some(score) = match_score(query, candidate) {
            scored.push((candidate.clone(), score));
        }
    }
    scored.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    scored.into_iter().take(limit).map(|(s, _)| s).collect()
}

/// Score `pattern` against `text`. Returns `None` if the pattern's
/// characters don't appear in order in `text`.
fn match_score(pattern: &str, text: &str) -> Option<i32> {
    if pattern.is_empty() {
        return Some(0);
    }
    if text.is_empty() {
        return None;
    }

    let pattern_chars: Vec<char> = pattern.to_lowercase().chars().collect();
    let text_chars: Vec<char> = text.chars().collect();
    let text_lower: Vec<char> = text.to_lowercase().chars().collect();

    if pattern_chars.len() > text_chars.len() {
        return None;
    }

    let mut pattern_idx: usize = 0;
    let mut score: i32 = 0;
    let mut last_match: Option<usize> = None;

    for (text_idx, ch) in text_lower.iter().enumerate() {
        if pattern_idx >= pattern_chars.len() {
            break;
        }
        if *ch == pattern_chars[pattern_idx] {
            score += score_for_match(text_idx, last_match, &text_chars);
            last_match = Some(text_idx);
            pattern_idx += 1;
        }
    }

    if pattern_idx != pattern_chars.len() {
        return None;
    }

    if text.to_lowercase().starts_with(&pattern.to_lowercase()) {
        score += 20;
    }

    let length_bonus = 10 - (text_chars.len() as i32 - pattern_chars.len() as i32);
    score += length_bonus.max(0);

    Some(score)
}

fn score_for_match(text_idx: usize, last_match: Option<usize>, text_chars: &[char]) -> i32 {
    let mut bonus: i32 = 0;
    if text_idx == 0 {
        bonus += 10;
    }
    if let Some(last) = last_match {
        if text_idx == last + 1 {
            bonus += 5;
        }
    }
    if text_idx > 0 {
        bonus += word_boundary_bonus(text_chars, text_idx);
    }
    if let Some(last) = last_match {
        let gap = (text_idx - last - 1) as i32;
        bonus -= gap;
    }
    bonus
}

fn word_boundary_bonus(text_chars: &[char], text_idx: usize) -> i32 {
    let prev = text_chars[text_idx - 1];
    let curr = text_chars[text_idx];
    if prev == '_' || prev == '.' || prev == '/' {
        return 8;
    }
    if curr.is_uppercase() && prev.is_lowercase() {
        return 8;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn finds_camel_case_subsequence() {
        // Pattern characters need only appear in order; doSomething has no
        // matching subsequence and must be filtered out, while the two
        // *Handler candidates both qualify.
        let candidates = names(&["completionHandler", "completeHandler", "doSomething"]);
        let hits = best_matches("cmpHndlr", &candidates, 3);
        assert_eq!(hits.len(), 2);
        assert!(hits.contains(&"completionHandler".to_string()));
        assert!(hits.contains(&"completeHandler".to_string()));
        assert!(!hits.contains(&"doSomething".to_string()));
    }

    #[test]
    fn rejects_when_pattern_chars_do_not_appear_in_order() {
        assert!(match_score("zzz", "abc").is_none());
    }

    #[test]
    fn empty_pattern_scores_zero() {
        assert_eq!(match_score("", "anything"), Some(0));
    }

    #[test]
    fn limit_caps_results() {
        let candidates = names(&["foo1", "foo2", "foo3", "foo4", "foo5"]);
        let hits = best_matches("foo", &candidates, 2);
        assert_eq!(hits.len(), 2);
    }
}
