//! Regex ranker for the client-executed `tool_search` fallback.
//!
//! Anthropic's `tool_search_tool_regex_20251119` exposes Python-style
//! regex. Rust's `regex` crate is a strict superset for the common
//! cases (character classes, groups, alternation, quantifiers) so we
//! compile queries directly. Case-insensitive matching by default —
//! models tend to write `/weather|forecast/` without bothering to
//! normalise capitalisation of tool names.
//!
//! Ranking: match counts across the corpus text, tie-broken by name.
//! A tool without any match is dropped.

use ::regex::RegexBuilder;

use super::{SearchOutcome, ToolCandidate};

pub(crate) fn search(
    pattern: &str,
    candidates: &[ToolCandidate],
    max_results: usize,
) -> SearchOutcome {
    if candidates.is_empty() {
        return SearchOutcome::empty("no candidate tools in the index");
    }
    let re = match RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20) // 1 MiB compiled — plenty for tool-search
        .build()
    {
        Ok(re) => re,
        Err(err) => {
            // Give the model actionable feedback so it can retry with a
            // valid pattern rather than silently returning zero hits.
            return SearchOutcome::empty(format!(
                "regex compile error: {err}. Anthropic's regex variant accepts \
                 Python-style patterns; the Rust runtime uses the `regex` crate \
                 (no backreferences, no lookaround). Use `|` for alternation."
            ));
        }
    };

    let mut scored: Vec<(usize, usize)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let text = c.corpus_text();
            let count = re.find_iter(&text).count();
            if count == 0 {
                None
            } else {
                Some((i, count))
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| candidates[a.0].name.cmp(&candidates[b.0].name))
    });
    scored.truncate(max_results);
    let tool_names: Vec<String> = scored
        .into_iter()
        .map(|(i, _)| candidates[i].name.clone())
        .collect();
    if tool_names.is_empty() {
        SearchOutcome::empty(
            "no tools matched your regex; widen the alternation or relax quantifiers",
        )
    } else {
        SearchOutcome {
            tool_names,
            diagnostic: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ToolCandidate;
    use super::*;

    fn candidate(name: &str, description: &str) -> ToolCandidate {
        ToolCandidate {
            name: name.to_string(),
            description: description.to_string(),
            param_text: Vec::new(),
        }
    }

    #[test]
    fn case_insensitive_name_match() {
        let corpus = vec![candidate("Weather", ""), candidate("cooking", "")];
        let result = search("weather", &corpus, 5);
        assert_eq!(result.tool_names, vec!["Weather"]);
    }

    #[test]
    fn alternation() {
        let corpus = vec![
            candidate("edit_file", ""),
            candidate("create_file", ""),
            candidate("run_shell", ""),
        ];
        let result = search("edit|create", &corpus, 5);
        assert!(result.tool_names.contains(&"edit_file".to_string()));
        assert!(result.tool_names.contains(&"create_file".to_string()));
        assert!(!result.tool_names.contains(&"run_shell".to_string()));
    }

    #[test]
    fn invalid_pattern_returns_diagnostic() {
        let corpus = vec![candidate("anything", "")];
        let result = search("(", &corpus, 5);
        assert!(result.tool_names.is_empty());
        assert!(result
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("regex compile error"));
    }

    #[test]
    fn ranks_by_match_count() {
        let corpus = vec![candidate("one_file", ""), candidate("file_file_file", "")];
        let result = search("file", &corpus, 5);
        assert_eq!(result.tool_names, vec!["file_file_file", "one_file"]);
    }
}
