//! `ast.structural_diff` — syntax-aware, non-patch diff for source files.
//!
//! This is intentionally a review surface, not an apply surface. It parses
//! both sides with the existing tree-sitter language registry, compares the
//! syntax tree while ignoring trivia, and returns changed node spans. When
//! structural comparison is unsafe or too expensive, it returns a line-diff
//! payload instead of raising: callers can render one shape and keep patch
//! application on the existing line-based path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_vm::VmValue;
use sha2::{Digest, Sha256};
use tree_sitter::{Node, Tree};

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_int, optional_string, require_string, str_value,
};

use super::language::Language;
use super::parse::parse_source;
use super::unified_diff::{render as render_unified_diff, ChangeKind};

const BUILTIN: &str = "hostlib_ast_structural_diff";
const DEFAULT_MAX_BYTES: usize = 1_048_576;
const DEFAULT_MAX_NODES: usize = 20_000;
const DEFAULT_MAX_GRAPH_EDGES: usize = 20_000_000;
const MOVE_MIN_BYTES: usize = 4;
const SNIPPET_LIMIT: usize = 200;

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_a = require_string(BUILTIN, dict, "path_a")?;
    let path_b = require_string(BUILTIN, dict, "path_b")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let limits = Limits::from_payload(dict)?;

    let before = read_source(&path_a)?;
    let after = read_source(&path_b)?;

    if let Some(reason) = limits.byte_fallback_reason(before.len(), after.len()) {
        return Ok(line_fallback(
            &path_a,
            &path_b,
            &before,
            &after,
            language_hint.as_deref().and_then(Language::from_name),
            &reason,
            &limits,
        ));
    }

    let language = match detect_language(&path_a, &path_b, language_hint.as_deref()) {
        Ok(language) => language,
        Err(reason) => {
            return Ok(line_fallback(
                &path_a, &path_b, &before, &after, None, &reason, &limits,
            ));
        }
    };

    let before_tree = match parse_valid_source(&before, language) {
        Ok(tree) => tree,
        Err(reason) => {
            return Ok(line_fallback(
                &path_a,
                &path_b,
                &before,
                &after,
                Some(language),
                reason,
                &limits,
            ));
        }
    };
    let after_tree = match parse_valid_source(&after, language) {
        Ok(tree) => tree,
        Err(reason) => {
            return Ok(line_fallback(
                &path_a,
                &path_b,
                &before,
                &after,
                Some(language),
                reason,
                &limits,
            ));
        }
    };

    let before_ast = match StructuralTree::build(&before_tree, &before, &path_a, &limits) {
        Ok(tree) => tree,
        Err(reason) => {
            return Ok(line_fallback(
                &path_a,
                &path_b,
                &before,
                &after,
                Some(language),
                &reason,
                &limits,
            ));
        }
    };
    let after_ast = match StructuralTree::build(&after_tree, &after, &path_b, &limits) {
        Ok(tree) => tree,
        Err(reason) => {
            return Ok(line_fallback(
                &path_a,
                &path_b,
                &before,
                &after,
                Some(language),
                &reason,
                &limits,
            ));
        }
    };
    if let Some(reason) =
        limits.graph_fallback_reason(before_ast.nodes.len(), after_ast.nodes.len())
    {
        return Ok(line_fallback(
            &path_a,
            &path_b,
            &before,
            &after,
            Some(language),
            &reason,
            &limits,
        ));
    }

    let mut diff = TreeDiff::new(&before_ast, &after_ast, &limits);
    if let Err(reason) = diff.diff_roots() {
        return Ok(line_fallback(
            &path_a,
            &path_b,
            &before,
            &after,
            Some(language),
            &reason,
            &limits,
        ));
    }

    Ok(structural_response(
        &path_a,
        &path_b,
        language,
        &before_ast,
        &after_ast,
        &diff.changes,
        &limits,
    ))
}

#[derive(Clone, Copy)]
struct Limits {
    max_bytes: usize,
    max_nodes: usize,
    max_graph_edges: usize,
}

