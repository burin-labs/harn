//! Typed symbol graph layered on top of the flat code index.
//!
//! Nodes are typed by [`NodeKind`] (Function, Type, Field, EnumCase,
//! Module, Import, CallSite, Macro) and edges by [`EdgeKind`] (Calls,
//! Refs, Imports, Contains, Overrides). The graph is built lazily from the AST symbol
//! extractor and the existing import [`super::DepGraph`]; it does not
//! duplicate the trigram or word indexes.
//!
//! [`SymbolGraph::rebuild_file`] re-parses a single file and replaces the
//! node + edge slice belonging to that file. Both forward and reverse
//! adjacency lists are kept so the Cypher executor in [`super::cypher`]
//! can traverse `<-[:EDGE]-` patterns without rescanning.

use std::collections::{BTreeSet, HashMap};

use tree_sitter::{Node as TsNode, Tree};

use crate::ast::{api as ast_api, Language, Symbol, SymbolKind};

use super::file_table::FileId;

/// Typed node identifier. Stable across `rebuild_file` calls that don't
/// touch the file (id assignment is per-file deterministic — see
/// [`SymbolGraph::rebuild_file`]).
pub type NodeId = u32;

/// Coarse typed node kinds defined in issue #2434.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// Functions, methods, free-standing closures with names.
    Function,
    /// Classes, structs, enums, interfaces, protocols, type aliases.
    Type,
    /// Struct/class/interface fields and properties.
    Field,
    /// Individual enum cases / variants.
    EnumCase,
    /// One per indexed file; acts as the container for top-level decls.
    Module,
    /// One per raw import string surfaced by the import extractor.
    Import,
    /// One per `f(...)` call expression matched in source.
    CallSite,
    /// Macro definitions (reserved for language-specific extraction).
    Macro,
}

impl NodeKind {
    /// Label-case wire form used by Cypher and the JSON projection.
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Function => "Function",
            NodeKind::Type => "Type",
            NodeKind::Field => "Field",
            NodeKind::EnumCase => "EnumCase",
            NodeKind::Module => "Module",
            NodeKind::Import => "Import",
            NodeKind::CallSite => "CallSite",
            NodeKind::Macro => "Macro",
        }
    }

    /// Parse a case-sensitive Cypher label.
    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "Function" => Some(NodeKind::Function),
            "Type" => Some(NodeKind::Type),
            "Field" => Some(NodeKind::Field),
            "EnumCase" => Some(NodeKind::EnumCase),
            "Module" => Some(NodeKind::Module),
            "Import" => Some(NodeKind::Import),
            "CallSite" => Some(NodeKind::CallSite),
            "Macro" => Some(NodeKind::Macro),
            _ => None,
        }
    }
}

/// Coarse typed edge kinds defined in issue #2434.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// CallSite → Function. A call expression resolving to a function.
    Calls,
    /// Module → any. Cheap heuristic for cross-file name references.
    Refs,
    /// Module → Module (resolved) or Module → Import (unresolved).
    Imports,
    /// Container → child. Module-to-decl or Type-to-method.
    Contains,
    /// Method → method. Reserved for explicit overrides.
    Overrides,
}

impl EdgeKind {
    /// Wire form (uppercase, matches Cypher convention).
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Calls => "CALLS",
            EdgeKind::Refs => "REFS",
            EdgeKind::Imports => "IMPORTS",
            EdgeKind::Contains => "CONTAINS",
            EdgeKind::Overrides => "OVERRIDES",
        }
    }

    /// Parse an edge label, accepting both forward (`CALLS`) and inverse
    /// (`CALLED_BY`) spellings. Returns `(kind, reversed)` so the executor
    /// flips direction during traversal.
    pub fn parse_with_direction(label: &str) -> Option<(Self, bool)> {
        if let Some(kind) = forward_match(label) {
            return Some((kind, false));
        }
        match label {
            "CALLED_BY" => Some((EdgeKind::Calls, true)),
            "REFERENCED_BY" => Some((EdgeKind::Refs, true)),
            "IMPORTED_BY" => Some((EdgeKind::Imports, true)),
            "CONTAINED_BY" => Some((EdgeKind::Contains, true)),
            "OVERRIDDEN_BY" => Some((EdgeKind::Overrides, true)),
            _ => None,
        }
    }
}

