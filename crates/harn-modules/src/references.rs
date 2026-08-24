//! Inverse of [`super::ModuleGraph::definition_of`].
//!
//! The graph already records where a name is defined. This module walks
//! retained ASTs and records every use that resolves to that same
//! [`super::DefSite`], so find-references and `harn graph` answer from
//! resolution rather than a bare-string match.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use harn_lexer::Span;
use harn_parser::{visit::walk_program_interpolated, Node};

use super::{DefSite, ModuleGraph, ParsedModuleSource};

/// One use of a name that resolved to a [`DefSite`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSite {
    pub file: PathBuf,
    pub span: Span,
    pub name: String,
}

/// Resolution-backed reference index for a built module graph.
///
/// Built from the same graph that answers go-to-definition. Two same-named
/// symbols in different modules stay distinct because each use is keyed by
/// the `DefSite` it resolved to, not by the identifier text.
#[derive(Debug, Clone, Default)]
pub struct ReferenceIndex {
    /// Every file whose AST was walked. A consumer can tell whether the
    /// answer covers the tree it asked about.
    pub files: Vec<PathBuf>,
    /// True when at least one walked file came from an unsaved buffer
    /// rather than disk. The LSP has those; `harn graph` does not.
    pub has_unsaved_buffers: bool,
    by_def: HashMap<DefKey, Vec<RefSite>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DefKey {
    file: PathBuf,
    name: String,
    start: usize,
    end: usize,
}

impl DefKey {
    fn from_def(def: &DefSite) -> Self {
        Self {
            file: def.file.clone(),
            name: def.name.clone(),
            start: def.span.start,
            end: def.span.end,
        }
    }
}

impl ReferenceIndex {
    /// Uses that resolve to `def`, including the definition site itself.
    pub fn references_to(&self, def: &DefSite) -> Vec<RefSite> {
        self.by_def
            .get(&DefKey::from_def(def))
            .cloned()
            .unwrap_or_default()
    }

    /// Every resolved use → definition edge, sorted for deterministic tests
    /// and `harn graph --json`.
    pub fn edges(&self) -> Vec<ReferenceEdge> {
        let mut edges = Vec::new();
        for (key, refs) in &self.by_def {
            for site in refs {
                edges.push(ReferenceEdge {
                    from: site.clone(),
                    to_file: key.file.clone(),
                    to_name: key.name.clone(),
                });
            }
        }
        edges.sort_by(|left, right| {
            left.from
                .file
                .cmp(&right.from.file)
                .then_with(|| left.from.span.start.cmp(&right.from.span.start))
                .then_with(|| left.to_file.cmp(&right.to_file))
                .then_with(|| left.to_name.cmp(&right.to_name))
        });
        edges
    }
}

/// One resolution-backed reference edge: a use site and the definition it
/// resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceEdge {
    pub from: RefSite,
    pub to_file: PathBuf,
    pub to_name: String,
}

/// Walk `sources` and record every identifier that [`ModuleGraph::definition_of`]
/// can resolve.
///
/// `unsaved` names files whose text came from an editor buffer. The index
/// reports that so a consumer knows whether it is looking at the file it
/// just edited.
pub fn index_references(
    graph: &ModuleGraph,
    sources: &HashMap<PathBuf, ParsedModuleSource>,
    unsaved: &HashSet<PathBuf>,
) -> ReferenceIndex {
    let mut files: Vec<PathBuf> = sources.keys().cloned().collect();
    files.sort();
    let has_unsaved_buffers = files.iter().any(|file| unsaved.contains(file));
    let mut index = ReferenceIndex {
        files,
        has_unsaved_buffers,
        by_def: HashMap::new(),
    };

    for (file, parsed) in sources {
        walk_program_interpolated(&parsed.source, &parsed.program, &mut |node| {
            for (name, span) in name_uses(node) {
                if let Some(def) = graph.definition_of(file, name) {
                    index
                        .by_def
                        .entry(DefKey::from_def(&def))
                        .or_default()
                        .push(RefSite {
                            file: file.clone(),
                            span,
                            name: name.to_string(),
                        });
                }
            }
        });
    }

    for refs in index.by_def.values_mut() {
        refs.sort_by(|left, right| {
            left.file
                .cmp(&right.file)
                .then_with(|| left.span.start.cmp(&right.span.start))
        });
        refs.dedup_by(|left, right| {
            left.file == right.file
                && left.span.start == right.span.start
                && left.name == right.name
        });
    }

    index
}

fn name_uses(node: &harn_parser::SNode) -> Vec<(&str, Span)> {
    match &node.node {
        Node::Identifier(name) => vec![(name.as_str(), node.span)],
        Node::FunctionCall { name, .. } => vec![(name.as_str(), node.span)],
        Node::FnDecl { name, .. }
        | Node::Pipeline { name, .. }
        | Node::ToolDecl { name, .. }
        | Node::SkillDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::EnumDecl { name, .. }
        | Node::InterfaceDecl { name, .. }
        | Node::TypeDecl { name, .. }
        | Node::OverrideDecl { name, .. } => vec![(name.as_str(), node.span)],
        Node::EvalPackDecl { binding_name, .. } => vec![(binding_name.as_str(), node.span)],
        _ => Vec::new(),
    }
}
