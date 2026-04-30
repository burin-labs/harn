//! Completion, including dot-access completions and discriminator-value
//! suggestions for tagged-shape-union match arms.

use harn_parser::{format_type, ShapeField, TypeExpr};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::constants::{BUILTINS, DICT_METHODS, KEYWORDS, LIST_METHODS, STRING_METHODS};
use crate::helpers::{
    char_before_position, infer_dot_receiver_name, infer_dot_receiver_type, lsp_position_to_offset,
    position_in_span,
};
use crate::symbols::{EnumVariantInfo, HarnSymbolKind, SymbolInfo};
use crate::HarnLsp;

impl HarnLsp {
    pub(super) async fn handle_completion(
        &self,
        params: CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };

        let source = state.source.clone();
        let symbols = state.symbols.clone();
        let ast = state.cached_ast.clone();
        drop(docs);

        let mut items = Vec::new();

        if char_before_position(&source, position) == Some('.') {
            return Ok(Some(CompletionResponse::Array(dot_completion_items(
                &source, position, &symbols,
            ))));
        }

        // Discriminator-value completion: when the cursor sits inside
        // a `match obj.<tag> { … }` arm position and `obj` resolves to
        // a tagged shape union, offer each distinct discriminator
        // literal as a completion item. This fires only when the
        // match's value expression is the common `ident.prop` form;
        // more complex matched expressions fall through to the normal
        // identifier/builtin/keyword list.
        if let Some(ast) = ast.as_ref() {
            if let Some(discriminator_items) =
                discriminator_value_completions(ast, &source, position, &symbols)
            {
                if !discriminator_items.is_empty() {
                    return Ok(Some(CompletionResponse::Array(discriminator_items)));
                }
            }
        }

        // Symbol is visible iff it's top-level (no scope_span) or the cursor
        // sits inside its scope_span.
        for sym in &symbols {
            let visible = match sym.scope_span {
                None => true,
                Some(ref scope) => position_in_span(&position, scope, &source),
            };
            if !visible {
                continue;
            }
            let (kind, detail) = match sym.kind {
                HarnSymbolKind::Pipeline => (CompletionItemKind::FUNCTION, "pipeline"),
                HarnSymbolKind::Function => (CompletionItemKind::FUNCTION, "function"),
                HarnSymbolKind::Variable => (CompletionItemKind::VARIABLE, "variable"),
                HarnSymbolKind::Parameter => (CompletionItemKind::VARIABLE, "parameter"),
                HarnSymbolKind::Enum => (CompletionItemKind::ENUM, "enum"),
                HarnSymbolKind::Struct => (CompletionItemKind::STRUCT, "struct"),
                HarnSymbolKind::Interface => (CompletionItemKind::INTERFACE, "interface"),
            };
            items.push(CompletionItem {
                label: sym.name.clone(),
                kind: Some(kind),
                detail: Some(sym.signature.as_deref().unwrap_or(detail).to_string()),
                ..Default::default()
            });
        }

        for &(name, detail) in BUILTINS {
            items.push(CompletionItem {
                label: name.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(detail.to_string()),
                ..Default::default()
            });
        }

        for kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }

        Ok(Some(CompletionResponse::Array(items)))
    }
}