fn forward_match(label: &str) -> Option<EdgeKind> {
    match label {
        "CALLS" => Some(EdgeKind::Calls),
        "REFS" => Some(EdgeKind::Refs),
        "IMPORTS" => Some(EdgeKind::Imports),
        "CONTAINS" => Some(EdgeKind::Contains),
        "OVERRIDES" => Some(EdgeKind::Overrides),
        _ => None,
    }
}

/// One typed node in the symbol graph. `line` is 1-based to match the
/// rest of the host-builtin wire format; `path` is workspace-relative.
#[derive(Debug, Clone)]
pub struct Node {
    /// Stable graph-local id assigned at construction.
    pub id: NodeId,
    /// Typed kind ([`NodeKind`]).
    pub kind: NodeKind,
    /// Display name (function/type identifier, module basename, or
    /// raw import string for [`NodeKind::Import`]).
    pub name: String,
    /// Owning file id from the flat code index.
    pub file_id: FileId,
    /// Workspace-relative path of the owning file.
    pub path: String,
    /// 1-based start line within the file.
    pub line: u32,
    /// Single-line signature/preview.
    pub signature: String,
    /// Enclosing container name (class/struct/module), if any.
    pub container: Option<String>,
    /// Normalized declaration access level when known.
    pub access_level: Option<String>,
    /// Tree-sitter language name (e.g. `"rust"`, `"typescript"`).
    pub language: String,
}

/// One directed edge.
#[derive(Debug, Clone, Copy)]
pub struct Edge {
    /// Source node id.
    pub from: NodeId,
    /// Destination node id.
    pub to: NodeId,
    /// Typed edge kind ([`EdgeKind`]).
    pub kind: EdgeKind,
}

/// Result of [`SymbolGraph::rebuild_file`]. Exposes the flat symbol list
/// produced by the tree-sitter parse so callers can populate sibling
/// indexes (e.g. `IndexedFile::symbols`) without re-parsing.
#[derive(Debug, Clone, Default)]
pub struct RebuildOutcome {
    /// Number of nodes installed for this file, including the Module
    /// node. Matches the previous `usize` return value.
    pub node_count: usize,
    /// Flat symbol list extracted from the parse. Empty when the
    /// grammar didn't recognise the source.
    pub symbols: Vec<Symbol>,
}

/// Typed symbol graph for a single workspace.
#[derive(Debug, Default, Clone)]
pub struct SymbolGraph {
    nodes: HashMap<NodeId, Node>,
    by_file: HashMap<FileId, Vec<NodeId>>,
    by_name: HashMap<String, Vec<NodeId>>,
    out_edges: HashMap<NodeId, Vec<Edge>>,
    in_edges: HashMap<NodeId, Vec<Edge>>,
    next_id: NodeId,
}

impl SymbolGraph {
    /// Construct an empty graph.
    pub fn new() -> Self {
        Self {
            next_id: 1,
            ..Self::default()
        }
    }

    /// Total node count.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Total edge count.
    pub fn edge_count(&self) -> usize {
        self.out_edges.values().map(Vec::len).sum()
    }

    /// Borrow a node by id.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Iterate every node (order is unspecified).
    pub fn iter_nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    /// All node ids of a specific kind. Used by the Cypher executor's
    /// label-driven scan.
    pub fn nodes_of_kind(&self, kind: NodeKind) -> Vec<NodeId> {
        let mut out: Vec<NodeId> = self
            .nodes
            .values()
            .filter(|n| n.kind == kind)
            .map(|n| n.id)
            .collect();
        out.sort_unstable();
        out
    }

    /// Every node id, sorted. Used as the unfiltered scan when the
    /// Cypher pattern has no label predicate.
    pub fn all_node_ids(&self) -> Vec<NodeId> {
        let mut out: Vec<NodeId> = self.nodes.keys().copied().collect();
        out.sort_unstable();
        out
    }

