//! `code_index.repo_map` — personalized structural symbol map.
//!
//! This is a read-only query over the typed symbol graph. It keeps the
//! graph-ranking policy in hostlib so hosts can inject the resulting map
//! without reimplementing PageRank or graph traversal in product glue.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use harn_vm::VmValue;

use super::builtins::SharedIndex;
use super::file_table::FileId;
use super::state::IndexState;
use super::symbol_graph::{Node, NodeId, NodeKind, SymbolGraph};
use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_int, optional_string, optional_string_list, str_value,
};

pub(super) const BUILTIN: &str = "hostlib_code_index_repo_map";

const DEFAULT_MAX_ENTRIES: usize = 12;
const DEFAULT_TOKEN_BUDGET: usize = 800;
const TASK_BOOST: f64 = 10.0;
const CONTEXT_FILE_BOOST: f64 = 50.0;
const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 20;

pub(super) fn run(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();
    let task = optional_string(BUILTIN, dict, "task")?.unwrap_or_default();
    let context_files = optional_string_list(BUILTIN, dict, "context_files")?;
    let max_entries = positive_usize_arg(
        "max_entries",
        optional_int(BUILTIN, dict, "max_entries", DEFAULT_MAX_ENTRIES as i64)?,
    )?;
    let token_budget = non_negative_usize_arg(
        "token_budget",
        optional_int(BUILTIN, dict, "token_budget", DEFAULT_TOKEN_BUDGET as i64)?,
    )?;

    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Ok(empty_response(None));
    };

    let graph = state.overlays.graph(&state.symbols);
    let context_file_ids = context_file_ids(state, &context_files);
    let task_terms = task_terms(&task);
    let ranks = personalized_pagerank(graph, &task_terms, &context_file_ids);
    let mut entries = ranked_definition_entries(graph, &ranks, &task_terms, &context_file_ids);
    let truncated_by_entries = entries.len() > max_entries;
    if truncated_by_entries {
        entries.truncate(max_entries);
    }
    let (rendered, truncated_by_budget) = render_entries(&entries, token_budget);

    Ok(build_dict([
        ("rendered", str_value(rendered)),
        (
            "entries",
            VmValue::List(Arc::new(entries.iter().map(entry_to_vm).collect())),
        ),
        (
            "truncated",
            VmValue::Bool(truncated_by_entries || truncated_by_budget),
        ),
        (
            "overlay",
            state
                .overlays
                .active()
                .map(str_value)
                .unwrap_or(VmValue::Nil),
        ),
    ]))
}

fn empty_response(overlay: Option<&str>) -> VmValue {
    build_dict([
        ("rendered", str_value("")),
        ("entries", VmValue::List(Arc::new(Vec::new()))),
        ("truncated", VmValue::Bool(false)),
        ("overlay", overlay.map(str_value).unwrap_or(VmValue::Nil)),
    ])
}

fn positive_usize_arg(param: &'static str, value: i64) -> Result<usize, HostlibError> {
    if value < 1 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param,
            message: format!("must be >= 1, got {value}"),
        });
    }
    usize::try_from(value).map_err(|_| HostlibError::InvalidParameter {
        builtin: BUILTIN,
        param,
        message: "does not fit in usize".to_string(),
    })
}

fn non_negative_usize_arg(param: &'static str, value: i64) -> Result<usize, HostlibError> {
    if value < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param,
            message: format!("must be >= 0, got {value}"),
        });
    }
    usize::try_from(value).map_err(|_| HostlibError::InvalidParameter {
        builtin: BUILTIN,
        param,
        message: "does not fit in usize".to_string(),
    })
}

fn context_file_ids(state: &IndexState, raw_paths: &[String]) -> HashSet<FileId> {
    raw_paths
        .iter()
        .filter_map(|path| state.lookup_path(path))
        .collect()
}

fn task_terms(task: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut current = String::new();
    for ch in task.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else {
            absorb_task_term(&mut out, &mut current);
        }
    }
    absorb_task_term(&mut out, &mut current);
    out
}

