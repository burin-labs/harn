//! Shared machinery for the code_index refactoring builtins.
//!
//! `rename_symbol` was the first cross-file refactoring; the pieces it
//! needs are the same ones every graph-grounded refactoring needs, so they
//! live here and the builtins stay thin:
//!
//! 1. **Seed resolution** — [`resolve_seed`] pins a `symbol_ref` to one
//!    declaration node in the [`SymbolGraph`]; [`competing_declarations`]
//!    finds same-named declarations the name-based graph cannot tell apart.
//! 2. **Scope** — [`files_in_scope`] widens the seed file to every file that
//!    can mention the name.
//! 3. **Identifier spans** — [`collect_identifier_spans`] finds identifier
//!    occurrences while skipping comments and string bodies.
//! 4. **Reference sites** — [`reference_sites`] classifies each occurrence
//!    across files as a call, qualified call, method call, import, type
//!    reference or value reference.
//! 5. **All-or-nothing apply** — [`plan_file`] splices one file in memory and
//!    re-parses it; [`write_plans`] persists only after every plan passed.
//! 6. **Response envelope** — [`edit_envelope`] spells the tagged result
//!    every code_index edit builtin returns; each operation adds its own
//!    refusal tags and fields.

use std::collections::{BTreeSet, HashSet};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use harn_vm::VmValue;
use sha2::{Digest, Sha256};
use tree_sitter::Node;

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;
use crate::tools::args::{build_dict, str_value};

use super::state::IndexState;
use super::symbol_graph::{EdgeKind, Node as GraphNode, NodeId, NodeKind, SymbolGraph};

/// How far a refactoring walks from its seed. `File` is just the seed
/// file; `Module` is an alias today (one Module node per file in the
/// graph) but is kept distinct on the wire so future per-package
/// scoping doesn't require a wire break. `Workspace` follows REFS
/// edges across files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    File,
    Module,
    Workspace,
}

impl Scope {
    pub(super) fn parse(builtin: &'static str, raw: &str) -> Result<Self, HostlibError> {
        match raw {
            "file" => Ok(Self::File),
            "module" => Ok(Self::Module),
            "workspace" => Ok(Self::Workspace),
            other => Err(HostlibError::InvalidParameter {
                builtin,
                param: "scope",
                message: format!(
                    "expected one of \"file\" | \"module\" | \"workspace\", got `{other}`"
                ),
            }),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Workspace => "workspace",
        }
    }
}

