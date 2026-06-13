//! Shared data types for the `ast::*` builtins.
//!
//! These mirror the JSON schemas in `crates/harn-hostlib/schemas/ast/` and
//! are the structural source of truth for the wire format. The
//! [`to_vm_value`](Symbol::to_vm_value) helpers shape each value into the
//! `VmValue::Dict` layout the schema declares so handlers in
//! [`crate::ast`] don't have to rebuild the field set in three places.

#![allow(missing_docs)]

use std::sync::Arc;

use harn_vm::VmValue;

/// Symbol kind. The wire form is the lowercase string returned by
/// [`SymbolKind::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Protocol,
    Type,
    Variable,
    Module,
    Other,
}

impl SymbolKind {
    /// Wire form ("function", "class", ...).
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::Protocol => "protocol",
            SymbolKind::Type => "type",
            SymbolKind::Variable => "variable",
            SymbolKind::Module => "module",
            SymbolKind::Other => "other",
        }
    }

    /// True for kinds that contain other symbols (classes, enums, …).
    /// Drives the flat-symbols → nested-outline fold in
    /// [`crate::ast::outline`].
    pub fn is_container(self) -> bool {
        matches!(
            self,
            SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Interface
                | SymbolKind::Protocol
                | SymbolKind::Module
        )
    }
}

/// A flat symbol record. All row/col coordinates are 0-based, matching
/// tree-sitter native positions.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub container: Option<String>,
    pub signature: String,
    pub start_row: u32,
    pub start_col: u32,
    pub end_row: u32,
    pub end_col: u32,
}

impl Symbol {
    /// Render as a `VmValue::Dict` matching `schemas/ast/symbols.response.json`.
    pub fn to_vm_value(&self) -> VmValue {
        let mut dict: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
        dict.insert(
            "name".into(),
            VmValue::String(Arc::from(self.name.as_str())),
        );
        dict.insert(
            "kind".into(),
            VmValue::String(Arc::from(self.kind.as_str())),
        );
        dict.insert(
            "container".into(),
            match &self.container {
                Some(s) => VmValue::String(Arc::from(s.as_str())),
                None => VmValue::Nil,
            },
        );
        dict.insert(
            "signature".into(),
            VmValue::String(Arc::from(self.signature.as_str())),
        );
        dict.insert("start_row".into(), VmValue::Int(self.start_row as i64));
        dict.insert("start_col".into(), VmValue::Int(self.start_col as i64));
        dict.insert("end_row".into(), VmValue::Int(self.end_row as i64));
        dict.insert("end_col".into(), VmValue::Int(self.end_col as i64));
        VmValue::dict(dict)
    }
}

/// One node in a hierarchical outline. The `children` list nests in
/// document order; see [`crate::ast::outline`] for the fold algorithm.
#[derive(Debug, Clone)]
pub struct OutlineItem {
    pub name: String,
    pub kind: SymbolKind,
    pub signature: String,
    pub start_row: u32,
    pub end_row: u32,
    pub children: Vec<OutlineItem>,
}

impl OutlineItem {
    pub fn to_vm_value(&self) -> VmValue {
        let mut dict: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
        dict.insert(
            "name".into(),
            VmValue::String(Arc::from(self.name.as_str())),
        );
        dict.insert(
            "kind".into(),
            VmValue::String(Arc::from(self.kind.as_str())),
        );
        dict.insert(
            "signature".into(),
            VmValue::String(Arc::from(self.signature.as_str())),
        );
        dict.insert("start_row".into(), VmValue::Int(self.start_row as i64));
        dict.insert("end_row".into(), VmValue::Int(self.end_row as i64));
        let kids: Vec<VmValue> = self.children.iter().map(OutlineItem::to_vm_value).collect();
        dict.insert("children".into(), VmValue::List(Arc::new(kids)));
        VmValue::dict(dict)
    }
}

/// One tree-sitter node, flattened for `parse_file`'s wire format.
/// Matches `schemas/ast/parse_file.response.json#/$defs/Node`.
#[derive(Debug, Clone)]
pub struct ParsedNode {
    pub id: u32,
    pub parent_id: Option<u32>,
    pub kind: String,
    pub is_named: bool,
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_row: u32,
    pub start_col: u32,
    pub end_row: u32,
    pub end_col: u32,
}

