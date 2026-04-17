//! Client-executed tool-search fallback (harn#70).
//!
//! When a user requests `tool_search` against a provider that has no
//! native defer-loading / tool-search support, the agent loop injects a
//! synthetic tool (default name: `__harn_tool_search`) whose handler
//! runs the configured strategy in-process and returns a list of
//! matching tool names. The loop then promotes those deferred tools
//! into the eager set for the *next* turn.
//!
//! Four strategies:
//!   - **`bm25`**  — in-tree BM25 over `name + description + param
//!     names/descriptions`. Default.
//!   - **`regex`** — case-insensitive regex over the same corpus.
//!   - **`semantic`** — delegated to the host via the `tool_search/query`
//!     bridge RPC (lets integrators wire embeddings without Harn
//!     depending on ML crates).
//!   - **`host`** — pure host-side implementation; the VM just
//!     round-trips the query to the host and returns whatever names it
//!     suggests.
//!
//! The two in-tree strategies (`bm25`, `regex`) share a `ToolCandidate`
//! corpus built once per turn and are cheap enough to rebuild from the
//! tool registry whenever `opts.native_tools` mutates.

use std::collections::BTreeMap;

pub(crate) mod bm25;
pub(crate) mod regex;

/// One searchable tool in the client-mode index. Built from either the
/// VM-side tool registry (`VmValue::Dict`) or the provider-native JSON
/// tool array — whichever the caller's `llm_call` options settled on.
#[derive(Clone, Debug)]
pub(crate) struct ToolCandidate {
    pub name: String,
    pub description: String,
    /// Flattened `"name: description"` strings for every top-level
    /// parameter of the tool. BM25/regex index this alongside the
    /// description so "file" can match a `path: string  // file path`
    /// parameter on a tool whose top-level description only mentions
    /// "edit".
    pub param_text: Vec<String>,
}

impl ToolCandidate {
    /// Build the BM25 token bag for a single candidate: the concatenation
    /// of every searchable field, normalized to lowercase, split on
    /// non-alphanumeric boundaries. Kept as a method so regex strategy
    /// and BM25 share the same corpus for determinism across strategies.
    pub(crate) fn corpus_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(&self.description);
        for p in &self.param_text {
            out.push(' ');
            out.push_str(p);
        }
        out
    }
}

/// Build `ToolCandidate`s from a provider-native JSON tools array.
/// Accepts both Anthropic shape (`{name, description, input_schema}`)
/// and OpenAI shape (`{type:"function", function:{name,description,parameters}}`).
/// Tools missing a `name` are dropped silently — they'd be unreachable
/// anyway.
pub(crate) fn candidates_from_native(native_tools: &[serde_json::Value]) -> Vec<ToolCandidate> {
    native_tools
        .iter()
        .filter_map(candidate_from_native_entry)
        .collect()
}

fn candidate_from_native_entry(tool: &serde_json::Value) -> Option<ToolCandidate> {
    // Anthropic shape.
    let (name, description, input_schema) = if tool.get("type").is_none() {
        let name = tool.get("name")?.as_str()?.to_string();
        let description = tool
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let input_schema = tool.get("input_schema").cloned();
        (name, description, input_schema)
    } else {
        // OpenAI-compat shape: {type:"function", function:{...}}.
        let function = tool.get("function")?;
        let name = function.get("name")?.as_str()?.to_string();
        let description = function
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let input_schema = function.get("parameters").cloned();
        (name, description, input_schema)
    };

    let param_text = extract_param_text(input_schema.as_ref());
    Some(ToolCandidate {
        name,
        description,
        param_text,
    })
}

fn extract_param_text(schema: Option<&serde_json::Value>) -> Vec<String> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, prop) in properties {
        let description = prop
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if description.is_empty() {
            out.push(name.clone());
        } else {
            out.push(format!("{name}: {description}"));
        }
    }
    out
}

/// Minimum of `query` length and a configured floor. Guards against a
/// pathological "" query matching every tool under BM25.
pub(crate) const MIN_QUERY_CHARS: usize = 1;

/// Hard cap on how many tools a single search call can return. Provider
/// context windows aren't infinite; even with generous `budget_tokens`,
/// we never want a single search to promote hundreds of tools.
pub(crate) const DEFAULT_MAX_RESULTS: usize = 20;

/// Outcome of a single search query. Carries the strategy used so the
/// agent loop can emit an accurate `tool_search_result` transcript event
/// and replay.rs can reconstruct the decision.
#[derive(Clone, Debug)]
pub(crate) struct SearchOutcome {
    /// The tool names to promote, in rank order (highest score first).
    pub tool_names: Vec<String>,
    /// Optional diagnostic string: "no match", "regex compile error:
    /// ...", etc. Shown to the model as part of the tool_result so it
    /// can self-correct (retry with a broader query).
    pub diagnostic: Option<String>,
}

impl SearchOutcome {
    pub(crate) fn into_tool_result(self) -> serde_json::Value {
        let mut obj = BTreeMap::new();
        obj.insert(
            "tool_names".to_string(),
            serde_json::Value::Array(
                self.tool_names
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
        if let Some(diag) = self.diagnostic {
            obj.insert("diagnostic".to_string(), serde_json::Value::String(diag));
        }
        serde_json::Value::Object(obj.into_iter().collect())
    }

    pub(crate) fn empty(diagnostic: impl Into<String>) -> Self {
        Self {
            tool_names: Vec::new(),
            diagnostic: Some(diagnostic.into()),
        }
    }
}

/// Run the configured in-tree strategy. Host / semantic strategies are
/// handled by the agent loop directly (they require async + bridge
/// access) — this function only covers strategies that live in the VM.
pub(crate) fn run_in_tree(
    strategy: InTreeStrategy,
    query: &str,
    candidates: &[ToolCandidate],
    max_results: usize,
) -> SearchOutcome {
    let query_trimmed = query.trim();
    if query_trimmed.chars().count() < MIN_QUERY_CHARS {
        return SearchOutcome::empty("empty query; specify search terms or a regex pattern");
    }
    match strategy {
        InTreeStrategy::Bm25 => bm25::search(query_trimmed, candidates, max_results),
        InTreeStrategy::Regex => regex::search(query_trimmed, candidates, max_results),
    }
}

/// Strategies that run synchronously in the VM. `Semantic` and `Host`
/// are not members because they must bounce through the host bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InTreeStrategy {
    Bm25,
    Regex,
}

#[cfg(test)]
mod tests;