/// `(path, line, kind)` of a declaration offered back to the caller when a
/// `symbol_ref` does not pin one node.
pub(super) type SeedCandidate = (String, u32, &'static str);

pub(super) enum SeedLookup {
    One(NodeId),
    None,
    Many(Vec<SeedCandidate>),
}

pub(super) fn resolve_seed(
    graph: &SymbolGraph,
    relative_path: &str,
    name: &str,
    line: Option<u32>,
    kind: Option<NodeKind>,
) -> SeedLookup {
    let candidates: Vec<&GraphNode> = graph
        .nodes_named(name)
        .iter()
        .filter_map(|id| graph.node(*id))
        .filter(|node| is_refactor_declaration(node))
        .collect();

    let mut narrowed: Vec<&GraphNode> = candidates
        .iter()
        .copied()
        .filter(|node| paths_match(&node.path, relative_path))
        .collect();

    if narrowed.is_empty() {
        // The caller may have passed a workspace-wide hint; allow seeds
        // from any file as long as the name matches and `line`/`kind`
        // can pin one down.
        narrowed = candidates;
    }

    if let Some(line) = line {
        narrowed.retain(|node| node.line == line);
    }
    if let Some(kind) = kind {
        narrowed.retain(|node| node.kind == kind);
    }

    match narrowed.len() {
        0 => SeedLookup::None,
        1 => SeedLookup::One(narrowed[0].id),
        _ => SeedLookup::Many(
            narrowed
                .iter()
                .map(|n| (n.path.clone(), n.line, n.kind.as_str()))
                .collect(),
        ),
    }
}

/// Whether a graph node declares a binding a refactoring can target.
pub(super) fn is_refactor_declaration(node: &GraphNode) -> bool {
    if node.kind == NodeKind::Module
        && (node.signature.strip_prefix("module ") == Some(node.path.as_str())
            || (node.language == "harn" && node.signature.starts_with("impl ")))
    {
        // The graph adds a synthetic module for every file and projects Harn
        // implementation blocks as modules. Neither declares a new binding.
        return false;
    }
    matches!(
        node.kind,
        NodeKind::Function
            | NodeKind::Type
            | NodeKind::Field
            | NodeKind::EnumCase
            | NodeKind::Module
    )
}

/// Other declarations named like the seed inside `in_scope_files`.
///
/// The index's REFS edges are name-based, not binding-resolved. Pinning a
/// declaration identifies the seed, but cannot establish which same-named
/// declaration each use refers to, so a non-empty result means an
/// identifier rewrite would also touch unrelated bindings.
pub(super) fn competing_declarations(
    graph: &SymbolGraph,
    seed: NodeId,
    name: &str,
    in_scope_files: &[String],
) -> Vec<SeedCandidate> {
    graph
        .nodes_named(name)
        .iter()
        .filter(|id| **id != seed)
        .filter_map(|id| graph.node(*id))
        .filter(|node| is_refactor_declaration(node) && in_scope_files.contains(&node.path))
        .map(|node| (node.path.clone(), node.line, node.kind.as_str()))
        .collect()
}

fn paths_match(a: &str, b: &str) -> bool {
    a == b
        || a.replace('\\', "/") == b.replace('\\', "/")
        || a.ends_with(&format!("/{b}"))
        || b.ends_with(&format!("/{a}"))
}

pub(super) fn files_in_scope(
    state: &IndexState,
    scope: Scope,
    name: &str,
    seed_path: &str,
    session_id: Option<&str>,
) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    seen.insert(seed_path.to_string());
    if scope == Scope::Workspace {
        for id in state.symbols.nodes_named(name) {
            if let Some(node) = state.symbols.node(*id) {
                seen.insert(node.path.clone());
            }
        }
        // Also include files whose source contains the bare word — the
        // REFS edge catches the common case but the graph only adds
        // edges for *named* nodes, missing call sites and free-text
        // uses. The textual sweep here is cheap (file-bound) and we
        // re-validate via tree-sitter in the rewrite pass. Reads route
        // through staged-fs (#1722) when a session id is supplied so
        // we observe pending writes from the same session.
        for file in state.files.values() {
            let abs = state.root.join(&file.relative_path);
            if file_contains_word(&abs, name, session_id) {
                seen.insert(file.relative_path.clone());
            }
        }
    }
    seen.into_iter().collect()
}

fn file_contains_word(path: &Path, name: &str, session_id: Option<&str>) -> bool {
    let bytes = match crate::fs::read(path, session_id) {
        Some(Ok(bytes)) => bytes,
        Some(Err(_)) => return false,
        None => match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        },
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|tok| tok == name)
}

pub(super) fn parse_kind(builtin: &'static str, raw: &str) -> Result<NodeKind, HostlibError> {
    match raw {
        "Function" => Ok(NodeKind::Function),
        "Type" => Ok(NodeKind::Type),
        "Field" => Ok(NodeKind::Field),
        "EnumCase" => Ok(NodeKind::EnumCase),
        "Module" => Ok(NodeKind::Module),
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: "symbol_ref.kind",
            message: format!(
                "expected one of [Function, Type, Field, EnumCase, Module], got `{other}`"
            ),
        }),
    }
}