impl ParsedNode {
    pub fn to_vm_value(&self) -> VmValue {
        let mut dict: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
        dict.insert("id".into(), VmValue::Int(self.id as i64));
        dict.insert(
            "parent_id".into(),
            self.parent_id
                .map_or(VmValue::Nil, |id| VmValue::Int(id as i64)),
        );
        dict.insert(
            "kind".into(),
            VmValue::String(Arc::from(self.kind.as_str())),
        );
        dict.insert("is_named".into(), VmValue::Bool(self.is_named));
        dict.insert("start_byte".into(), VmValue::Int(self.start_byte as i64));
        dict.insert("end_byte".into(), VmValue::Int(self.end_byte as i64));
        dict.insert("start_row".into(), VmValue::Int(self.start_row as i64));
        dict.insert("start_col".into(), VmValue::Int(self.start_col as i64));
        dict.insert("end_row".into(), VmValue::Int(self.end_row as i64));
        dict.insert("end_col".into(), VmValue::Int(self.end_col as i64));
        VmValue::dict(dict)
    }
}

/// One ERROR / MISSING node from a tree-sitter parse. All row/column
/// coordinates are 0-based, matching tree-sitter's native Point.
///
/// `message` is a short human-readable description (e.g. `"unexpected
/// '+'"`, `"missing ')'"`). `snippet` is the raw source text covered by
/// the node, truncated to 60 chars and with newlines escaped — kept
/// separately so callers can render the message without re-parsing it.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub start_row: u32,
    pub start_col: u32,
    pub end_row: u32,
    pub end_col: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    pub message: String,
    pub snippet: String,
    pub missing: bool,
}

impl ParseError {
    pub fn to_vm_value(&self) -> VmValue {
        self.to_vm_value_with_span(false)
    }

    /// Same as [`to_vm_value`], plus a `spans_full_source` flag the caller
    /// computes from the total line count. When true, this ERROR node covers
    /// essentially the entire file — the fingerprint of a tree-sitter
    /// grammar-limitation cascade (e.g. Scala 3 indented `match`) rather than a
    /// localized, model-authored syntax mistake. Edit-validation gates use this
    /// to avoid hard-rejecting a correct edit on a grammar blind spot.
    pub fn to_vm_value_with_span(&self, spans_full_source: bool) -> VmValue {
        let mut dict: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
        dict.insert("start_row".into(), VmValue::Int(self.start_row as i64));
        dict.insert("start_col".into(), VmValue::Int(self.start_col as i64));
        dict.insert("end_row".into(), VmValue::Int(self.end_row as i64));
        dict.insert("end_col".into(), VmValue::Int(self.end_col as i64));
        dict.insert("start_byte".into(), VmValue::Int(self.start_byte as i64));
        dict.insert("end_byte".into(), VmValue::Int(self.end_byte as i64));
        dict.insert(
            "message".into(),
            VmValue::String(Arc::from(self.message.as_str())),
        );
        dict.insert(
            "snippet".into(),
            VmValue::String(Arc::from(self.snippet.as_str())),
        );
        dict.insert("missing".into(), VmValue::Bool(self.missing));
        dict.insert("spans_full_source".into(), VmValue::Bool(spans_full_source));
        VmValue::dict(dict)
    }
}

/// Reference to an identifier that wasn't defined within the current
/// file. Coordinates are 0-based. `kind` is `"identifier"` for value-side
/// references and `"type"` for type-only references (TypeScript only).
#[derive(Debug, Clone)]
pub struct UndefinedName {
    pub name: String,
    pub kind: &'static str,
    pub row: u32,
    pub column: u32,
}

impl UndefinedName {
    pub fn to_vm_value(&self) -> VmValue {
        let mut dict: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
        dict.insert(
            "name".into(),
            VmValue::String(Arc::from(self.name.as_str())),
        );
        dict.insert("kind".into(), VmValue::String(Arc::from(self.kind)));
        dict.insert("row".into(), VmValue::Int(self.row as i64));
        dict.insert("column".into(), VmValue::Int(self.column as i64));
        let message = format!("undefined name '{}'", self.name);
        dict.insert(
            "message".into(),
            VmValue::String(Arc::from(message.as_str())),
        );
        VmValue::dict(dict)
    }
}