impl Limits {
    fn from_payload(dict: &BTreeMap<String, VmValue>) -> Result<Self, HostlibError> {
        Ok(Self {
            max_bytes: optional_limit(dict, "max_bytes", DEFAULT_MAX_BYTES)?,
            max_nodes: optional_limit(dict, "max_nodes", DEFAULT_MAX_NODES)?,
            max_graph_edges: optional_limit(dict, "max_graph_edges", DEFAULT_MAX_GRAPH_EDGES)?,
        })
    }

    fn byte_fallback_reason(self, before_len: usize, after_len: usize) -> Option<String> {
        if self.max_bytes == 0 {
            return None;
        }
        let total = before_len.saturating_add(after_len);
        (total > self.max_bytes).then(|| {
            format!(
                "byte_limit_exceeded: {total} bytes exceeds max_bytes {}",
                self.max_bytes
            )
        })
    }

    fn graph_fallback_reason(self, before_nodes: usize, after_nodes: usize) -> Option<String> {
        if self.max_nodes > 0 && (before_nodes > self.max_nodes || after_nodes > self.max_nodes) {
            return Some(format!(
                "node_limit_exceeded: {before_nodes}/{after_nodes} nodes exceeds max_nodes {}",
                self.max_nodes
            ));
        }
        if self.max_graph_edges == 0 {
            return None;
        }
        let edges = before_nodes.saturating_mul(after_nodes);
        (edges > self.max_graph_edges).then(|| {
            format!(
                "graph_limit_exceeded: {edges} candidate edges exceeds max_graph_edges {}",
                self.max_graph_edges
            )
        })
    }
}

fn optional_limit(
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
    default: usize,
) -> Result<usize, HostlibError> {
    let value = optional_int(BUILTIN, dict, key, default as i64)?;
    if value < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: key,
            message: "must be >= 0".into(),
        });
    }
    Ok(value as usize)
}

fn detect_language(path_a: &str, path_b: &str, hint: Option<&str>) -> Result<Language, String> {
    if let Some(hint) = hint.filter(|s| !s.trim().is_empty()) {
        return Language::from_name(hint)
            .ok_or_else(|| format!("unsupported_language: `{hint}` is not registered"));
    }
    let a = Language::detect(Path::new(path_a), None);
    let b = Language::detect(Path::new(path_b), None);
    match (a, b) {
        (Some(left), Some(right)) if left == right => Ok(left),
        (Some(left), None) => Ok(left),
        (None, Some(right)) => Ok(right),
        (Some(left), Some(right)) => Err(format!(
            "language_mismatch: `{}` vs `{}`; pass `language` to override",
            left.name(),
            right.name()
        )),
        (None, None) => Err("unsupported_language: could not infer a registered grammar".into()),
    }
}

