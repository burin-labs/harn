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

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tree_sitter::{Node as TsNode, Tree};

use crate::ast::{api as ast_api, Language, Symbol, SymbolKind};
use crate::ResolvedHarnReference;

use super::file_table::FileId;

/// Typed node identifier. Stable across `rebuild_file` calls that don't
/// touch the file (id assignment is per-file deterministic — see
/// [`SymbolGraph::rebuild_file`]).
pub type NodeId = u32;

/// Coarse typed node kinds defined in issue #2434.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// Every kind, so an exhaustiveness test can enumerate them.
    pub const ALL: [NodeKind; 8] = [
        NodeKind::Function,
        NodeKind::Type,
        NodeKind::Field,
        NodeKind::EnumCase,
        NodeKind::Module,
        NodeKind::Import,
        NodeKind::CallSite,
        NodeKind::Macro,
    ];

    /// Whether a node of this kind can be named from another file by a
    /// bare identifier, and is therefore a legitimate target for the
    /// `REFS` name heuristic.
    ///
    /// Two kinds are excluded for different reasons, and both exclusions
    /// are load-bearing:
    ///
    /// - [`NodeKind::CallSite`] is a *use*, not a declaration. Pointing a
    ///   REFS edge at one asserts that module A references a call
    ///   expression inside module B, which is not a fact anybody wants.
    ///   It is also where essentially all the edges came from: on a
    ///   7,038-file workspace, call sites absorbed 80.9M of 87.0M REFS
    ///   edges, because a name like `assert` has 32,343 call sites and
    ///   seven actual declarations.
    /// - [`NodeKind::Field`] and [`NodeKind::EnumCase`] are scoped to
    ///   their container. A module that happens to contain the word
    ///   `path` is not referencing all 469 struct fields named `path`.
    ///
    /// [`NodeKind::Import`] is excluded because its name is a raw import
    /// string, which never word-matches.
    pub fn is_name_addressable(self) -> bool {
        matches!(
            self,
            NodeKind::Function | NodeKind::Type | NodeKind::Macro | NodeKind::Module
        )
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

/// Shortest declaration name the REFS heuristic links. Shorter words
/// (`id`, `ok`) match too much to mean anything.
const MIN_REF_WORD_LEN: usize = 3;

/// Coarse typed edge kinds defined in issue #2434.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    /// CallSite → Function. A call expression resolving to a function.
    Calls,
    /// Module → any. Name-heuristic for languages without a resolver.
    /// Harn files do not emit these; they use ModuleGraph instead.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
    /// Function names whose declarations changed since the last
    /// [`Self::take_dirty_call_sites`]. Every call site with one of these
    /// names may now resolve differently.
    dirty_call_names: HashSet<String>,
    /// Files rebuilt with call resolution deferred.
    dirty_call_files: HashSet<FileId>,
}