pub(super) fn is_identifier_token(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Node kinds that must terminate identifier descent — strings, comments,
/// and anything else where a matching textual substring is *not* an
/// identifier reference.
fn is_skip_kind(kind: &str) -> bool {
    matches!(
        kind,
        "comment"
            | "line_comment"
            | "block_comment"
            | "doc_comment"
            | "hash_bang_line"
            | "shebang"
            | "string"
            | "string_literal"
            | "string_fragment"
            | "string_content"
            | "raw_string_literal"
            | "interpreted_string_literal"
            | "interpreted_string"
            | "char_literal"
            | "character_literal"
            | "template_string"
    )
}

/// Code embedded in a string: a Python f-string `{..}` or a TypeScript
/// template `${..}`. Identifier descent re-enters a skipped string here.
fn is_interpolation_kind(kind: &str) -> bool {
    matches!(kind, "interpolation" | "template_substitution")
}

/// Byte and 0-based row/column extent of one identifier token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct IdentifierSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

impl IdentifierSpan {
    fn of(node: Node<'_>) -> Self {
        let start = node.start_position();
        let end = node.end_position();
        Self {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_row: start.row,
            start_col: start.column,
            end_row: end.row,
            end_col: end.column,
        }
    }
}

/// An identifier that already spells the name a refactoring would introduce.
#[derive(Clone, Debug)]
pub(super) struct ShadowSite {
    pub path: String,
    pub row: usize,
    pub col: usize,
}

/// Visit every identifier-context node in `root`, in no particular order,
/// skipping comment and string bodies but not string interpolations.
fn for_each_identifier<'tree>(
    root: Node<'tree>,
    bytes: &[u8],
    identifier_kinds: &[&str],
    mut visit: impl FnMut(Node<'tree>, &str),
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if is_skip_kind(node.kind()) {
            let mut cursor = node.walk();
            stack.extend(
                node.children(&mut cursor)
                    .filter(|child| is_interpolation_kind(child.kind())),
            );
            continue;
        }
        if identifier_kinds.contains(&node.kind()) {
            if let Ok(text) = std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]) {
                visit(node, text);
            }
            // Identifier nodes are leaves — no children to recurse into.
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// Spans of every identifier spelled `target_name` go to `targets`; every
/// identifier spelled `shadow_name` goes to `shadows`. Pass an empty
/// `shadow_name` to skip shadow detection.
pub(super) fn collect_identifier_spans(
    root: Node<'_>,
    bytes: &[u8],
    target_name: &str,
    shadow_name: &str,
    identifier_kinds: &[&str],
    path: &str,
    targets: &mut Vec<IdentifierSpan>,
    shadows: &mut Vec<ShadowSite>,
) {
    for_each_identifier(root, bytes, identifier_kinds, |node, text| {
        if text == target_name {
            targets.push(IdentifierSpan::of(node));
        } else if text == shadow_name {
            let pos = node.start_position();
            shadows.push(ShadowSite {
                path: path.to_string(),
                row: pos.row,
                col: pos.column,
            });
        }
    });
}

#[derive(Clone, Debug)]
pub(super) struct EditSpan {
    pub span: IdentifierSpan,
    pub before: String,
    pub after: String,
}

/// One file's edit, staged in memory before any disk write.
pub(super) struct FilePlan {
    pub path: String,
    pub language: Language,
    pub source: String,
    pub patched: String,
    pub edits: Vec<EditSpan>,
}

/// Splice `edits` into `source` and, with `validate`, re-parse the result.
/// `Err` carries the first syntax error in the patched text.
pub(super) fn plan_file(
    path: String,
    language: Language,
    source: String,
    edits: Vec<EditSpan>,
    validate: bool,
) -> Result<FilePlan, String> {
    let patched = splice(&source, &edits);
    if validate {
        if let Some(detail) = first_syntax_error(&patched, language) {
            return Err(detail);
        }
    }
    Ok(FilePlan {
        path,
        language,
        source,
        patched,
        edits,
    })
}

pub(super) fn splice(source: &str, edits: &[EditSpan]) -> String {
    let mut ordered: Vec<&EditSpan> = edits.iter().collect();
    ordered.sort_by_key(|e| std::cmp::Reverse(e.span.start_byte));
    let mut out = source.to_string();
    for edit in ordered {
        out.replace_range(edit.span.start_byte..edit.span.end_byte, &edit.after);
    }
    out
}

/// Grammar nodes the language itself rejects although its tree-sitter
/// grammar accepts them: tree-sitter-python still parses Python 2's
/// `print` and `exec` statements, which Python 3 refuses.
fn rejected_kinds(language: Language) -> &'static [&'static str] {
    match language {
        Language::Python => &["print_statement", "exec_statement"],
        _ => &[],
    }
}

/// Refuse a file that does not parse before an edit plans anything in it.
/// Every refactoring calls this on each file it would rewrite, so it never
/// rewrites input that is already broken; `Err` is the refusal detail.
pub(super) fn parse_gate(path: &str, source: &str, language: Language) -> Result<(), String> {
    match first_syntax_error(source, language) {
        None => Ok(()),
        Some(detail) => Err(format!(
            "`{path}` does not parse before the edit ({detail}); fix it first, then retry"
        )),
    }
}