fn read_source(path: &str) -> Result<String, HostlibError> {
    let path_buf = PathBuf::from(path);
    let bytes = match crate::fs::read(&path_buf, None) {
        Some(result) => result,
        None => std::fs::read(path),
    }
    .map_err(|err| HostlibError::Backend {
        builtin: BUILTIN,
        message: format!("read `{path}`: {err}"),
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_valid_source(source: &str, language: Language) -> Result<Tree, &'static str> {
    match parse_source(source, language) {
        Ok(tree) if !tree.root_node().has_error() => Ok(tree),
        Ok(_) | Err(_) => Err("parse_error"),
    }
}

struct StructuralTree {
    path: String,
    source: String,
    root: usize,
    nodes: Vec<StructuralNode>,
    visited_nodes: usize,
}

impl StructuralTree {
    fn build(tree: &Tree, source: &str, path: &str, limits: &Limits) -> Result<Self, String> {
        let mut out = Self {
            path: path.to_string(),
            source: source.to_string(),
            root: 0,
            nodes: Vec::new(),
            visited_nodes: 0,
        };
        out.root = out.push_node(tree.root_node(), limits)?;
        Ok(out)
    }

    fn push_node(&mut self, node: Node<'_>, limits: &Limits) -> Result<usize, String> {
        if let Some(reason) = node_limit_fallback_reason(self.visited_nodes, limits) {
            return Err(reason);
        }
        self.visited_nodes += 1;

        let mut children = Vec::with_capacity(node.child_count());
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            children.push(self.push_node(child, limits)?);
        }

        let start = node.start_position();
        let end = node.end_position();
        let text = if children.is_empty() {
            self.source
                .get(node.start_byte()..node.end_byte())
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        let fingerprint = self.fingerprint(node, &children, &text);
        let id = self.nodes.len();
        self.nodes.push(StructuralNode {
            kind: node.kind().to_string(),
            is_named: node.is_named(),
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_row: start.row,
            start_col: start.column,
            end_row: end.row,
            end_col: end.column,
            children,
            fingerprint,
            text,
        });
        Ok(id)
    }

    fn fingerprint(&self, node: Node<'_>, children: &[usize], leaf_text: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(node.kind().as_bytes());
        hasher.update([u8::from(node.is_named())]);
        if children.is_empty() {
            hasher.update(b"\0leaf\0");
            hasher.update(leaf_text.as_bytes());
        } else {
            hasher.update(b"\0node\0");
            for &child in children {
                hasher.update(self.nodes[child].fingerprint.as_bytes());
                hasher.update(b"\0");
            }
        }
        hex::encode(hasher.finalize())
    }

    fn source_for(&self, node: &StructuralNode) -> &str {
        self.source
            .get(node.start_byte..node.end_byte)
            .unwrap_or("")
    }
}

fn node_limit_fallback_reason(current_nodes: usize, limits: &Limits) -> Option<String> {
    (limits.max_nodes > 0 && current_nodes >= limits.max_nodes).then(|| {
        format!(
            "node_limit_exceeded: parsed tree exceeds max_nodes {}",
            limits.max_nodes
        )
    })
}

struct StructuralNode {
    kind: String,
    is_named: bool,
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
    children: Vec<usize>,
    fingerprint: String,
    text: String,
}

impl StructuralNode {
    fn byte_len(&self) -> usize {
        self.end_byte.saturating_sub(self.start_byte)
    }

    fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }

    fn move_candidate(&self) -> bool {
        self.is_named && self.byte_len() >= MOVE_MIN_BYTES
    }

    fn same_shape(&self, other: &StructuralNode) -> bool {
        self.kind == other.kind && self.is_named == other.is_named
    }
}

struct TreeDiff<'a> {
    before: &'a StructuralTree,
    after: &'a StructuralTree,
    limits: &'a Limits,
    changes: Vec<Change>,
}

impl<'a> TreeDiff<'a> {
    fn new(before: &'a StructuralTree, after: &'a StructuralTree, limits: &'a Limits) -> Self {
        Self {
            before,
            after,
            limits,
            changes: Vec::new(),
        }
    }

    fn diff_roots(&mut self) -> Result<(), String> {
        self.diff_node(self.before.root, self.after.root)
    }

    fn diff_node(&mut self, before_id: usize, after_id: usize) -> Result<(), String> {
        let before = &self.before.nodes[before_id];
        let after = &self.after.nodes[after_id];
        if before.fingerprint == after.fingerprint {
            return Ok(());
        }
        if before.same_shape(after) && !before.is_leaf() && !after.is_leaf() {
            return self.diff_children(&before.children, &after.children);
        }
        self.changes.push(Change::Replace {
            before: before_id,
            after: after_id,
        });
        Ok(())
    }

    fn diff_children(&mut self, before: &[usize], after: &[usize]) -> Result<(), String> {
        if self.record_pure_reorder(before, after) {
            return Ok(());
        }

        let pairs = self.lcs_pairs(before, after)?;
        let mut before_cursor = 0usize;
        let mut after_cursor = 0usize;

        for (before_match, after_match) in pairs {
            self.diff_unmatched(
                &before[before_cursor..before_match],
                &after[after_cursor..after_match],
            )?;
            self.diff_node(before[before_match], after[after_match])?;
            before_cursor = before_match + 1;
            after_cursor = after_match + 1;
        }

        self.diff_unmatched(&before[before_cursor..], &after[after_cursor..])
    }