    /// All nodes matching `name` (case-sensitive). Empty when no match.
    pub fn nodes_named(&self, name: &str) -> &[NodeId] {
        match self.by_name.get(name) {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    /// Outgoing edges from `id`.
    pub fn outgoing(&self, id: NodeId) -> &[Edge] {
        self.out_edges.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Incoming edges to `id`.
    pub fn incoming(&self, id: NodeId) -> &[Edge] {
        self.in_edges.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// File ids that own at least one node in this graph.
    pub fn file_ids(&self) -> Vec<FileId> {
        let mut out: Vec<FileId> = self.by_file.keys().copied().collect();
        out.sort_unstable();
        out
    }

    /// Drop every node + edge owned by `file_id`.
    pub fn remove_file(&mut self, file_id: FileId) {
        let Some(node_ids) = self.by_file.remove(&file_id) else {
            return;
        };
        for id in node_ids {
            self.drop_node(id);
        }
    }

    fn drop_node(&mut self, id: NodeId) {
        let Some(node) = self.nodes.remove(&id) else {
            return;
        };
        if let Some(bucket) = self.by_name.get_mut(&node.name) {
            bucket.retain(|n| *n != id);
            if bucket.is_empty() {
                self.by_name.remove(&node.name);
            }
        }
        if let Some(outs) = self.out_edges.remove(&id) {
            for e in outs {
                if let Some(bucket) = self.in_edges.get_mut(&e.to) {
                    bucket.retain(|edge| edge.from != id);
                }
            }
        }
        if let Some(ins) = self.in_edges.remove(&id) {
            for e in ins {
                if let Some(bucket) = self.out_edges.get_mut(&e.from) {
                    bucket.retain(|edge| edge.to != id);
                }
            }
        }
    }

    /// Replace every node + edge belonging to `file_id` with the freshly
    /// parsed set derived from `source`. Returns the count of nodes
    /// installed (including the per-file Module node) along with the
    /// flat symbol list that was extracted from the parse — callers
    /// (notably [`super::IndexState`]) reuse the symbol list to populate
    /// `IndexedFile::symbols` without re-parsing.
    pub fn rebuild_file(
        &mut self,
        file_id: FileId,
        path: &str,
        language: Language,
        source: &str,
        import_strings: &[String],
    ) -> RebuildOutcome {
        self.remove_file(file_id);
        let module_id = self.add_module_for_file(file_id, path, &language);

        // Parse once and reuse the tree for both symbol extraction and
        // the call-site sweep. Falling back to empty results when the
        // grammar is unhappy keeps one bad file from poisoning the
        // wider rebuild.
        let (tree, symbols) = match ast_api::parse_with_symbols(source, language) {
            Ok((t, s)) => (Some(t), s),
            Err(err) => {
                tracing::debug!(
                    "code_index: tree-sitter parse failed for `{path}`: {err}; \
                     symbol graph slice will be Module-only"
                );
                (None, Vec::new())
            }
        };

        // Functions / Types / Modules + CONTAINS edges. Nested decls
        // point at a previously-emitted container symbol when one
        // exists, otherwise at the file's Module node.
        let mut container_ids: HashMap<String, NodeId> = HashMap::new();
        for sym in &symbols {
            let Some(kind) = map_symbol_kind(sym.kind) else {
                continue;
            };
            let id = self.add_node(Node {
                id: 0,
                kind,
                name: sym.name.clone(),
                file_id,
                path: path.to_string(),
                line: sym.start_row.saturating_add(1),
                signature: sym.signature.clone(),
                container: sym.container.clone(),
                access_level: sym.access_level.clone(),
                language: language.name().to_string(),
            });
            if matches!(kind, NodeKind::Type | NodeKind::Module) {
                container_ids.insert(sym.name.clone(), id);
            }
            let parent_id = sym
                .container
                .as_deref()
                .and_then(|c| container_ids.get(c).copied())
                .unwrap_or(module_id);
            self.add_edge(parent_id, id, EdgeKind::Contains);
        }

        // CallSite nodes + CALLS edges. Targets are resolved against
        // the global by-name index, so cross-file calls become callable
        // once every file has been ingested at least once.
        if let Some(tree) = tree.as_ref() {
            for (callee_name, line) in extract_call_sites_from_tree(tree, source) {
                let call_id = self.add_node(Node {
                    id: 0,
                    kind: NodeKind::CallSite,
                    name: callee_name.clone(),
                    file_id,
                    path: path.to_string(),
                    line,
                    signature: format!("{callee_name}(…)"),
                    container: None,
                    access_level: None,
                    language: language.name().to_string(),
                });
                self.add_edge(module_id, call_id, EdgeKind::Contains);
                let targets: Vec<NodeId> = self
                    .nodes_named(&callee_name)
                    .iter()
                    .copied()
                    .filter(|nid| {
                        self.nodes
                            .get(nid)
                            .is_some_and(|n| matches!(n.kind, NodeKind::Function))
                    })
                    .collect();
                for t in targets {
                    self.add_edge(call_id, t, EdgeKind::Calls);
                }
            }
        }

        // Import nodes — one per raw import string. IMPORTS edge from
        // the file's Module to the Import marker. A second resolution
        // pass in [`Self::link_imports`] adds Module→Module edges once
        // every file has been ingested.
        for raw in import_strings {
            let imp_id = self.add_node(Node {
                id: 0,
                kind: NodeKind::Import,
                name: raw.clone(),
                file_id,
                path: path.to_string(),
                line: 1,
                signature: format!("import {raw}"),
                container: None,
                access_level: None,
                language: language.name().to_string(),
            });
            self.add_edge(module_id, imp_id, EdgeKind::Imports);
        }

        // REFS: any cross-file by-name reference. Cheap heuristic; the
        // REFS edge isn't meant to be precise. Collected upfront so the
        // immutable borrow of `self` ends before we add the edges.
        for target in self.collect_cross_file_refs(source, file_id) {
            self.add_edge(module_id, target, EdgeKind::Refs);
        }

        let node_count = self.by_file.get(&file_id).map(Vec::len).unwrap_or_default();
        RebuildOutcome {
            node_count,
            symbols,
        }
    }

    /// Resolve every IMPORTS edge whose target is currently an `Import`
    /// marker to the corresponding Module-to-Module edge, using the
    /// resolution table from the flat dep graph. Add-only: the marker
    /// edges remain so the Import nodes still anchor the raw strings.
    pub fn link_imports(&mut self, resolved: &HashMap<FileId, Vec<FileId>>) {
        for (src_file, targets) in resolved {
            let Some(src_module) = self.module_node_for_file(*src_file) else {
                continue;
            };
            for tgt_file in targets {
                let Some(tgt_module) = self.module_node_for_file(*tgt_file) else {
                    continue;
                };
                // Idempotent add: `link_imports` re-runs over the WHOLE
                // workspace after every per-file reindex, but `rebuild_file`
                // only clears the reindexed file's edges. Without this guard,
                // every reindex appends another copy of every still-valid
                // Module→Module edge, growing the graph without bound and
                // returning duplicate rows from IMPORTS/IMPORTED_BY traversals.
                let already_linked = self.out_edges.get(&src_module).is_some_and(|edges| {
                    edges
                        .iter()
                        .any(|e| e.to == tgt_module && e.kind == EdgeKind::Imports)
                });
                if !already_linked {
                    self.add_edge(src_module, tgt_module, EdgeKind::Imports);
                }
            }
        }
    }

    /// Find the Module node owned by `file_id`, if one exists.
    pub fn module_node_for_file(&self, file_id: FileId) -> Option<NodeId> {
        let ids = self.by_file.get(&file_id)?;
        ids.iter().copied().find(|id| {
            self.nodes
                .get(id)
                .is_some_and(|n| matches!(n.kind, NodeKind::Module))
        })
    }

    /// Walk `source` once, collecting node ids whose name appears as a
    /// word in the file *and* who live in a different file. Each target
    /// id appears at most once.
    fn collect_cross_file_refs(&self, source: &str, this_file: FileId) -> BTreeSet<NodeId> {
        let mut out: BTreeSet<NodeId> = BTreeSet::new();
        if self.by_name.is_empty() {
            return out;
        }
        let mut word = String::with_capacity(32);
        for ch in source.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                word.push(ch);
            } else if !word.is_empty() {
                self.absorb_word_refs(&word, this_file, &mut out);
                word.clear();
            }
        }
        if !word.is_empty() {
            self.absorb_word_refs(&word, this_file, &mut out);
        }
        out
    }

    fn absorb_word_refs(&self, word: &str, this_file: FileId, bag: &mut BTreeSet<NodeId>) {
        if word.len() < 3 {
            return;
        }
        let Some(ids) = self.by_name.get(word) else {
            return;
        };
        for nid in ids {
            let same_file = self.nodes.get(nid).is_some_and(|n| n.file_id == this_file);
            if !same_file {
                bag.insert(*nid);
            }
        }
    }

    fn add_module_for_file(&mut self, file_id: FileId, path: &str, language: &Language) -> NodeId {
        let name = module_name_from_path(path);
        self.add_node(Node {
            id: 0,
            kind: NodeKind::Module,
            name,
            file_id,
            path: path.to_string(),
            line: 1,
            signature: format!("module {path}"),
            container: None,
            access_level: None,
            language: language.name().to_string(),
        })
    }

    fn add_node(&mut self, mut node: Node) -> NodeId {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).expect("NodeId overflow");
        node.id = id;
        self.by_file.entry(node.file_id).or_default().push(id);
        self.by_name.entry(node.name.clone()).or_default().push(id);
        self.nodes.insert(id, node);
        id
    }

    fn add_edge(&mut self, from: NodeId, to: NodeId, kind: EdgeKind) {
        let edge = Edge { from, to, kind };
        self.out_edges.entry(from).or_default().push(edge);
        self.in_edges.entry(to).or_default().push(edge);
    }
}

/// Derive a coarse module name from a workspace-relative path (basename
/// without extension). Used to make Module-node queries human-readable.
pub fn module_name_from_path(path: &str) -> String {
    let stem = path.rsplit_once('/').map(|(_, name)| name).unwrap_or(path);
    let base = stem.rsplit_once('.').map(|(name, _)| name).unwrap_or(stem);
    base.to_string()
}

fn map_symbol_kind(kind: SymbolKind) -> Option<NodeKind> {
    match kind {
        SymbolKind::Function | SymbolKind::Method => Some(NodeKind::Function),
        SymbolKind::Field => Some(NodeKind::Field),
        SymbolKind::EnumCase => Some(NodeKind::EnumCase),
        SymbolKind::Class
        | SymbolKind::Struct
        | SymbolKind::Enum
        | SymbolKind::Interface
        | SymbolKind::Protocol
        | SymbolKind::Type => Some(NodeKind::Type),
        SymbolKind::Module => Some(NodeKind::Module),
        SymbolKind::Variable | SymbolKind::Other => None,
    }
}

/// Sweep an already-parsed tree for `call_expression`-like nodes. The
/// set of node kinds we accept covers the major tree-sitter grammars
/// wired into `harn-hostlib`. Returns `(callee_name, 1-based line)`
/// pairs.
fn extract_call_sites_from_tree(tree: &Tree, source: &str) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    let mut cursor = tree.root_node().walk();
    let mut stack: Vec<TsNode<'_>> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if is_call_kind(node.kind()) {
            if let Some(name) = call_callee_name(node, source) {
                let line = node.start_position().row as u32 + 1;
                out.push((name, line));
            }
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

fn is_call_kind(kind: &str) -> bool {
    matches!(
        kind,
        "call_expression"
            | "call"
            | "function_call"
            | "method_invocation"
            | "method_call_expression"
            | "invocation_expression"
            | "function_call_expression"
            | "macro_invocation"
    )
}

fn call_callee_name(node: TsNode<'_>, source: &str) -> Option<String> {
    let callee = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("method"))
        .or_else(|| node.child(0u32))?;
    let text = &source[callee.start_byte()..callee.end_byte()];
    let last = text.rsplit_once(['.', ':', '!']);
    let raw = last.map(|(_, name)| name).unwrap_or(text);
    let trimmed = raw.trim();
    let plain: String = trimmed
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if plain.is_empty() {
        None
    } else {
        Some(plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_remove_round_trip() {
        let mut g = SymbolGraph::new();
        let outcome = g.rebuild_file(1, "src/a.rs", Language::Rust, "fn foo() {}\n", &[]);
        assert!(
            outcome.node_count >= 2,
            "module + function expected, got {}",
            outcome.node_count
        );
        assert!(
            outcome.symbols.iter().any(|s| s.name == "foo"),
            "rebuild_file should surface the parsed `foo` symbol"
        );
        assert!(!g.nodes_named("foo").is_empty());
        g.remove_file(1);
        assert_eq!(g.node_count(), 0);
        assert!(g.nodes_named("foo").is_empty());
    }

    #[test]
    fn rebuild_file_emits_function_module_and_call_nodes() {
        let mut g = SymbolGraph::new();
        let src = "fn alpha() {}\nfn beta() { alpha(); }\n";
        let outcome = g.rebuild_file(7, "src/x.rs", Language::Rust, src, &[]);
        assert!(
            outcome.node_count >= 3,
            "expected module + 2 functions, got {}",
            outcome.node_count
        );
        let alpha_funcs: Vec<_> = g
            .iter_nodes()
            .filter(|n| n.kind == NodeKind::Function && n.name == "alpha")
            .collect();
        assert_eq!(alpha_funcs.len(), 1);
        let beta_funcs: Vec<_> = g
            .iter_nodes()
            .filter(|n| n.kind == NodeKind::Function && n.name == "beta")
            .collect();
        assert_eq!(beta_funcs.len(), 1);
        let beta_calls: Vec<_> = g
            .iter_nodes()
            .filter(|n| n.kind == NodeKind::CallSite && n.name == "alpha")
            .collect();
        assert!(!beta_calls.is_empty(), "expected a CallSite for alpha()");
    }

    #[test]
    fn rebuild_file_emits_fields_and_enum_cases() {
        let mut g = SymbolGraph::new();
        let src = "pub struct Greeter {\n    pub name: String,\n}\n\nenum Color {\n    Red,\n}\n";
        g.rebuild_file(9, "src/lib.rs", Language::Rust, src, &[]);

        let field = g
            .iter_nodes()
            .find(|n| n.kind == NodeKind::Field && n.name == "name")
            .expect("expected public field node");
        assert_eq!(field.container.as_deref(), Some("Greeter"));
        assert_eq!(field.access_level.as_deref(), Some("public"));

        let case = g
            .iter_nodes()
            .find(|n| n.kind == NodeKind::EnumCase && n.name == "Red")
            .expect("expected enum case node");
        assert_eq!(case.container.as_deref(), Some("Color"));

        let color = g
            .iter_nodes()
            .find(|n| n.kind == NodeKind::Type && n.name == "Color")
            .expect("expected enum type node");
        assert!(
            g.outgoing(color.id)
                .iter()
                .any(|edge| edge.kind == EdgeKind::Contains && edge.to == case.id),
            "enum type should contain its case"
        );
    }

    #[test]
    fn called_by_inverse_label_resolves() {
        let (kind, reversed) = EdgeKind::parse_with_direction("CALLED_BY").unwrap();
        assert_eq!(kind, EdgeKind::Calls);
        assert!(reversed);
        let (kind, reversed) = EdgeKind::parse_with_direction("CALLS").unwrap();
        assert_eq!(kind, EdgeKind::Calls);
        assert!(!reversed);
    }

    #[test]
    fn link_imports_creates_module_to_module_edges() {
        let mut g = SymbolGraph::new();
        g.rebuild_file(
            1,
            "src/a.ts",
            Language::TypeScript,
            "import { x } from \"./b\";\n",
            &["./b".into()],
        );
        g.rebuild_file(
            2,
            "src/b.ts",
            Language::TypeScript,
            "export const x = 1;\n",
            &[],
        );
        let mut resolved: HashMap<FileId, Vec<FileId>> = HashMap::new();
        resolved.insert(1, vec![2]);
        g.link_imports(&resolved);
        let a_mod = g.module_node_for_file(1).unwrap();
        let b_mod = g.module_node_for_file(2).unwrap();
        let edge_exists = g
            .outgoing(a_mod)
            .iter()
            .any(|e| e.kind == EdgeKind::Imports && e.to == b_mod);
        assert!(edge_exists, "expected Module→Module IMPORTS edge");
    }

    #[test]
    fn link_imports_is_idempotent_across_repeated_relinks() {
        let mut g = SymbolGraph::new();
        g.rebuild_file(
            1,
            "src/a.ts",
            Language::TypeScript,
            "import { x } from \"./b\";\n",
            &["./b".into()],
        );
        g.rebuild_file(
            2,
            "src/b.ts",
            Language::TypeScript,
            "export const x = 1;\n",
            &[],
        );
        let mut resolved: HashMap<FileId, Vec<FileId>> = HashMap::new();
        resolved.insert(1, vec![2]);
        // `link_imports` re-runs over the whole workspace after every per-file
        // reindex, so relinking three times must not accumulate duplicate
        // Module→Module IMPORTS edges.
        g.link_imports(&resolved);
        g.link_imports(&resolved);
        g.link_imports(&resolved);
        let a_mod = g.module_node_for_file(1).unwrap();
        let b_mod = g.module_node_for_file(2).unwrap();
        let module_import_edges = g
            .outgoing(a_mod)
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports && e.to == b_mod)
            .count();
        assert_eq!(
            module_import_edges, 1,
            "Module→Module IMPORTS edge must not duplicate across relinks"
        );
    }
}