/// The first reason `source` is not valid `language`: a tree-sitter
/// ERROR/MISSING node, or a construct in [`rejected_kinds`].
pub(super) fn first_syntax_error(source: &str, language: Language) -> Option<String> {
    let tree = ast_api::parse_tree(source, language).ok()?;
    let root = tree.root_node();
    if !root.has_error() {
        return first_rejected_node(root, language);
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_missing() {
            let pos = node.start_position();
            return Some(format!(
                "missing `{}` at line {}, column {}",
                node.kind(),
                pos.row + 1,
                pos.column + 1
            ));
        }
        if node.is_error() {
            let pos = node.start_position();
            return Some(format!(
                "unexpected token at line {}, column {}",
                pos.row + 1,
                pos.column + 1
            ));
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.has_error() || child.is_missing() {
                stack.push(child);
            }
        }
    }
    Some("the source has parse errors".into())
}

fn first_rejected_node(root: Node<'_>, language: Language) -> Option<String> {
    let rejected = rejected_kinds(language);
    if rejected.is_empty() {
        return None;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if rejected.contains(&node.kind()) {
            let pos = node.start_position();
            return Some(format!(
                "`{}` is not valid {} at line {}, column {}",
                node.kind(),
                language.name(),
                pos.row + 1,
                pos.column + 1
            ));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

pub(super) fn read_source(
    builtin: &'static str,
    path: &Path,
    session_id: Option<&str>,
) -> Result<String, HostlibError> {
    let bytes = if let Some(result) = crate::fs::read(path, session_id) {
        result.map_err(|err| HostlibError::Backend {
            builtin,
            message: format!("read `{}`: {err}", path.display()),
        })?
    } else {
        std::fs::read(path).map_err(|err| HostlibError::Backend {
            builtin,
            message: format!("read `{}`: {err}", path.display()),
        })?
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) fn write_source(
    builtin: &'static str,
    path: &Path,
    contents: &str,
    session_id: Option<&str>,
) -> Result<(), String> {
    match crate::fs::stage_write_or_none(builtin, path, contents.as_bytes(), true, true, session_id)
    {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(err) => return Err(err.to_string()),
    }
    crate::fs_snapshot::auto_capture_for_write(builtin, path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("mkdir `{}`: {err}", parent.display()))?;
        }
    }
    std::fs::write(path, contents).map_err(|err| format!("write `{}`: {err}", path.display()))
}

/// Persist every plan in one pass. Call only after every plan has passed
/// pre-flight, so a clean run is all-or-nothing modulo mid-call disk
/// failures; those come back as `(path, reason)`.
pub(super) fn write_plans(
    builtin: &'static str,
    root: &Path,
    plans: &[FilePlan],
    session_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut failed = Vec::new();
    for plan in plans {
        let abs = root.join(&plan.path);
        if let Err(err) = write_source(builtin, &abs, &plan.patched, session_id) {
            failed.push((plan.path.clone(), err));
        }
    }
    failed
}

// === Reference sites ===
//
// The move/extract/change-signature builtins are the production consumers
// of this query; until they land only the golden tests call it.

/// How a reference site uses the seed symbol.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ReferenceKind {
    /// `f(...)`.
    Call,
    /// `mod::f(...)`, or `m.f(...)` where `m` is bound by an import in the
    /// same file.
    QualifiedCall,
    /// `value.f(...)` on a receiver no import binds.
    MethodCall,
    /// Inside an import or use statement, including grouped use-trees.
    Import,
    /// A type position (`let w: Widget`).
    TypeReference,
    /// Any other use: `let g = f;`, `a::f` without a call, a field read.
    ValueReference,
}

impl ReferenceKind {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::QualifiedCall => "qualified_call",
            Self::MethodCall => "method_call",
            Self::Import => "import",
            Self::TypeReference => "type_reference",
            Self::ValueReference => "value_reference",
        }
    }
}

/// One use of the seed symbol's name.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReferenceSite {
    pub path: String,
    pub language: Language,
    pub kind: ReferenceKind,
    /// The identifier token itself.
    pub span: IdentifierSpan,
    /// Source text of the qualifying path or receiver: `crate::a` in
    /// `crate::a::f()`, `m` in `m.f()`.
    pub qualifier: Option<String>,
    /// Bytes of the construct a rewrite works on: the whole call
    /// expression for calls, the import statement for imports, and the
    /// (possibly qualified) name expression otherwise.
    pub enclosing: Range<usize>,
}

