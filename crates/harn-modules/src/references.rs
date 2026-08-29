//! Inverse of [`super::ModuleGraph::definition_of`].
//!
//! The graph already records where a name is defined. This module walks
//! retained ASTs and records every use that resolves to that same
//! [`super::DefSite`], so find-references and `harn graph` answer from
//! resolution rather than a bare-string match.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use harn_lexer::{Lexer, Span, Token, TokenKind};
use harn_parser::{
    lexical::{resolved_identifier_bindings, BindingId},
    visit::walk_program_interpolated,
    Node,
};

use super::{DefKind, DefSite, ModuleGraph, ParsedModuleSource};

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
    definitions: HashMap<DefKey, DefSite>,
    site_defs: Vec<(RefSite, DefKey)>,
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
    /// Definition resolved at one identifier token.
    ///
    /// This positional lookup is required for lexical shadowing: a file and
    /// name alone cannot distinguish a local binding from an imported symbol.
    pub fn definition_at(
        &self,
        file: &std::path::Path,
        name: &str,
        offset: usize,
    ) -> Option<DefSite> {
        let file = super::canonical_path(file);
        self.site_defs
            .iter()
            .filter(|(site, _)| {
                site.file == file
                    && site.name == name
                    && offset >= site.span.start
                    && offset <= site.span.end
            })
            .min_by_key(|(site, _)| site.span.end.saturating_sub(site.span.start))
            .and_then(|(_, key)| self.definitions.get(key))
            .cloned()
    }

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
                    to_line: self
                        .definitions
                        .get(key)
                        .map(|definition| definition.span.line)
                        .unwrap_or(1),
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
    /// One-based declaration line. Together with file and name this
    /// preserves the resolved definition identity for graph projections.
    pub to_line: usize,
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
        definitions: HashMap::new(),
        site_defs: Vec::new(),
    };

    for (file, parsed) in sources {
        let lexical = resolved_identifier_bindings(&[], &parsed.program);
        let tokens = Lexer::new(&parsed.source)
            .tokenize()
            .expect("a retained parsed source must still lex");

        // A local declaration may not otherwise appear as an identifier AST
        // node. Seed every binding reached by a use so include-declaration and
        // a cursor on the declaration resolve to the same stable identity.
        let mut lexical_defs: HashMap<BindingId, DefSite> = HashMap::new();
        for binding in lexical.values() {
            lexical_defs.entry(binding.clone()).or_insert_with(|| {
                graph
                    .definition_of(file, &binding.name)
                    .filter(|def| {
                        def.file == *file
                            && def.span.start == binding.declaration_start
                            && def.span.end == binding.declaration_end
                    })
                    .unwrap_or_else(|| DefSite {
                        name: binding.name.clone(),
                        file: file.clone(),
                        kind: DefKind::Variable,
                        span: Span::with_offsets(
                            binding.declaration_start,
                            binding.declaration_end,
                            1,
                            1,
                        ),
                    })
            });
        }
        for (binding, def) in &lexical_defs {
            if let Some(span) = identifier_span(
                &tokens,
                &binding.name,
                binding.declaration_start,
                binding.declaration_end,
            ) {
                insert_reference(
                    &mut index,
                    def.clone(),
                    RefSite {
                        file: file.clone(),
                        span,
                        name: binding.name.clone(),
                    },
                );
            }
        }

        walk_program_interpolated(&parsed.source, &parsed.program, &mut |node| {
            for (name, broad_span) in name_uses(node) {
                let def = lexical
                    .get(&(broad_span.start, broad_span.end))
                    .and_then(|binding| lexical_defs.get(binding).cloned())
                    .or_else(|| graph.definition_of(file, name));
                if let (Some(def), Some(span)) = (
                    def,
                    identifier_span(&tokens, name, broad_span.start, broad_span.end),
                ) {
                    insert_reference(
                        &mut index,
                        def,
                        RefSite {
                            file: file.clone(),
                            span,
                            name: name.to_string(),
                        },
                    );
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
    index.site_defs.sort_by(|(left, _), (right, _)| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.span.start.cmp(&right.span.start))
            .then_with(|| left.name.cmp(&right.name))
    });
    index
        .site_defs
        .dedup_by(|(left, left_key), (right, right_key)| {
            left.file == right.file
                && left.span == right.span
                && left.name == right.name
                && left_key == right_key
        });

    index
}

fn insert_reference(index: &mut ReferenceIndex, def: DefSite, site: RefSite) {
    let key = DefKey::from_def(&def);
    index.definitions.entry(key.clone()).or_insert(def);
    index
        .by_def
        .entry(key.clone())
        .or_default()
        .push(site.clone());
    index.site_defs.push((site, key));
}

fn identifier_span(tokens: &[Token], name: &str, start: usize, end: usize) -> Option<Span> {
    tokens.iter().find_map(|token| match &token.kind {
        TokenKind::Identifier(found)
            if found == name && token.span.start >= start && token.span.end <= end =>
        {
            Some(token.span)
        }
        _ => None,
    })
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
