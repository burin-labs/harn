//! BM25 ranker for the client-executed `tool_search` fallback.
//!
//! Implements Okapi BM25 with the conventional `k1 = 1.5`, `b = 0.75`.
//! The corpus is small (usually 10-100 tools) so we build the index
//! on-the-fly per search call — no need for a long-lived inverted
//! index.
//!
//! Tokenization is deliberately simple: lowercase + split on
//! non-alphanumeric boundaries. Splitting on snake_case / kebab-case /
//! camelCase boundaries means a query for `open file` matches a tool
//! called `open_file`, and `read` matches a tool whose description
//! says `read_bytes_from_file`.

use std::collections::HashMap;

use super::{SearchOutcome, ToolCandidate};

const K1: f64 = 1.5;
const B: f64 = 0.75;

pub(crate) fn search(
    query: &str,
    candidates: &[ToolCandidate],
    max_results: usize,
) -> SearchOutcome {
    if candidates.is_empty() {
        return SearchOutcome::empty("no candidate tools in the index");
    }
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return SearchOutcome::empty(
            "empty query after tokenization; use alphanumeric search terms",
        );
    }

    // Pre-tokenize every candidate. Cheap — tool registries are small.
    let docs: Vec<Vec<String>> = candidates
        .iter()
        .map(|c| tokenize(&c.corpus_text()))
        .collect();
    let n = docs.len() as f64;
    let avgdl: f64 = docs.iter().map(|d| d.len()).sum::<usize>() as f64 / n.max(1.0);

    // Document frequency for each query token.
    let mut df: HashMap<&str, usize> = HashMap::new();
    for qt in &query_tokens {
        let qt_ref = qt.as_str();
        if df.contains_key(qt_ref) {
            continue;
        }
        let count = docs
            .iter()
            .filter(|d| d.iter().any(|t| t == qt_ref))
            .count();
        df.insert(qt_ref, count);
    }

    let mut scored: Vec<(usize, f64)> = docs
        .iter()
        .enumerate()
        .map(|(i, doc)| {
            let dl = doc.len() as f64;
            let mut term_counts: HashMap<&str, usize> = HashMap::new();
            for t in doc {
                *term_counts.entry(t.as_str()).or_insert(0) += 1;
            }
            let mut score = 0.0f64;
            for qt in &query_tokens {
                let n_qi = *df.get(qt.as_str()).unwrap_or(&0) as f64;
                if n_qi == 0.0 {
                    continue;
                }
                let idf = ((n - n_qi + 0.5) / (n_qi + 0.5) + 1.0).ln();
                let f = *term_counts.get(qt.as_str()).unwrap_or(&0) as f64;
                if f == 0.0 {
                    continue;
                }
                let norm = 1.0 - B + B * (dl / avgdl.max(1e-9));
                score += idf * ((f * (K1 + 1.0)) / (f + K1 * norm));
            }
            (i, score)
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();

    // Stable descending sort by score, break ties by name.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| candidates[a.0].name.cmp(&candidates[b.0].name))
    });
    scored.truncate(max_results);

    let tool_names: Vec<String> = scored
        .into_iter()
        .map(|(i, _)| candidates[i].name.clone())
        .collect();
    if tool_names.is_empty() {
        SearchOutcome::empty("no tools matched your BM25 query; try shorter or more generic terms")
    } else {
        SearchOutcome {
            tool_names,
            diagnostic: None,
        }
    }
}

/// Lowercase + split on non-alphanumeric characters. Keeps tokens of
/// length 1+ (not filtering stopwords — the corpus is small and a
/// "file" hit on a one-char token is still useful signal).
fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                current.push(lower);
            }
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::ToolCandidate;
    use super::*;

    fn candidate(name: &str, description: &str, params: &[&str]) -> ToolCandidate {
        ToolCandidate {
            name: name.to_string(),
            description: description.to_string(),
            param_text: params.iter().map(|s| s.to_string()).collect(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn empty_corpus_returns_diagnostic() {
        let result = search("anything", &[], 5);
        assert!(result.tool_names.is_empty());
        assert!(result.diagnostic.is_some());
    }

    #[test]
    fn ranks_name_match_above_description_match() {
        let corpus = vec![
            candidate("weather_lookup", "", &[]),
            candidate("irrelevant", "this tool mentions weather once", &[]),
        ];
        let result = search("weather", &corpus, 5);
        assert_eq!(result.tool_names, vec!["weather_lookup", "irrelevant"]);
    }

    #[test]
    fn tokenizer_splits_snake_and_kebab_case() {
        let corpus = vec![
            candidate("open_file", "", &[]),
            candidate("read-bytes", "", &[]),
        ];
        let hit_file = search("file", &corpus, 5);
        assert!(hit_file.tool_names.contains(&"open_file".to_string()));
        let hit_bytes = search("bytes", &corpus, 5);
        assert!(hit_bytes.tool_names.contains(&"read-bytes".to_string()));
    }

    #[test]
    fn respects_max_results() {
        let corpus: Vec<ToolCandidate> = (0..10)
            .map(|i| candidate(&format!("tool_{i}"), "a generic tool", &[]))
            .collect();
        let result = search("tool", &corpus, 3);
        assert_eq!(result.tool_names.len(), 3);
    }

    #[test]
    fn zero_score_candidates_omitted() {
        let corpus = vec![candidate("weather", "", &[]), candidate("cooking", "", &[])];
        let result = search("weather", &corpus, 5);
        assert_eq!(result.tool_names, vec!["weather"]);
    }

    #[test]
    fn params_indexed_alongside_description() {
        let corpus = vec![candidate(
            "execute_sql",
            "run a database command",
            &["query: the SQL text to run"],
        )];
        let result = search("sql", &corpus, 5);
        assert!(result.tool_names.contains(&"execute_sql".to_string()));
    }
}