/// An in-scope file the reference query could not read structurally.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SkippedFile {
    pub path: String,
    pub reason: String,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, Default)]
pub(super) struct ReferenceSites {
    /// Sorted by path, then byte offset.
    pub sites: Vec<ReferenceSite>,
    pub skipped: Vec<SkippedFile>,
}

/// Every reference to `seed` across the workspace, classified.
///
/// The file set is the seed's file, the sources of REFS and CALLS edges
/// into the seed, and every file that contains the bare name. REFS alone
/// is not enough: they are computed per file while the index is built, so
/// a file ingested before the declaring file carries none.
///
/// Like rename, matching is by name inside identifier context. Declarations
/// (the seed's own and any other of the same name) are not reference
/// sites; [`competing_declarations`] reports the latter.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn reference_sites(
    builtin: &'static str,
    state: &IndexState,
    seed: NodeId,
    session_id: Option<&str>,
) -> Result<ReferenceSites, HostlibError> {
    let Some(seed_node) = state.symbols.node(seed) else {
        return Ok(ReferenceSites::default());
    };
    let name = seed_node.name.as_str();
    let mut files: BTreeSet<String> =
        files_in_scope(state, Scope::Workspace, name, &seed_node.path, session_id)
            .into_iter()
            .collect();
    for edge in state.symbols.incoming(seed) {
        if matches!(edge.kind, EdgeKind::Refs | EdgeKind::Calls) {
            if let Some(from) = state.symbols.node(edge.from) {
                files.insert(from.path.clone());
            }
        }
    }

    let mut out = ReferenceSites::default();
    for path in files {
        let Some(language) = Language::detect(Path::new(&path), None) else {
            out.skipped.push(SkippedFile {
                path,
                reason: "no tree-sitter grammar".into(),
            });
            continue;
        };
        let Some(identifier_kinds) = language.rename_identifier_kinds() else {
            out.skipped.push(SkippedFile {
                path,
                reason: format!("no identifier-kind table for `{}`", language.name()),
            });
            continue;
        };
        let source = read_source(builtin, &state.root.join(&path), session_id)?;
        let tree = match ast_api::parse_tree(&source, language) {
            Ok(tree) => tree,
            Err(err) => {
                out.skipped.push(SkippedFile {
                    path,
                    reason: format!("parse failed: {err}"),
                });
                continue;
            }
        };
        let bytes = source.as_bytes();
        let root = tree.root_node();
        let import_bindings = import_bound_names(root, bytes, identifier_kinds);
        let mut file_sites = Vec::new();
        for_each_identifier(root, bytes, identifier_kinds, |node, text| {
            if text != name {
                return;
            }
            if let Some((kind, qualifier, enclosing)) = classify(node, bytes, &import_bindings) {
                file_sites.push(ReferenceSite {
                    path: path.clone(),
                    language,
                    kind,
                    span: IdentifierSpan::of(node),
                    qualifier,
                    enclosing,
                });
            }
        });
        file_sites.sort_by_key(|site| site.span.start_byte);
        out.sites.extend(file_sites);
    }
    Ok(out)
}

/// Statements whose identifiers are all import or use bindings.
const IMPORT_STATEMENT_KINDS: &[&str] = &[
    "use_declaration",
    "import_statement",
    "import_from_statement",
    "future_import_statement",
    "import_declaration",
];

/// Call expression kind → field holding the callee.
const CALL_KINDS: &[(&str, &str)] = &[
    ("call_expression", "function"),
    ("call", "function"),
    ("new_expression", "constructor"),
];

/// Member access kind → (receiver field, member field).
const MEMBER_KINDS: &[(&str, &str, &str)] = &[
    ("field_expression", "value", "field"),
    ("attribute", "object", "attribute"),
    ("member_expression", "object", "property"),
    ("selector_expression", "operand", "field"),
];

/// Path kinds whose `name` is qualified by a `path` (`a::b::f`).
const PATH_KINDS: &[&str] = &["scoped_identifier", "scoped_type_identifier"];