fn absorb_task_term(out: &mut HashSet<String>, current: &mut String) {
    if current.len() >= 3 {
        out.insert(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn personalized_pagerank(
    graph: &SymbolGraph,
    task_terms: &HashSet<String>,
    context_file_ids: &HashSet<FileId>,
) -> HashMap<NodeId, f64> {
    let ids = graph.all_node_ids();
    if ids.is_empty() {
        return HashMap::new();
    }
    let index_by_id: HashMap<NodeId, usize> = ids
        .iter()
        .copied()
        .enumerate()
        .map(|(idx, id)| (id, idx))
        .collect();
    let adjacency: Vec<Vec<usize>> = ids
        .iter()
        .map(|id| {
            graph
                .outgoing(*id)
                .iter()
                .filter_map(|edge| index_by_id.get(&edge.to).copied())
                .collect()
        })
        .collect();
    let personalization = normalized_personalization(graph, &ids, task_terms, context_file_ids);
    let mut ranks = personalization.clone();
    for _ in 0..ITERATIONS {
        let mut next: Vec<f64> = personalization
            .iter()
            .map(|weight| (1.0 - DAMPING) * weight)
            .collect();
        let mut dangling = 0.0;
        for idx in 0..ids.len() {
            let neighbors = &adjacency[idx];
            if neighbors.is_empty() {
                dangling += ranks[idx];
                continue;
            }
            let share = ranks[idx] / neighbors.len() as f64;
            for neighbor in neighbors.iter().copied() {
                next[neighbor] += DAMPING * share;
            }
        }
        if dangling > 0.0 {
            for (idx, weight) in personalization.iter().enumerate() {
                next[idx] += DAMPING * dangling * weight;
            }
        }
        ranks = next;
    }
    ids.into_iter().zip(ranks).collect()
}

fn normalized_personalization(
    graph: &SymbolGraph,
    ids: &[NodeId],
    task_terms: &HashSet<String>,
    context_file_ids: &HashSet<FileId>,
) -> Vec<f64> {
    let mut weights: Vec<f64> = ids
        .iter()
        .map(|id| {
            let Some(node) = graph.node(*id) else {
                return 1.0;
            };
            let mut weight = 1.0;
            if name_matches_task(node, task_terms) {
                weight += TASK_BOOST;
            }
            if context_file_ids.contains(&node.file_id) {
                weight += CONTEXT_FILE_BOOST;
            }
            weight
        })
        .collect();
    let total: f64 = weights.iter().sum();
    if total > 0.0 {
        for weight in &mut weights {
            *weight /= total;
        }
    }
    weights
}

fn name_matches_task(node: &Node, task_terms: &HashSet<String>) -> bool {
    if task_terms.is_empty() {
        return false;
    }
    let name = node.name.to_ascii_lowercase();
    if task_terms.contains(&name) {
        return true;
    }
    identifier_terms(&name)
        .iter()
        .any(|term| task_terms.contains(term))
}

fn identifier_terms(name: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut current = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch.to_ascii_lowercase());
        } else {
            absorb_task_term(&mut out, &mut current);
        }
    }
    absorb_task_term(&mut out, &mut current);
    out
}

#[derive(Debug, Clone)]
struct RepoMapEntry {
    path: String,
    line: u32,
    kind: &'static str,
    name: String,
    signature: String,
    score: f64,
    reasons: Vec<&'static str>,
}

fn ranked_definition_entries(
    graph: &SymbolGraph,
    ranks: &HashMap<NodeId, f64>,
    task_terms: &HashSet<String>,
    context_file_ids: &HashSet<FileId>,
) -> Vec<RepoMapEntry> {
    let mut entries: Vec<RepoMapEntry> = graph
        .all_node_ids()
        .into_iter()
        .filter_map(|id| {
            let node = graph.node(id)?;
            if !matches!(node.kind, NodeKind::Function | NodeKind::Type) {
                return None;
            }
            let mut reasons = Vec::new();
            if name_matches_task(node, task_terms) {
                reasons.push("task_symbol");
            }
            if context_file_ids.contains(&node.file_id) {
                reasons.push("context_file");
            }
            Some(RepoMapEntry {
                path: node.path.clone(),
                line: node.line,
                kind: node.kind.as_str(),
                name: node.name.clone(),
                signature: node.signature.clone(),
                score: ranks.get(&id).copied().unwrap_or(0.0),
                reasons,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.name.cmp(&b.name))
    });
    entries
}

fn render_entries(entries: &[RepoMapEntry], token_budget: usize) -> (String, bool) {
    let char_budget = token_budget.saturating_mul(4);
    if char_budget == 0 {
        return (String::new(), !entries.is_empty());
    }
    let mut remaining = char_budget;
    let mut out = String::new();
    let mut rendered_entries = 0usize;
    let mut current_path = "";
    for entry in entries {
        let header = if current_path != entry.path {
            format!("{}:\n", entry.path)
        } else {
            String::new()
        };
        let line = render_entry_line(entry);
        if header.len() + line.len() > remaining {
            return (out.trim_end().to_string(), true);
        }
        if !header.is_empty() {
            out.push_str(&header);
            remaining -= header.len();
            current_path = &entry.path;
        }
        out.push_str(&line);
        remaining -= line.len();
        rendered_entries += 1;
    }
    (out.trim_end().to_string(), rendered_entries < entries.len())
}

fn render_entry_line(entry: &RepoMapEntry) -> String {
    let detail = if entry.signature.trim().is_empty() {
        format!("{} {}", entry.kind, entry.name)
    } else {
        entry.signature.trim().to_string()
    };
    format!("  L{} {}\n", entry.line, detail)
}

fn entry_to_vm(entry: &RepoMapEntry) -> VmValue {
    build_dict([
        ("path", str_value(&entry.path)),
        ("line", VmValue::Int(entry.line as i64)),
        ("kind", str_value(entry.kind)),
        ("name", str_value(&entry.name)),
        ("signature", str_value(&entry.signature)),
        ("score", VmValue::Float(entry.score)),
        (
            "reasons",
            VmValue::List(Arc::new(
                entry.reasons.iter().copied().map(str_value).collect(),
            )),
        ),
    ])
}
