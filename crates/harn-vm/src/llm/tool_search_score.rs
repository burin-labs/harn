//! Pure ranking primitive for Harn-managed client-mode tool search.
//!
//! The agent loop owns when the synthetic search tool is shown, when events
//! are emitted, and which tools get promoted. Rust only ranks a static tool
//! registry for a query so the scoring math stays shared and testable.

use std::collections::HashMap;

use regex::RegexBuilder;

const K1: f64 = 1.5;
const B: f64 = 0.75;
const DEFAULT_MAX_RESULTS: usize = 20;

#[derive(Clone, Debug)]
struct Candidate {
    name: String,
    description: String,
    param_text: Vec<String>,
}

impl Candidate {
    fn corpus_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(&self.description);
        for param in &self.param_text {
            out.push(' ');
            out.push_str(param);
        }
        out
    }

    fn snippet(&self) -> String {
        if !self.description.trim().is_empty() {
            return self.description.trim().to_string();
        }
        self.param_text.first().cloned().unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RankedTool {
    pub(crate) tool_name: String,
    pub(crate) score: f64,
    pub(crate) snippet: String,
}

pub(crate) fn score_tools(
    query: &str,
    registry: &serde_json::Value,
    opts: &serde_json::Value,
) -> Vec<RankedTool> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let max_results = opts
        .get("max_results")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(DEFAULT_MAX_RESULTS as u64)
        .max(1) as usize;
    let strategy = opts
        .get("strategy")
        .and_then(serde_json::Value::as_str)
        .or_else(|| opts.get("variant").and_then(serde_json::Value::as_str))
        .unwrap_or("bm25");
    let candidates = candidates_from_registry(registry);
    match strategy {
        "regex" => score_regex(query, &candidates, max_results),
        "hybrid" => score_hybrid(query, &candidates, max_results),
        _ => score_bm25(query, &candidates, max_results),
    }
}

fn candidates_from_registry(registry: &serde_json::Value) -> Vec<Candidate> {
    let tools = registry
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .or_else(|| registry.as_array());
    tools
        .into_iter()
        .flatten()
        .filter_map(candidate_from_entry)
        .collect()
}

fn candidate_from_entry(tool: &serde_json::Value) -> Option<Candidate> {
    if let Some(function) = tool.get("function") {
        let name = function.get("name")?.as_str()?.to_string();
        let description = function
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let param_text = extract_param_text(function.get("parameters"));
        return Some(Candidate {
            name,
            description,
            param_text,
        });
    }

    let name = tool.get("name")?.as_str()?.to_string();
    let description = tool
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let param_text =
        extract_param_text(tool.get("input_schema").or_else(|| tool.get("parameters")));
    Some(Candidate {
        name,
        description,
        param_text,
    })
}

fn extract_param_text(schema: Option<&serde_json::Value>) -> Vec<String> {
    let Some(properties) = schema
        .and_then(|schema| schema.get("properties").or(Some(schema)))
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, prop)| {
            let description = prop
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if description.is_empty() {
                name.clone()
            } else {
                format!("{name}: {description}")
            }
        })
        .collect()
}

fn score_bm25(query: &str, candidates: &[Candidate], max_results: usize) -> Vec<RankedTool> {
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() || candidates.is_empty() {
        return Vec::new();
    }
    let docs: Vec<Vec<String>> = candidates
        .iter()
        .map(|candidate| tokenize(&candidate.corpus_text()))
        .collect();
    let n = docs.len() as f64;
    let avgdl = docs.iter().map(Vec::len).sum::<usize>() as f64 / n.max(1.0);
    let mut df: HashMap<&str, usize> = HashMap::new();
    for token in &query_tokens {
        let token_ref = token.as_str();
        if df.contains_key(token_ref) {
            continue;
        }
        let count = docs
            .iter()
            .filter(|doc| doc.iter().any(|item| item == token_ref))
            .count();
        df.insert(token_ref, count);
    }

    let mut scored: Vec<(usize, f64)> = docs
        .iter()
        .enumerate()
        .map(|(index, doc)| {
            let dl = doc.len() as f64;
            let mut term_counts: HashMap<&str, usize> = HashMap::new();
            for token in doc {
                *term_counts.entry(token.as_str()).or_insert(0) += 1;
            }
            let mut score = 0.0;
            for query_token in &query_tokens {
                let n_qi = *df.get(query_token.as_str()).unwrap_or(&0) as f64;
                if n_qi == 0.0 {
                    continue;
                }
                let f = *term_counts.get(query_token.as_str()).unwrap_or(&0) as f64;
                if f == 0.0 {
                    continue;
                }
                let idf = ((n - n_qi + 0.5) / (n_qi + 0.5)).ln_1p();
                let norm = B.mul_add(dl / avgdl.max(1e-9), 1.0 - B);
                score += idf * ((f * (K1 + 1.0)) / K1.mul_add(norm, f));
            }
            (index, score)
        })
        .filter(|(_, score)| *score > 0.0)
        .collect();

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| candidates[a.0].name.cmp(&candidates[b.0].name))
    });
    scored.truncate(max_results);
    scored
        .into_iter()
        .map(|(index, score)| ranked(&candidates[index], score))
        .collect()
}