/// Declaration node kinds, by suffix, whose `name` field binds the name
/// rather than using it.
const DECLARATION_SUFFIXES: &[&str] = &[
    "_item",
    "_definition",
    "_declaration",
    "_declarator",
    "_signature",
    "_spec",
];

/// Every name an import statement in this file binds, so `m.f()` can be
/// told apart from a method call. Over-approximates: `from p import m`
/// contributes both `p` and `m`.
fn import_bound_names(root: Node<'_>, bytes: &[u8], identifier_kinds: &[&str]) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if IMPORT_STATEMENT_KINDS.contains(&node.kind()) {
            for_each_identifier(node, bytes, identifier_kinds, |_, text| {
                names.insert(text.to_string());
            });
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    names
}

fn text_of<'a>(node: Node<'_>, bytes: &'a [u8]) -> &'a str {
    std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]).unwrap_or("")
}

fn is_field_child(parent: Node<'_>, field: &str, node: Node<'_>) -> bool {
    parent.child_by_field_name(field).map(|c| c.id()) == Some(node.id())
}

fn leftmost_leaf(mut node: Node<'_>) -> Node<'_> {
    while let Some(child) = node.named_child(0) {
        node = child;
    }
    node
}

/// Classify one identifier occurrence. `None` means it declares the name.
fn classify(
    node: Node<'_>,
    bytes: &[u8],
    import_bindings: &HashSet<String>,
) -> Option<(ReferenceKind, Option<String>, Range<usize>)> {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if IMPORT_STATEMENT_KINDS.contains(&current.kind()) {
            return Some((ReferenceKind::Import, None, current.byte_range()));
        }
        ancestor = current.parent();
    }

    let parent = node.parent()?;
    if is_field_child(parent, "name", node)
        && DECLARATION_SUFFIXES
            .iter()
            .any(|suffix| parent.kind().ends_with(suffix))
    {
        return None;
    }

    // Widen the bare identifier to the expression that names the symbol.
    let (callee, qualifier, receiver_imported) = if PATH_KINDS.contains(&parent.kind())
        && is_field_child(parent, "name", node)
    {
        let path = parent.child_by_field_name("path");
        (parent, path.map(|p| text_of(p, bytes).to_string()), true)
    } else if let Some((_, receiver_field, _)) = MEMBER_KINDS
        .iter()
        .find(|(kind, _, member)| parent.kind() == *kind && is_field_child(parent, member, node))
    {
        let receiver = parent.child_by_field_name(receiver_field);
        let imported = receiver
            .map(|r| import_bindings.contains(text_of(leftmost_leaf(r), bytes)))
            .unwrap_or(false);
        (
            parent,
            receiver.map(|r| text_of(r, bytes).to_string()),
            imported,
        )
    } else {
        (node, None, false)
    };

    let call = callee.parent().filter(|outer| {
        CALL_KINDS
            .iter()
            .any(|(kind, field)| outer.kind() == *kind && is_field_child(*outer, field, callee))
    });
    let kind = match (call.is_some(), qualifier.is_some(), receiver_imported) {
        (true, false, _) => ReferenceKind::Call,
        (true, true, true) => ReferenceKind::QualifiedCall,
        (true, true, false) => ReferenceKind::MethodCall,
        (false, _, _) if node.kind() == "type_identifier" => ReferenceKind::TypeReference,
        (false, _, _) => ReferenceKind::ValueReference,
    };
    let enclosing = call.unwrap_or(callee).byte_range();
    Some((kind, qualifier, enclosing))
}

// === Response envelope ===
//
// Every code_index edit builtin answers with the envelope
// `schemas/code_index/rename_symbol.response.json` locks. An operation adds
// its own result tags (refusals) and fields through [`EditEnvelope::extra`];
// the shared fields are spelled here once.

/// The seed a response describes.
pub(super) struct EditSymbol<'a> {
    pub name: &'a str,
    /// Only `rename_symbol` has a new name.
    pub new_name: Option<&'a str>,
    pub path: &'a str,
    pub line: Option<u32>,
    pub kind: Option<NodeKind>,
}