pub(super) fn dot_completion_items(
    source: &str,
    position: Position,
    symbols: &[SymbolInfo],
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let receiver_type = infer_dot_receiver_type(source, position, symbols);
    let receiver_name = infer_dot_receiver_name(source, position);

    if let Some(receiver_type) = receiver_type.as_ref() {
        match receiver_type {
            TypeExpr::Shape(fields) => {
                push_field_items(&mut items, fields);
            }
            TypeExpr::Named(name) if name == "string" => {
                push_method_items(&mut items, STRING_METHODS);
            }
            TypeExpr::Named(name) if name == "list" => {
                push_method_items(&mut items, LIST_METHODS);
            }
            TypeExpr::Named(name) if name == "dict" => {
                push_method_items(&mut items, DICT_METHODS);
            }
            TypeExpr::Named(name) => {
                if let Some(fields) = struct_fields(symbols, name) {
                    push_field_items(&mut items, &fields);
                    push_impl_method_items(&mut items, symbols, name);
                } else if let Some(variants) = enum_variants(symbols, name) {
                    if receiver_name.as_deref() == Some(name) {
                        push_enum_variant_items(&mut items, &variants);
                    } else {
                        items.push(CompletionItem {
                            label: "variant".to_string(),
                            kind: Some(CompletionItemKind::FIELD),
                            detail: Some("string".to_string()),
                            ..Default::default()
                        });
                        items.push(CompletionItem {
                            label: "fields".to_string(),
                            kind: Some(CompletionItemKind::FIELD),
                            detail: Some("list<any>".to_string()),
                            ..Default::default()
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if items.is_empty() {
        push_method_items(&mut items, STRING_METHODS);
        push_method_items(&mut items, LIST_METHODS);
        push_method_items(&mut items, DICT_METHODS);
    }

    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label && a.kind == b.kind);
    items
}

fn push_method_items(items: &mut Vec<CompletionItem>, methods: &[&str]) {
    for method in methods {
        items.push(CompletionItem {
            label: method.to_string(),
            kind: Some(CompletionItemKind::METHOD),
            ..Default::default()
        });
    }
}

fn push_field_items(items: &mut Vec<CompletionItem>, fields: &[ShapeField]) {
    for field in fields {
        items.push(CompletionItem {
            label: field.name.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(format_type(&field.type_expr)),
            ..Default::default()
        });
    }
}

fn push_enum_variant_items(items: &mut Vec<CompletionItem>, variants: &[EnumVariantInfo]) {
    for variant in variants {
        let detail = if variant.fields.is_empty() {
            "enum variant".to_string()
        } else {
            let fields = variant
                .fields
                .iter()
                .map(|field| format!("{}: {}", field.name, format_type(&field.type_expr)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("enum variant ({fields})")
        };
        items.push(CompletionItem {
            label: variant.name.clone(),
            kind: Some(CompletionItemKind::ENUM_MEMBER),
            detail: Some(detail),
            ..Default::default()
        });
    }
}

fn push_impl_method_items(
    items: &mut Vec<CompletionItem>,
    symbols: &[SymbolInfo],
    type_name: &str,
) {
    for sym in symbols.iter().filter(|sym| {
        sym.kind == HarnSymbolKind::Function && sym.impl_type.as_deref() == Some(type_name)
    }) {
        items.push(CompletionItem {
            label: sym.name.clone(),
            kind: Some(CompletionItemKind::METHOD),
            detail: sym.signature.clone(),
            ..Default::default()
        });
    }
}

fn struct_fields(symbols: &[SymbolInfo], type_name: &str) -> Option<Vec<ShapeField>> {
    symbols
        .iter()
        .find(|sym| {
            sym.kind == HarnSymbolKind::Struct && sym.name == type_name && !sym.fields.is_empty()
        })
        .map(|sym| sym.fields.clone())
}

fn enum_variants(symbols: &[SymbolInfo], type_name: &str) -> Option<Vec<EnumVariantInfo>> {
    symbols
        .iter()
        .find(|sym| {
            sym.kind == HarnSymbolKind::Enum
                && sym.name == type_name
                && !sym.enum_variants.is_empty()
        })
        .map(|sym| sym.enum_variants.clone())
}

/// Collect discriminator-value completion items for a cursor inside
/// a `match obj.<tag> { … }` block. Returns `None` when the cursor
/// isn't inside such a match; returns an empty `Vec` when the match
/// is found but `obj` isn't a recognised tagged shape union (the
/// caller then falls through to the normal completion list).
pub(super) fn discriminator_value_completions(
    ast: &[harn_parser::SNode],
    source: &str,
    position: Position,
    symbols: &[SymbolInfo],
) -> Option<Vec<CompletionItem>> {
    let cursor_offset = lsp_position_to_offset(source, position);
    let match_node = find_innermost_match_at(ast, cursor_offset)?;
    let harn_parser::Node::MatchExpr { value, arms } = &match_node.node else {
        return None;
    };
    // Only offer discriminator completions when the cursor is inside
    // the match's body region — i.e. after the opening `{` — not when
    // editing the matched expression itself.
    if cursor_offset <= value.span.end {
        return None;
    }
    // Skip when the cursor sits inside an existing arm's body block;
    // we only want to suggest literals at arm-pattern position.
    for arm in arms {
        let body_span_start = arm.pattern.span.end;
        let body_span_end = arm
            .body
            .last()
            .map(|b| b.span.end)
            .unwrap_or(arm.pattern.span.end);
        if cursor_offset > body_span_start && cursor_offset < body_span_end {
            return None;
        }
    }
    let harn_parser::Node::PropertyAccess { object, property } = &value.node else {
        return Some(Vec::new());
    };
    let harn_parser::Node::Identifier(obj_name) = &object.node else {
        return Some(Vec::new());
    };
    // Find the object's type from the symbol table. Function params
    // store their declared type verbatim, so `m: Msg` yields
    // `TypeExpr::Named("Msg")` — we then resolve one level of alias
    // by walking the AST for `type Msg = ...` declarations.
    let sym = symbols.iter().find(|s| s.name == *obj_name)?;
    let ty = sym.type_info.as_ref()?;
    let resolved = resolve_type_alias_from_ast(ty, ast);
    let TypeExpr::Union(members) = resolved else {
        return Some(Vec::new());
    };
    let mut already_covered: Vec<String> = Vec::new();
    for arm in arms {
        collect_literal_alternatives(&arm.pattern, &mut already_covered);
    }
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for member in members {
        let TypeExpr::Shape(fields) = member else {
            continue;
        };
        let Some(field) = fields.iter().find(|f| f.name == *property) else {
            continue;
        };
        let (label, insert) = match &field.type_expr {
            TypeExpr::LitString(s) => (format!("\"{s}\""), format!("\"{s}\"")),
            TypeExpr::LitInt(v) => (v.to_string(), v.to_string()),
            _ => continue,
        };
        if seen.contains(&label) || already_covered.contains(&label) {
            continue;
        }
        seen.push(label.clone());
        out.push(CompletionItem {
            label,
            kind: Some(CompletionItemKind::ENUM_MEMBER),
            detail: Some(format!("{obj_name}.{property} variant")),
            insert_text: Some(insert),
            ..Default::default()
        });
    }
    Some(out)
}

/// Resolve a `TypeExpr::Named(alias)` by looking up the matching
/// top-level `type NAME = <body>` declaration in the AST and
/// substituting its body. Walks the alias chain up to a small depth
/// so nested aliases unwrap correctly. Non-`Named` inputs pass
/// through unchanged. The LSP helpers only need this because the
/// symbol table stores the declared-name form of a parameter type;
/// upstream consumers that already call `TypeChecker::resolve_alias`
/// don't need it.
fn resolve_type_alias_from_ast(ty: &TypeExpr, ast: &[harn_parser::SNode]) -> TypeExpr {
    let mut current = ty.clone();
    let mut seen: Vec<String> = Vec::new();
    loop {
        let TypeExpr::Named(name) = &current else {
            return current;
        };
        if seen.contains(name) {
            return current;
        }
        seen.push(name.clone());
        let Some(body) = find_type_alias_body(ast, name) else {
            return current;
        };
        current = body;
    }
}

fn find_type_alias_body(ast: &[harn_parser::SNode], name: &str) -> Option<TypeExpr> {
    let mut found: Option<TypeExpr> = None;
    visit_nodes(ast, &mut |node| {
        if found.is_some() {
            return;
        }
        if let harn_parser::Node::TypeDecl {
            name: n, type_expr, ..
        } = &node.node
        {
            if n == name {
                found = Some(type_expr.clone());
            }
        }
    });
    found
}

fn collect_literal_alternatives(pattern: &harn_parser::SNode, out: &mut Vec<String>) {
    match &pattern.node {
        harn_parser::Node::StringLiteral(s) => out.push(format!("\"{s}\"")),
        harn_parser::Node::IntLiteral(v) => out.push(v.to_string()),
        harn_parser::Node::OrPattern(alts) => {
            for alt in alts {
                collect_literal_alternatives(alt, out);
            }
        }
        _ => {}
    }
}

/// Depth-first search of the AST for the innermost `Node::MatchExpr`
/// whose span contains `offset`. The LSP completion handler uses this
/// to decide when the cursor is inside a match's arm-pattern position.
fn find_innermost_match_at(
    ast: &[harn_parser::SNode],
    offset: usize,
) -> Option<&harn_parser::SNode> {
    let mut best: Option<&harn_parser::SNode> = None;
    visit_nodes(ast, &mut |node| {
        if !matches!(node.node, harn_parser::Node::MatchExpr { .. }) {
            return;
        }
        let span = node.span;
        if offset < span.start || offset > span.end {
            return;
        }
        // Prefer the node with the smallest span (deepest nesting).
        if let Some(current) = best {
            let current_len = current.span.end - current.span.start;
            let node_len = span.end - span.start;
            if node_len < current_len {
                best = Some(node);
            }
        } else {
            best = Some(node);
        }
    });
    best
}

fn visit_nodes<'a, F>(nodes: &'a [harn_parser::SNode], visitor: &mut F)
where
    F: FnMut(&'a harn_parser::SNode),
{
    for node in nodes {
        visit_node(node, visitor);
    }
}

fn visit_node<'a, F>(node: &'a harn_parser::SNode, visitor: &mut F)
where
    F: FnMut(&'a harn_parser::SNode),
{
    visitor(node);
    use harn_parser::Node;
    match &node.node {
        Node::Pipeline { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::Block(body)
        | Node::Closure { body, .. }
        | Node::TryExpr { body }
        | Node::SpawnExpr { body }
        | Node::MutexBlock { body }
        | Node::DeferStmt { body } => visit_nodes(body, visitor),
        Node::MatchExpr { value, arms } => {
            visit_node(value, visitor);
            for arm in arms {
                visit_node(&arm.pattern, visitor);
                if let Some(g) = &arm.guard {
                    visit_node(g, visitor);
                }
                visit_nodes(&arm.body, visitor);
            }
        }
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            visit_node(condition, visitor);
            visit_nodes(then_body, visitor);
            if let Some(eb) = else_body {
                visit_nodes(eb, visitor);
            }
        }
        Node::ForIn { iterable, body, .. } => {
            visit_node(iterable, visitor);
            visit_nodes(body, visitor);
        }
        Node::WhileLoop { condition, body } => {
            visit_node(condition, visitor);
            visit_nodes(body, visitor);
        }
        Node::CostRoute { options, body } => {
            for (_, value) in options {
                visit_node(value, visitor);
            }
            visit_nodes(body, visitor);
        }
        Node::BinaryOp { left, right, .. } => {
            visit_node(left, visitor);
            visit_node(right, visitor);
        }
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::TryOperator { operand: object }
        | Node::TryStar { operand: object } => visit_node(object, visitor),
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            visit_node(object, visitor);
            for a in args {
                visit_node(a, visitor);
            }
        }
        Node::FunctionCall { args, .. } => {
            for a in args {
                visit_node(a, visitor);
            }
        }
        Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
            visit_node(value, visitor);
        }
        Node::ReturnStmt { value: Some(v) } | Node::YieldExpr { value: Some(v) } => {
            visit_node(v, visitor)
        }
        Node::EmitExpr { value } => visit_node(value, visitor),
        Node::AttributedDecl { inner, .. } => visit_node(inner, visitor),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{discriminator_value_completions, dot_completion_items};
    use crate::document::DocumentState;
    use tower_lsp::lsp_types::Position;

    fn completion_items_at(source: &str, marker: &str) -> Vec<(String, Option<String>)> {
        let state = DocumentState::new(source.to_string());
        let mut location = None;
        for (line_index, line) in source.lines().enumerate() {
            if let Some(column) = line.find(marker) {
                location = Some(Position::new(
                    line_index as u32,
                    (column + marker.len()) as u32,
                ));
                break;
            }
        }
        let position = location.expect("marker should exist in source");
        dot_completion_items(&state.source, position, &state.symbols)
            .into_iter()
            .map(|item| (item.label, item.detail))
            .collect()
    }

    #[test]
    fn dot_completion_prefers_shape_fields() {
        let items = completion_items_at(
            r#"pipeline test() {
  let data = {name: "Ada", count: 3}
  data.name
}"#,
            "data.",
        );
        assert!(
            items
                .iter()
                .any(|(label, detail)| { label == "name" && detail.as_deref() == Some("string") }),
            "items: {items:?}"
        );
        assert!(
            items
                .iter()
                .any(|(label, detail)| { label == "count" && detail.as_deref() == Some("int") }),
            "items: {items:?}"
        );
        assert!(
            !items.iter().any(|(label, _)| label == "merge"),
            "items: {items:?}"
        );
    }

    #[test]
    fn dot_completion_includes_struct_fields_and_methods() {
        let items = completion_items_at(
            r#"pipeline test() {
  struct Person { name: string, age: int }
  impl Person {
    fn greet(self) -> string { return self.name }
  }
  let person = Person({name: "Ada", age: 3})
  person.name
}"#,
            "person.",
        );
        assert!(
            items
                .iter()
                .any(|(label, detail)| { label == "name" && detail.as_deref() == Some("string") }),
            "items: {items:?}"
        );
        assert!(
            items.iter().any(|(label, detail)| {
                label == "greet"
                    && detail
                        .as_deref()
                        .is_some_and(|detail| detail.contains("fn greet"))
            }),
            "items: {items:?}"
        );
    }

    #[test]
    fn dot_completion_includes_enum_variants_with_field_details() {
        let items = completion_items_at(
            r#"pipeline test() {
  enum Event {
    Click(x: int, y: int),
    Quit,
  }
  Event.Click
}"#,
            "Event.",
        );
        assert!(
            items.iter().any(|(label, detail)| {
                label == "Click"
                    && detail.as_deref().is_some_and(|detail| {
                        detail.contains("x: int") && detail.contains("y: int")
                    })
            }),
            "items: {items:?}"
        );
        assert!(
            items.iter().any(|(label, detail)| {
                label == "Quit" && detail.as_deref() == Some("enum variant")
            }),
            "items: {items:?}"
        );
    }

    fn discriminator_items_at(source: &str, marker: &str) -> Vec<String> {
        let state = DocumentState::new(source.to_string());
        let mut location = None;
        for (line_index, line) in source.lines().enumerate() {
            if let Some(column) = line.find(marker) {
                location = Some(Position::new(
                    line_index as u32,
                    (column + marker.len()) as u32,
                ));
                break;
            }
        }
        let position = location.expect("marker should exist in source");
        let ast = state
            .cached_ast
            .as_ref()
            .expect("ast should parse — check the test fixture for syntax issues");
        discriminator_value_completions(ast, &state.source, position, &state.symbols)
            .unwrap_or_default()
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    #[test]
    fn discriminator_completion_suggests_tagged_shape_union_literals() {
        // Cursor sits at the start of an empty dummy arm; all
        // discriminator variants should be offered. The dummy `_`
        // arm keeps the match parseable while the user is typing
        // real alternatives in its place.
        let items = discriminator_items_at(
            r#"type Msg = {kind: "ping", ttl: int} | {kind: "pong", latency_ms: int}

fn handle(m: Msg) -> string {
  return match m.kind {
    MARK_ -> { "todo" }
  }
}
pipeline default() { }"#,
            "MARK",
        );
        assert!(
            items.iter().any(|l| l == "\"ping\""),
            "expected \"ping\" completion, got: {items:?}"
        );
        assert!(
            items.iter().any(|l| l == "\"pong\""),
            "expected \"pong\" completion, got: {items:?}"
        );
    }

    #[test]
    fn discriminator_completion_excludes_already_covered_arms() {
        // Same shape but with one explicit arm already covering
        // "ping"; the completion should omit it and still offer
        // "pong".
        let items = discriminator_items_at(
            r#"type Msg = {kind: "ping", ttl: int} | {kind: "pong", latency_ms: int}

fn handle(m: Msg) -> string {
  return match m.kind {
    "ping" -> { "p" }
    MARK_ -> { "todo" }
  }
}
pipeline default() { }"#,
            "MARK",
        );
        assert!(
            !items.iter().any(|l| l == "\"ping\""),
            "expected \"ping\" filtered out after explicit arm, got: {items:?}"
        );
        assert!(
            items.iter().any(|l| l == "\"pong\""),
            "expected \"pong\" still offered, got: {items:?}"
        );
    }
}