fn score_regex(pattern: &str, candidates: &[Candidate], max_results: usize) -> Vec<RankedTool> {
    let Ok(regex) = RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
    else {
        return Vec::new();
    };
    let mut scored: Vec<(usize, f64)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let count = regex.find_iter(&candidate.corpus_text()).count();
            (count > 0).then_some((index, count as f64))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| candidates[a.0].name.cmp(&candidates[b.0].name))
    });
    scored.truncate(max_results);
    scored
        .into_iter()
        .map(|(index, score)| ranked(&candidates[index], score))
        .collect()
}

fn score_hybrid(query: &str, candidates: &[Candidate], max_results: usize) -> Vec<RankedTool> {
    // Pull 4x the requested fan-out from each ranker so the fusion has
    // room to reorder beyond the top-N from any single list.
    let pool = max_results.saturating_mul(4);
    let field = score_field_weighted(query, candidates, pool);
    // Field-weighted ranking is included twice to bias the fused order
    // toward exact-name/path matches over BM25's bag-of-tokens score.
    let lists = vec![score_bm25(query, candidates, pool), field.clone(), field];
    reciprocal_rank_fuse(lists, candidates, max_results)
}

fn score_field_weighted(
    query: &str,
    candidates: &[Candidate],
    max_results: usize,
) -> Vec<RankedTool> {
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return Vec::new();
    }
    let mut scored = candidates
        .iter()
        .filter_map(|candidate| {
            let name = candidate.name.to_lowercase();
            let name_parts: Vec<&str> = name.split(['_', '-', '.', ':']).collect();
            let description = candidate.description.to_lowercase();
            let params = candidate.param_text.join(" ").to_lowercase();
            let mut score = 0.0;
            for token in &query_tokens {
                if name == *token {
                    score += 10.0;
                } else if name_parts.iter().any(|part| *part == token) {
                    score += 7.0;
                } else if name.contains(token) {
                    score += 5.0;
                }
                if description.contains(token) {
                    score += 2.0;
                }
                if params.contains(token) {
                    score += 1.0;
                }
            }
            if query_tokens
                .iter()
                .all(|token| name_parts.iter().any(|part| *part == token) || name.contains(token))
            {
                score += 1.0 / name_parts.len().max(1) as f64;
            }
            (score > 0.0).then(|| ranked(candidate, score))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.tool_name.cmp(&b.tool_name))
    });
    scored.truncate(max_results);
    scored
}

fn reciprocal_rank_fuse(
    lists: Vec<Vec<RankedTool>>,
    candidates: &[Candidate],
    max_results: usize,
) -> Vec<RankedTool> {
    const RRF_K: f64 = 60.0;
    let mut scores: HashMap<String, f64> = HashMap::new();
    for list in lists {
        for (rank, item) in list.into_iter().enumerate() {
            *scores.entry(item.tool_name).or_insert(0.0) += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    let by_name = candidates
        .iter()
        .map(|candidate| (candidate.name.as_str(), candidate))
        .collect::<HashMap<_, _>>();
    let mut fused = scores
        .into_iter()
        .filter_map(|(name, score)| {
            by_name
                .get(name.as_str())
                .map(|candidate| ranked(candidate, score))
        })
        .collect::<Vec<_>>();
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.tool_name.cmp(&b.tool_name))
    });
    fused.truncate(max_results);
    fused
}

fn ranked(candidate: &Candidate, score: f64) -> RankedTool {
    RankedTool {
        tool_name: candidate.name.clone(),
        score,
        snippet: candidate.snippet(),
    }
}

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
    use super::*;

    #[test]
    fn bm25_ranks_deferred_registry_entries() {
        let registry = serde_json::json!({
            "tools": [
                {"name": "deploy_service", "description": "Deploy production", "parameters": {}},
                {"name": "query_metrics", "description": "Read dashboards", "parameters": {}}
            ]
        });
        let ranked = score_tools(
            "deploy",
            &registry,
            &serde_json::json!({"strategy": "bm25"}),
        );
        assert_eq!(ranked[0].tool_name, "deploy_service");
    }

    #[test]
    fn regex_uses_same_candidate_corpus() {
        let registry = serde_json::json!({
            "tools": [
                {"name": "edit_file", "description": "Edit file", "parameters": {}},
                {"name": "run_shell", "description": "Run command", "parameters": {}}
            ]
        });
        let ranked = score_tools(
            "edit|create",
            &registry,
            &serde_json::json!({"strategy": "regex"}),
        );
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].tool_name, "edit_file");
    }

    #[test]
    fn hybrid_boosts_exact_name_matches() {
        let registry = serde_json::json!({
            "tools": [
                {"name": "run_command", "description": "Execute a shell process", "parameters": {}},
                {"name": "read_command_output", "description": "Read process output", "parameters": {}}
            ]
        });
        let ranked = score_tools(
            "command",
            &registry,
            &serde_json::json!({"strategy": "hybrid"}),
        );
        assert_eq!(ranked[0].tool_name, "run_command");
    }
}