    fn record_pure_reorder(&mut self, before: &[usize], after: &[usize]) -> bool {
        if before.len() != after.len() || before.len() < 2 {
            return false;
        }
        let before_fps: Vec<&str> = before
            .iter()
            .map(|&id| self.before.nodes[id].fingerprint.as_str())
            .collect();
        let after_fps: Vec<&str> = after
            .iter()
            .map(|&id| self.after.nodes[id].fingerprint.as_str())
            .collect();
        if before_fps == after_fps {
            return false;
        }

        let mut counts: BTreeMap<&str, isize> = BTreeMap::new();
        for fp in &before_fps {
            *counts.entry(*fp).or_default() += 1;
        }
        for fp in &after_fps {
            *counts.entry(*fp).or_default() -= 1;
        }
        if counts.values().any(|count| *count != 0) {
            return false;
        }

        let mut after_used = vec![false; after.len()];
        let mut moves = Vec::new();
        for (i, &before_id) in before.iter().enumerate() {
            if self.same_fingerprint(before_id, after[i]) {
                after_used[i] = true;
                continue;
            }
            if !self.before.nodes[before_id].move_candidate() {
                return false;
            }
            let Some(j) = after.iter().enumerate().find_map(|(j, &after_id)| {
                (!after_used[j]
                    && self.same_fingerprint(before_id, after_id)
                    && self.after.nodes[after_id].move_candidate())
                .then_some(j)
            }) else {
                return false;
            };
            after_used[j] = true;
            moves.push(Change::Move {
                before: before_id,
                after: after[j],
            });
        }

        if moves.is_empty() {
            return false;
        }
        self.changes.extend(moves);
        true
    }