/// Only the owning graph facts cross the snapshot boundary. Name, file, and
/// reverse-edge indexes are derived once on restore rather than stored twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GraphSnapshot {
    pub next_id: NodeId,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl SymbolGraph {
    /// Construct an empty graph.
    pub fn new() -> Self {
        Self {
            next_id: 1,
            ..Self::default()
        }
    }

    pub(super) fn snapshot(&self) -> GraphSnapshot {
        let ids = self.all_node_ids();
        let mut nodes = Vec::with_capacity(ids.len());
        let mut edges = Vec::with_capacity(self.edge_count());
        for id in ids {
            let node = &self.nodes[&id];
            nodes.push(node.clone());
            // Harn references come from the embedding host's resolver. A
            // different process may have no resolver or a different answer.
            edges.extend(self.outgoing(id).iter().copied().filter(|edge| {
                !(node.kind == NodeKind::Module
                    && node.language == "harn"
                    && edge.kind == EdgeKind::Refs)
            }));
        }
        GraphSnapshot {
            next_id: self.next_id,
            nodes,
            edges,
        }
    }

    pub(super) fn from_snapshot(snapshot: GraphSnapshot) -> Result<Self, &'static str> {
        if snapshot.next_id == 0 {
            return Err("symbol graph next id is zero");
        }
        let mut graph = Self {
            next_id: snapshot.next_id,
            ..Self::default()
        };
        for node in snapshot.nodes {
            if node.id == 0 || node.id >= graph.next_id || graph.nodes.contains_key(&node.id) {
                return Err("symbol graph has an invalid or duplicate node id");
            }
            graph.by_file.entry(node.file_id).or_default().push(node.id);
            graph
                .by_name
                .entry(node.name.clone())
                .or_default()
                .push(node.id);
            graph.nodes.insert(node.id, node);
        }
        for edge in snapshot.edges {
            if !graph.nodes.contains_key(&edge.from) || !graph.nodes.contains_key(&edge.to) {
                return Err("symbol graph edge names an absent node");
            }
            graph.out_edges.entry(edge.from).or_default().push(edge);
            graph.in_edges.entry(edge.to).or_default().push(edge);
        }
        Ok(graph)
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
        self.mark_function_names_dirty(file_id);
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
    ///
    /// `imported_files` is this file's **resolved** import set, which
    /// scopes call resolution. The caller owns import resolution and must
    /// have run it for `file_id` already;
    /// [`super::IndexState`] does, at all three sites that reach here.
    /// Pass an empty slice to mean "this file imports nothing", which is
    /// the honest answer for a language or file with no import syntax —
    /// not "resolve against everything".
    pub fn rebuild_file(
        &mut self,
        file_id: FileId,
        path: &str,
        language: Language,
        source: &str,
        import_strings: &[String],
        imported_files: &[FileId],
    ) -> RebuildOutcome {
        // A Swift target or Go package can put hundreds of files in
        // scope, so the membership test is a set lookup rather than
        // a scan of the import list per candidate per call site.
        let visible: HashSet<FileId> = imported_files.iter().copied().collect();
        self.rebuild_file_resolving(
            file_id,
            path,
            language,
            source,
            import_strings,
            Some(&visible),
        )
    }

    /// [`Self::rebuild_file`] without resolving this file's call sites.
    /// The caller resolves them with every other affected site once the
    /// whole batch is in: [`Self::take_dirty_call_sites`] then
    /// [`Self::resolve_call_sites`].
    pub(super) fn rebuild_file_deferring_calls(
        &mut self,
        file_id: FileId,
        path: &str,
        language: Language,
        source: &str,
        import_strings: &[String],
    ) -> RebuildOutcome {
        self.dirty_call_files.insert(file_id);
        self.rebuild_file_resolving(file_id, path, language, source, import_strings, None)
    }

    fn rebuild_file_resolving(
        &mut self,
        file_id: FileId,
        path: &str,
        language: Language,
        source: &str,
        import_strings: &[String],
        visible: Option<&HashSet<FileId>>,
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

        // CallSite nodes, plus CALLS edges unless resolution is deferred.
        // Resolving here sees only the declarations already in the graph.
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
                if let Some(visible) = visible {
                    let functions = self.functions_named(&callee_name);
                    for t in pick_call_targets(&functions, file_id, visible) {
                        self.add_edge(call_id, t, EdgeKind::Calls);
                    }
                }
            }
        }
        self.mark_function_names_dirty(file_id);

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

        // REFS: name-heuristic only for languages without a resolver.
        // Harn answers from ModuleGraph (`definition_of`); a word match
        // here would collapse a local `run` with an imported `run`.
        if language.name() != "harn" {
            for target in self.collect_cross_file_refs(source, file_id) {
                self.add_edge(module_id, target, EdgeKind::Refs);
            }
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

    /// Replace every resolver-backed Harn `REFS` edge with `references`.
    ///
    /// The projection is path-qualified, so same-named declarations in
    /// separate modules remain distinct. Clearing first is intentional:
    /// rebuilds, editor reindexes, and branch overlays must remove stale
    /// answers rather than accumulate them.
    pub fn replace_harn_references(&mut self, references: &[ResolvedHarnReference]) {
        let harn_modules: BTreeSet<NodeId> = self
            .nodes
            .values()
            .filter(|node| node.kind == NodeKind::Module && node.language == "harn")
            .map(|node| node.id)
            .collect();
        for module in &harn_modules {
            if let Some(edges) = self.out_edges.get_mut(module) {
                let removed: Vec<Edge> = edges
                    .iter()
                    .copied()
                    .filter(|edge| edge.kind == EdgeKind::Refs)
                    .collect();
                edges.retain(|edge| edge.kind != EdgeKind::Refs);
                for edge in removed {
                    if let Some(incoming) = self.in_edges.get_mut(&edge.to) {
                        incoming.retain(|candidate| {
                            !(candidate.from == edge.from
                                && candidate.to == edge.to
                                && candidate.kind == EdgeKind::Refs)
                        });
                    }
                }
            }
        }

        for reference in references {
            let Some(from) = self
                .nodes
                .values()
                .find(|node| {
                    node.kind == NodeKind::Module
                        && node.language == "harn"
                        && node.path == reference.from_path
                })
                .map(|node| node.id)
            else {
                continue;
            };
            let targets: Vec<NodeId> = self
                .nodes
                .values()
                .filter(|node| {
                    node.path == reference.to_path
                        && node.name == reference.to_name
                        && node.kind != NodeKind::Module
                })
                .map(|node| node.id)
                .collect();
            for target in targets {
                let duplicate = self
                    .outgoing(from)
                    .iter()
                    .any(|edge| edge.kind == EdgeKind::Refs && edge.to == target);
                if !duplicate {
                    self.add_edge(from, target, EdgeKind::Refs);
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

    /// Add the REFS edges that point *into* `file_id`'s declarations.
    ///
    /// [`Self::rebuild_file`] links a module only to declarations that
    /// already exist when it is parsed. A declaration that arrives later
    /// needs the reverse direction: a file ingested after the files that
    /// name it, or a declaring file re-parsed after an edit, which drops
    /// every edge into its old nodes. With both directions a REFS edge
    /// exists exactly when another non-Harn module's source names the
    /// declaration, whatever order the files arrived in.
    ///
    /// `files_naming(word)` answers which files contain `word` as a
    /// [`super::words::tokenize`] token, the tokenizer the forward scan
    /// uses. Repeats are fine. Idempotent.
    pub fn link_refs_into_file(
        &mut self,
        file_id: FileId,
        files_naming: impl Fn(&str) -> Vec<FileId>,
    ) {
        let Some(ids) = self.by_file.get(&file_id) else {
            return;
        };
        let targets: Vec<(NodeId, String)> = ids
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .filter(|n| n.kind.is_name_addressable() && n.name.len() >= MIN_REF_WORD_LEN)
            .map(|n| (n.id, n.name.clone()))
            .collect();
        for (target, name) in targets {
            let files: BTreeSet<FileId> = files_naming(&name).into_iter().collect();
            let mut linked: HashSet<NodeId> = self
                .incoming(target)
                .iter()
                .filter(|e| e.kind == EdgeKind::Refs)
                .map(|e| e.from)
                .collect();
            for file in files {
                if file == file_id {
                    continue;
                }
                let Some(module) = self.module_node_for_file(file) else {
                    continue;
                };
                if self
                    .nodes
                    .get(&module)
                    .is_some_and(|m| m.language == "harn")
                {
                    continue;
                }
                if linked.insert(module) {
                    self.add_edge(module, target, EdgeKind::Refs);
                }
            }
        }
    }

    /// Collect the ids of declarations in other files whose name is a
    /// token of `source`. Each target id appears at most once.
    fn collect_cross_file_refs(&self, source: &str, this_file: FileId) -> BTreeSet<NodeId> {
        let mut out: BTreeSet<NodeId> = BTreeSet::new();
        if self.by_name.is_empty() {
            return out;
        }
        super::words::tokenize(source, |word| {
            self.absorb_word_refs(word, this_file, &mut out);
        });
        out
    }

    /// Every function declaration named `name`, with its file.
    fn functions_named(&self, name: &str) -> Vec<(NodeId, FileId)> {
        self.nodes_named(name)
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| (n.id, n.file_id))
            .collect()
    }

    fn mark_function_names_dirty(&mut self, file_id: FileId) {
        let Some(ids) = self.by_file.get(&file_id) else {
            return;
        };
        for id in ids {
            if let Some(node) = self.nodes.get(id) {
                if node.kind == NodeKind::Function {
                    self.dirty_call_names.insert(node.name.clone());
                }
            }
        }
    }

    /// Drain the dirty set into the call sites that need resolving: every
    /// site in a file rebuilt with deferred resolution, and every site
    /// whose callee name gained or lost a declaration. Sorted, unique.
    ///
    /// Resolution reads the whole workspace's declarations, so a site
    /// resolved before a later file arrived can be wrong in either
    /// direction: it missed a declaration that now exists, or it linked
    /// a name that was unique then and is ambiguous now.
    pub fn take_dirty_call_sites(&mut self) -> Vec<NodeId> {
        let names = std::mem::take(&mut self.dirty_call_names);
        let files = std::mem::take(&mut self.dirty_call_files);
        let mut sites: BTreeSet<NodeId> = BTreeSet::new();
        for file in files {
            for id in self.by_file.get(&file).map(Vec::as_slice).unwrap_or(&[]) {
                if self
                    .nodes
                    .get(id)
                    .is_some_and(|n| n.kind == NodeKind::CallSite)
                {
                    sites.insert(*id);
                }
            }
        }
        for name in names {
            for id in self.nodes_named(&name) {
                if self
                    .nodes
                    .get(id)
                    .is_some_and(|n| n.kind == NodeKind::CallSite)
                {
                    sites.insert(*id);
                }
            }
        }
        sites.into_iter().collect()
    }

    /// Replace the CALLS edges of each call site in `sites`. `visible`
    /// maps a call site's file to its resolved import set; a file with no
    /// entry imports nothing.
    pub fn resolve_call_sites(
        &mut self,
        sites: &[NodeId],
        visible: &HashMap<FileId, HashSet<FileId>>,
    ) {
        let empty = HashSet::new();
        let mut functions_by_name: HashMap<String, Vec<(NodeId, FileId)>> = HashMap::new();
        for site in sites {
            let Some((name, file_id)) = self
                .nodes
                .get(site)
                .filter(|n| n.kind == NodeKind::CallSite)
                .map(|n| (n.name.clone(), n.file_id))
            else {
                continue;
            };
            self.drop_out_edges(*site, EdgeKind::Calls);
            let functions = functions_by_name
                .entry(name)
                .or_insert_with_key(|name| self.functions_named(name));
            let targets =
                pick_call_targets(functions, file_id, visible.get(&file_id).unwrap_or(&empty));
            for target in targets {
                self.add_edge(*site, target, EdgeKind::Calls);
            }
        }
    }

    fn drop_out_edges(&mut self, from: NodeId, kind: EdgeKind) {
        let Some(edges) = self.out_edges.get_mut(&from) else {
            return;
        };
        let mut dropped = Vec::new();
        edges.retain(|edge| {
            let keep = edge.kind != kind;
            if !keep {
                dropped.push(edge.to);
            }
            keep
        });
        for to in dropped {
            if let Some(bucket) = self.in_edges.get_mut(&to) {
                bucket.retain(|edge| !(edge.from == from && edge.kind == kind));
            }
        }
    }

    /// Add every cross-file declaration named `word` to `bag`.
    ///
    /// `by_name` indexes every node, including call sites, because the
    /// CALLS resolver and the Cypher executor both need that. The REFS
    /// heuristic is narrower: it wants declarations a bare identifier in
    /// another file could actually be naming, so it filters to
    /// [`NodeKind::is_name_addressable`].
    fn absorb_word_refs(&self, word: &str, this_file: FileId, bag: &mut BTreeSet<NodeId>) {
        if word.len() < MIN_REF_WORD_LEN {
            return;
        }
        let Some(ids) = self.by_name.get(word) else {
            return;
        };
        for nid in ids {
            let Some(node) = self.nodes.get(nid) else {
                continue;
            };
            if node.file_id == this_file || !node.kind.is_name_addressable() {
                continue;
            }
            bag.insert(*nid);
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

/// Which functions a call to `callee_name` in `file_id` can be
/// reaching, in order of confidence.
///
/// The old rule was "every function with this name, anywhere". On a
/// 7,038-file workspace that made a call to `assert` link to all
/// seven unrelated functions of that name, so six of every seven
/// edges were wrong and one function node collected 32,887 callers
/// (#8107).
///
/// The replacement never guesses between candidates it cannot
/// distinguish:
///
/// 1. **Same file.** A local definition shadows anything imported,
///    so if the file defines the name itself, that is the call.
/// 2. **Imported files.** Otherwise the call can only reach what
///    this file actually imports. All matching declarations in the
///    resolved import set are returned, because a genuine ambiguity
///    across two imports is a fact about the code, not a guess.
/// 3. **Exactly one declaration workspace-wide.** A unique name is
///    unambiguous whether or not the import resolver saw the edge,
///    which keeps recall for implicit visibility — a sibling module
///    in the same crate, a global, a language with no import syntax.
/// 4. **Otherwise, nothing.** Several candidates and no import
///    linking any of them is precisely the case where an edge would
///    be invented rather than found.
fn pick_call_targets(
    functions: &[(NodeId, FileId)],
    file_id: FileId,
    visible: &HashSet<FileId>,
) -> Vec<NodeId> {
    let local: Vec<NodeId> = functions
        .iter()
        .filter(|(_, file)| *file == file_id)
        .map(|(id, _)| *id)
        .collect();
    if !local.is_empty() {
        return local;
    }
    let imported: Vec<NodeId> = functions
        .iter()
        .filter(|(_, file)| visible.contains(file))
        .map(|(id, _)| *id)
        .collect();
    if !imported.is_empty() {
        return imported;
    }
    if functions.len() == 1 {
        return vec![functions[0].0];
    }
    Vec::new()
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
    #[expect(
        clippy::string_slice,
        reason = "tree-sitter node byte ranges lie on char boundaries of the parsed source"
    )]
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
#[path = "symbol_graph_tests.rs"]
mod tests;
