//! Cross-language symbol-extraction helpers.
//!
//! Every per-language extractor in [`super`] reuses the same primitives
//! (named-tree walk, container tracking, named-declaration shaping,
//! function-signature shaping). Centralizing them here keeps the
//! per-language match arms small and prevents the inevitable copy-paste
//! drift that bit the Swift port.

use tree_sitter::Node;

use super::super::types::{Symbol, SymbolKind};

/// Position quadruple used by helpers that build [`Symbol`]s. We pull
/// these out once per node so the helper signatures stay short.
#[derive(Debug, Clone, Copy)]
pub(super) struct NodePos {
    pub start_row: u32,
    pub start_col: u32,
    pub end_row: u32,
    pub end_col: u32,
}

/// Iterate every child (named + anonymous) of `node` in source order.
/// Wraps the tree-sitter `child(i: u32)` API so per-language extractors
/// can write `for child in children(node)` without dealing with the index
/// cast.
pub(super) fn children<'tree>(node: Node<'tree>) -> impl Iterator<Item = Node<'tree>> {
    let count = node.child_count();
    (0..count).filter_map(move |i| node.child(i as u32))
}

pub(super) fn point_pos(node: Node<'_>) -> NodePos {
    let s = node.start_position();
    let e = node.end_position();
    NodePos {
        start_row: s.row as u32,
        start_col: s.column as u32,
        end_row: e.row as u32,
        end_col: e.column as u32,
    }
}

/// UTF-8 source slice for a node. Tree-sitter byte ranges are guaranteed
/// to land on UTF-8 boundaries, so the slice is always valid.
pub(super) fn node_text(node: Node<'_>, source: &str) -> String {
    let bytes = source.as_bytes();
    let start = node.start_byte().min(bytes.len());
    let end = node.end_byte().min(bytes.len());
    std::str::from_utf8(&bytes[start..end])
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Convenience: read a field by name and return its text. Returns `None`
/// when the field isn't set on the node.
pub(super) fn field_text(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| node_text(n, source))
}

/// Truncate to `max` chars with an ellipsis suffix, matching the Swift
/// `truncate(_, max:)` helper. Operates on Unicode scalars, not bytes.
pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

/// Depth-first named-tree walk. The visitor receives each named node and
/// the current container name; it may return a new container string to
/// stamp on this node's descendants.
///
/// This is the Rust equivalent of Swift's `walkTree(node:source:container:visitor:)`
/// in `TreeSitterIntegration.swift`.
pub(super) fn walk_named<'tree, F>(node: Node<'tree>, container: Option<&str>, visitor: &mut F)
where
    F: FnMut(Node<'tree>, Option<&str>) -> Option<String>,
{
    let new_container = visitor(node, container);
    let effective: Option<&str> = match new_container.as_deref() {
        Some(s) => Some(s),
        None => container,
    };
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.is_named() {
                walk_named(child, effective, visitor);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// True iff the node has a non-named (anonymous) child whose source text
/// equals `text`. Used by Kotlin's `interface` keyword detection and
/// Scala's `case class` detection — both grammars expose those keywords
/// only as anonymous tokens, not via field names.
pub(super) fn has_anonymous_child(node: Node<'_>, text: &str, source: &str) -> bool {
    for child in children(node) {
        if !child.is_named() && node_text(child, source) == text {
            return true;
        }
    }
    false
}

/// Shape a [`Symbol`]. Centralizes the field set so per-language code
/// doesn't have to remember the field names.
pub(super) fn sym(
    name: &str,
    kind: SymbolKind,
    container: Option<&str>,
    signature: String,
    pos: NodePos,
) -> Symbol {
    Symbol {
        name: name.to_string(),
        kind,
        container: container.map(str::to_string),
        signature,
        start_row: pos.start_row,
        start_col: pos.start_col,
        end_row: pos.end_row,
        end_col: pos.end_col,
    }
}

// ---------------------------------------------------------------------------
// Common shapes used across multiple languages
// ---------------------------------------------------------------------------

/// Args for the `extractNamedDecl`-equivalent helper. Bundled into a
/// struct because Rust's function-arg limit reads better than 7 args
/// inline at every call site.
pub(super) struct NamedDeclArgs<'src, 'tree, 'out> {
    pub node: Node<'tree>,
    pub source: &'src str,
    pub container: Option<&'src str>,
    pub pos: NodePos,
    pub kind: SymbolKind,
    pub keyword: &'static str,
    pub out: &'out mut Vec<Symbol>,
}

/// Push a "named declaration" symbol. Returns the symbol's name so the
/// caller can stamp it as a container on descendants. Mirrors
/// `extractNamedDecl(node:ctx:kind:keyword:)` in `TreeSitterIntegration.swift`.
pub(super) fn named_decl_with_keyword(args: NamedDeclArgs<'_, '_, '_>) -> Option<String> {
    let name_node = args.node.child_by_field_name("name")?;
    let name = node_text(name_node, args.source);
    if name.is_empty() {
        return None;
    }
    args.out.push(sym(
        &name,
        args.kind,
        args.container,
        format!("{} {name}", args.keyword),
        args.pos,
    ));
    Some(name)
}

/// Args for the `extractFuncDecl`-equivalent helper.
pub(super) struct PushFuncArgs<'src, 'tree, 'out> {
    pub node: Node<'tree>,
    pub source: &'src str,
    pub container: Option<&'src str>,
    pub pos: NodePos,
    pub kind: SymbolKind,
    pub prefix: &'static str,
    pub out: &'out mut Vec<Symbol>,
}

/// Push a function-style symbol. Mirrors `extractFuncDecl` in
/// `TreeSitterIntegration.swift`.
pub(super) fn push_func(args: PushFuncArgs<'_, '_, '_>) {
    let Some(name_node) = args.node.child_by_field_name("name") else {
        return;
    };
    let name = node_text(name_node, args.source);
    let params = field_text(args.node, "parameters", args.source).unwrap_or_else(|| "()".into());
    let sig = if args.prefix.is_empty() {
        format!("{name}{}", truncate(&params, 80))
    } else {
        format!("{} {name}{}", args.prefix, truncate(&params, 80))
    };
    args.out
        .push(sym(&name, args.kind, args.container, sig, args.pos));
}