    fn lcs_pairs(&self, before: &[usize], after: &[usize]) -> Result<Vec<(usize, usize)>, String> {
        let m = before.len();
        let n = after.len();
        if self.lcs_cells_exceed_limit(m, n) {
            return Err(format!(
                "graph_limit_exceeded: child graph {m}x{n} exceeds max_graph_edges {}",
                self.limits.max_graph_edges
            ));
        }
        let width = n + 1;
        let mut dp = vec![0u32; (m + 1) * (n + 1)];
        for i in (0..m).rev() {
            for j in (0..n).rev() {
                let idx = i * width + j;
                dp[idx] = if self.same_fingerprint(before[i], after[j]) {
                    dp[(i + 1) * width + j + 1] + 1
                } else {
                    dp[(i + 1) * width + j].max(dp[i * width + j + 1])
                };
            }
        }

        let mut pairs = Vec::new();
        let mut i = 0usize;
        let mut j = 0usize;
        while i < m && j < n {
            if self.same_fingerprint(before[i], after[j]) {
                pairs.push((i, j));
                i += 1;
                j += 1;
            } else if dp[(i + 1) * width + j] >= dp[i * width + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        Ok(pairs)
    }

    fn lcs_cells_exceed_limit(&self, before_len: usize, after_len: usize) -> bool {
        self.limits.max_graph_edges > 0
            && before_len.saturating_mul(after_len) > self.limits.max_graph_edges
    }

    fn diff_unmatched(&mut self, before: &[usize], after: &[usize]) -> Result<(), String> {
        let mut before_used = vec![false; before.len()];
        let mut after_used = vec![false; after.len()];

        for (i, &before_id) in before.iter().enumerate() {
            if !self.before.nodes[before_id].move_candidate() {
                continue;
            }
            if let Some(j) = after.iter().enumerate().find_map(|(j, &after_id)| {
                (!after_used[j]
                    && self.same_fingerprint(before_id, after_id)
                    && self.after.nodes[after_id].move_candidate())
                .then_some(j)
            }) {
                before_used[i] = true;
                after_used[j] = true;
                self.changes.push(Change::Move {
                    before: before_id,
                    after: after[j],
                });
            }
        }

        for (i, &before_id) in before.iter().enumerate() {
            if before_used[i] {
                continue;
            }
            if let Some(j) = after.iter().enumerate().find_map(|(j, &after_id)| {
                (!after_used[j]
                    && self.before.nodes[before_id].same_shape(&self.after.nodes[after_id]))
                .then_some(j)
            }) {
                before_used[i] = true;
                after_used[j] = true;
                self.diff_node(before_id, after[j])?;
            }
        }

        for (i, &before_id) in before.iter().enumerate() {
            if !before_used[i] {
                self.changes.push(Change::Delete { before: before_id });
            }
        }
        for (j, &after_id) in after.iter().enumerate() {
            if !after_used[j] {
                self.changes.push(Change::Insert { after: after_id });
            }
        }
        Ok(())
    }

    fn same_fingerprint(&self, before_id: usize, after_id: usize) -> bool {
        self.before.nodes[before_id].fingerprint == self.after.nodes[after_id].fingerprint
    }
}

enum Change {
    Insert { after: usize },
    Delete { before: usize },
    Replace { before: usize, after: usize },
    Move { before: usize, after: usize },
}

impl Change {
    fn kind(&self) -> &'static str {
        match self {
            Change::Insert { .. } => "insert",
            Change::Delete { .. } => "delete",
            Change::Replace { .. } => "replace",
            Change::Move { .. } => "move",
        }
    }
}

fn structural_response(
    path_a: &str,
    path_b: &str,
    language: Language,
    before: &StructuralTree,
    after: &StructuralTree,
    changes: &[Change],
    limits: &Limits,
) -> VmValue {
    let mut counts = ChangeCounts::default();
    for change in changes {
        counts.record(change);
    }
    let change_values: Vec<VmValue> = changes
        .iter()
        .map(|change| change_to_vm(change, before, after))
        .collect();
    build_dict([
        ("result", str_value("ok")),
        ("mode", str_value("structural")),
        ("changed", VmValue::Bool(!changes.is_empty())),
        ("path_a", str_value(path_a)),
        ("path_b", str_value(path_b)),
        ("language", str_value(language.name())),
        ("fallback_reason", VmValue::Nil),
        ("changes", VmValue::List(Arc::new(change_values))),
        ("line_diff", VmValue::Nil),
        (
            "summary",
            summary_value(&counts, 0, 0, before.nodes.len(), after.nodes.len()),
        ),
        ("limits", limits_value(limits)),
    ])
}

fn line_fallback(
    path_a: &str,
    path_b: &str,
    before: &str,
    after: &str,
    language: Option<Language>,
    reason: &str,
    limits: &Limits,
) -> VmValue {
    let display_path = fallback_display_path(path_a, path_b);
    let diff = render_unified_diff(&display_path, before, after, ChangeKind::Modify);
    let line_diff = build_dict([
        ("path", str_value(display_path)),
        ("diff", str_value(&diff.text)),
        ("lines_added", VmValue::Int(diff.lines_added as i64)),
        ("lines_removed", VmValue::Int(diff.lines_removed as i64)),
    ]);
    let counts = ChangeCounts::default();
    build_dict([
        ("result", str_value("fallback")),
        ("mode", str_value("line")),
        ("changed", VmValue::Bool(before != after)),
        ("path_a", str_value(path_a)),
        ("path_b", str_value(path_b)),
        (
            "language",
            language.map_or(VmValue::Nil, |lang| str_value(lang.name())),
        ),
        ("fallback_reason", str_value(reason)),
        ("changes", VmValue::List(Arc::new(Vec::new()))),
        ("line_diff", line_diff),
        (
            "summary",
            summary_value(&counts, diff.lines_added, diff.lines_removed, 0, 0),
        ),
        ("limits", limits_value(limits)),
    ])
}

fn fallback_display_path(path_a: &str, path_b: &str) -> String {
    if path_a == path_b {
        return path_a.to_string();
    }
    format!("{path_a}..{path_b}")
}

#[derive(Default)]
struct ChangeCounts {
    inserts: usize,
    deletes: usize,
    replaces: usize,
    moves: usize,
}

impl ChangeCounts {
    fn record(&mut self, change: &Change) {
        match change {
            Change::Insert { .. } => self.inserts += 1,
            Change::Delete { .. } => self.deletes += 1,
            Change::Replace { .. } => self.replaces += 1,
            Change::Move { .. } => self.moves += 1,
        }
    }