/// One response. Fields left at their default land as empty lists, zero,
/// and `false`.
#[derive(Default)]
pub(super) struct EditEnvelope {
    pub applied: bool,
    pub dry_run: bool,
    pub touched_files: Vec<VmValue>,
    pub conflicts: Vec<VmValue>,
    /// Strings, except on `ambiguous_symbol`, where each entry is a
    /// `{path, line, kind}` candidate ([`candidates_value`]).
    pub warnings: Vec<VmValue>,
    pub failed_paths: Vec<VmValue>,
    pub match_count: usize,
    pub details: String,
    /// The text-edit degradation path, on `unsupported_language` only.
    pub fallback_suggestion: Option<String>,
    /// Operation-specific fields, appended after the shared ones.
    pub extra: Vec<(&'static str, VmValue)>,
}

pub(super) fn edit_envelope(
    tag: &'static str,
    scope: Scope,
    symbol: &EditSymbol<'_>,
    envelope: EditEnvelope,
) -> VmValue {
    let list = |items: Vec<VmValue>| VmValue::List(Arc::new(items));
    let mut symbol_entries = vec![("name", str_value(symbol.name))];
    if let Some(new_name) = symbol.new_name {
        symbol_entries.push(("new_name", str_value(new_name)));
    }
    symbol_entries.extend([
        ("path", str_value(symbol.path)),
        (
            "line",
            symbol
                .line
                .map(|n| VmValue::Int(n as i64))
                .unwrap_or(VmValue::Nil),
        ),
        (
            "kind",
            symbol
                .kind
                .map(|k| str_value(k.as_str()))
                .unwrap_or(VmValue::Nil),
        ),
    ]);
    let mut entries: Vec<(&'static str, VmValue)> = vec![
        ("result", str_value(tag)),
        ("applied", VmValue::Bool(envelope.applied)),
        ("dry_run", VmValue::Bool(envelope.dry_run)),
        ("scope", str_value(scope.as_str())),
        ("symbol", build_dict(symbol_entries)),
        ("touched_files", list(envelope.touched_files)),
        ("conflicts", list(envelope.conflicts)),
        ("warnings", list(envelope.warnings)),
        ("failed_paths_with_reasons", list(envelope.failed_paths)),
        ("match_count", VmValue::Int(envelope.match_count as i64)),
        ("details", str_value(&envelope.details)),
    ];
    if let Some(fallback) = envelope.fallback_suggestion {
        entries.push(("fallback_suggestion", str_value(fallback)));
    }
    entries.extend(envelope.extra);
    build_dict(entries)
}

/// One `touched_files` entry.
pub(super) fn file_plan_value(plan: &FilePlan) -> VmValue {
    let edits: Vec<VmValue> = plan
        .edits
        .iter()
        .map(|edit| {
            build_dict([
                ("start_byte", VmValue::Int(edit.span.start_byte as i64)),
                ("end_byte", VmValue::Int(edit.span.end_byte as i64)),
                ("start_row", VmValue::Int(edit.span.start_row as i64)),
                ("start_col", VmValue::Int(edit.span.start_col as i64)),
                ("end_row", VmValue::Int(edit.span.end_row as i64)),
                ("end_col", VmValue::Int(edit.span.end_col as i64)),
                ("before", str_value(&edit.before)),
                ("after", str_value(&edit.after)),
            ])
        })
        .collect();
    build_dict([
        ("path", str_value(&plan.path)),
        ("language", str_value(plan.language.name())),
        (
            "before_sha256",
            str_value(sha256_hex(plan.source.as_bytes())),
        ),
        (
            "after_sha256",
            str_value(sha256_hex(plan.patched.as_bytes())),
        ),
        ("edits", VmValue::List(Arc::new(edits))),
    ])
}

pub(super) fn failed_paths_value(failed: &[(String, String)]) -> Vec<VmValue> {
    failed
        .iter()
        .map(|(path, reason)| {
            build_dict([("path", str_value(path)), ("reason", str_value(reason))])
        })
        .collect()
}

/// `ambiguous_symbol` warnings: one `{path, line, kind}` per candidate.
pub(super) fn candidates_value(candidates: &[SeedCandidate]) -> Vec<VmValue> {
    candidates
        .iter()
        .map(|(path, line, kind)| {
            build_dict([
                ("path", str_value(path)),
                ("line", VmValue::Int(*line as i64)),
                ("kind", str_value(*kind)),
            ])
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
#[path = "refactor_core_tests.rs"]
mod tests;