    fn total(&self) -> usize {
        self.inserts + self.deletes + self.replaces + self.moves
    }
}

fn summary_value(
    counts: &ChangeCounts,
    line_insertions: usize,
    line_deletions: usize,
    nodes_before: usize,
    nodes_after: usize,
) -> VmValue {
    build_dict([
        ("change_count", VmValue::Int(counts.total() as i64)),
        ("insertions", VmValue::Int(counts.inserts as i64)),
        ("deletions", VmValue::Int(counts.deletes as i64)),
        ("replacements", VmValue::Int(counts.replaces as i64)),
        ("moves", VmValue::Int(counts.moves as i64)),
        ("line_insertions", VmValue::Int(line_insertions as i64)),
        ("line_deletions", VmValue::Int(line_deletions as i64)),
        ("nodes_before", VmValue::Int(nodes_before as i64)),
        ("nodes_after", VmValue::Int(nodes_after as i64)),
    ])
}

fn limits_value(limits: &Limits) -> VmValue {
    build_dict([
        ("max_bytes", VmValue::Int(limits.max_bytes as i64)),
        ("max_nodes", VmValue::Int(limits.max_nodes as i64)),
        (
            "max_graph_edges",
            VmValue::Int(limits.max_graph_edges as i64),
        ),
    ])
}

fn change_to_vm(change: &Change, before: &StructuralTree, after: &StructuralTree) -> VmValue {
    match change {
        Change::Insert { after: after_id } => {
            let after_node = &after.nodes[*after_id];
            build_dict([
                ("kind", str_value(change.kind())),
                ("node_kind", str_value(&after_node.kind)),
                ("before", VmValue::Nil),
                ("after", span_value(after, after_node)),
                ("before_text", VmValue::Nil),
                ("after_text", snippet_value(after.source_for(after_node))),
            ])
        }
        Change::Delete { before: before_id } => {
            let before_node = &before.nodes[*before_id];
            build_dict([
                ("kind", str_value(change.kind())),
                ("node_kind", str_value(&before_node.kind)),
                ("before", span_value(before, before_node)),
                ("after", VmValue::Nil),
                ("before_text", snippet_value(before.source_for(before_node))),
                ("after_text", VmValue::Nil),
            ])
        }
        Change::Replace {
            before: before_id,
            after: after_id,
        } => {
            let before_node = &before.nodes[*before_id];
            let after_node = &after.nodes[*after_id];
            build_dict([
                ("kind", str_value(change.kind())),
                ("node_kind", str_value(&before_node.kind)),
                ("before", span_value(before, before_node)),
                ("after", span_value(after, after_node)),
                ("before_text", snippet_value(node_text(before, before_node))),
                ("after_text", snippet_value(node_text(after, after_node))),
            ])
        }
        Change::Move {
            before: before_id,
            after: after_id,
        } => {
            let before_node = &before.nodes[*before_id];
            let after_node = &after.nodes[*after_id];
            build_dict([
                ("kind", str_value(change.kind())),
                ("node_kind", str_value(&before_node.kind)),
                ("before", span_value(before, before_node)),
                ("after", span_value(after, after_node)),
                ("before_text", snippet_value(before.source_for(before_node))),
                ("after_text", snippet_value(after.source_for(after_node))),
            ])
        }
    }
}

fn node_text<'a>(tree: &'a StructuralTree, node: &'a StructuralNode) -> &'a str {
    if node.is_leaf() {
        &node.text
    } else {
        tree.source_for(node)
    }
}

fn span_value(tree: &StructuralTree, node: &StructuralNode) -> VmValue {
    build_dict([
        ("path", str_value(&tree.path)),
        ("start_byte", VmValue::Int(node.start_byte as i64)),
        ("end_byte", VmValue::Int(node.end_byte as i64)),
        ("start_row", VmValue::Int(node.start_row as i64)),
        ("start_col", VmValue::Int(node.start_col as i64)),
        ("end_row", VmValue::Int(node.end_row as i64)),
        ("end_col", VmValue::Int(node.end_col as i64)),
    ])
}

fn snippet_value(text: &str) -> VmValue {
    let mut snippet = String::new();
    let mut bytes = 0usize;
    for ch in text.chars() {
        let len = ch.len_utf8();
        if bytes + len > SNIPPET_LIMIT {
            snippet.push_str("...");
            break;
        }
        snippet.push(ch);
        bytes += len;
    }
    str_value(snippet)
}
