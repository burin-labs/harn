use std::collections::HashMap;
use std::time::Duration;

use harn_lexer::{Lexer, TokenKind};
use harn_modules::DefKind;
use harn_parser::{format_type, ShapeField, TypeExpr};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::constants::{
    builtin_doc, keyword_doc, BUILTINS, DICT_METHODS, KEYWORDS, LIST_METHODS, STRING_METHODS,
};
use crate::document::DocumentState;
use crate::helpers::{
    char_before_position, extract_backtick_name, find_word_in_region, infer_dot_receiver_name,
    infer_dot_receiver_type, lsp_position_to_offset, offset_to_position, position_in_span,
    span_to_full_range, span_to_range, word_at_position,
};
use crate::references::find_references;
use crate::semantic_tokens::{build_semantic_tokens, semantic_token_legend};
use crate::symbols::{
    format_shape_expanded, format_union_shapes_expanded, EnumVariantInfo, HarnSymbolKind,
    SymbolInfo,
};
use crate::HarnLsp;

/// Resolve the symbol through the current document's imported modules using
/// `harn-modules`, and return its definition location when available.
///
/// `harn_modules::build` recursively follows import paths, so seeding it
/// with the current file is enough to discover every module reachable via
/// imports.
/// Collect discriminator-value completion items for a cursor inside
/// a `match obj.<tag> { … }` block. Returns `None` when the cursor
/// isn't inside such a match; returns an empty `Vec` when the match
/// is found but `obj` isn't a recognised tagged shape union (the
/// caller then falls through to the normal completion list).
fn discriminator_value_completions(
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
        Node::AttributedDecl { inner, .. } => visit_node(inner, visitor),
        _ => {}
    }
}

/// Build a `TextEdit` that inserts "missing" match arms just before
/// the `}` that closes the match expression at `match_span`. Each
/// missing variant becomes one new arm of the form
/// `{pattern} -> { unreachable("TODO: handle {pattern}") }`, indented
/// relative to the closing brace.
///
/// Returns `None` when the span doesn't look like a well-formed
/// `match` expression (e.g. the closing `}` isn't at the expected
/// byte position) — in that case the code-action is silently skipped
/// rather than emitting a broken edit.
fn build_missing_arms_edit(
    source: &str,
    match_span: &harn_lexer::Span,
    missing: &[String],
) -> Option<TextEdit> {
    if missing.is_empty() {
        return None;
    }
    // Span.end is exclusive: the last byte of the match — the `}` —
    // is at span.end - 1.
    let close_brace_byte = match_span.end.checked_sub(1)?;
    let bytes = source.as_bytes();
    if close_brace_byte >= bytes.len() || bytes[close_brace_byte] != b'}' {
        return None;
    }
    // Measure the closing brace's indent by walking back from its
    // position to the start of its line and counting whitespace.
    let line_start = source[..close_brace_byte]
        .rfind('\n')
        .map(|n| n + 1)
        .unwrap_or(0);
    let indent_slice = &source[line_start..close_brace_byte];
    let brace_indent: String = indent_slice
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    // Arm indent is brace indent + 2 spaces (Harn formatter
    // convention). If the brace is on the same line as other content
    // (e.g. a single-line match), `indent_slice` still starts with
    // whatever lead-in was there — we conservatively still add 2
    // spaces of nesting, which produces correct but possibly ugly
    // output on single-line matches.
    let arm_indent = format!("{brace_indent}  ");
    let mut inserted = String::new();
    for pattern in missing {
        inserted.push('\n');
        inserted.push_str(&arm_indent);
        inserted.push_str(pattern);
        inserted.push_str(" -> { unreachable(\"TODO: handle ");
        inserted.push_str(pattern);
        inserted.push_str("\") }");
    }
    inserted.push('\n');
    inserted.push_str(&brace_indent);
    let brace_pos = offset_to_position(source, close_brace_byte);
    Some(TextEdit {
        range: Range {
            start: brace_pos,
            end: brace_pos,
        },
        new_text: inserted,
    })
}

fn resolve_cross_file_definition(uri: &Url, word: &str) -> Option<Location> {
    let current_path = uri.to_file_path().ok()?;
    let module_graph = harn_modules::build(std::slice::from_ref(&current_path));
    let def = module_graph.definition_of(&current_path, word)?;
    if !matches!(
        def.kind,
        DefKind::Pipeline
            | DefKind::Function
            | DefKind::Variable
            | DefKind::Parameter
            | DefKind::Enum
            | DefKind::Struct
            | DefKind::Interface
    ) {
        return None;
    }
    let imported_source = std::fs::read_to_string(&def.file).ok()?;
    let imported_uri = Url::from_file_path(&def.file).ok()?;
    Some(Location {
        uri: imported_uri,
        range: span_to_full_range(&def.span, &imported_source),
    })
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

fn dot_completion_items(
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

#[tower_lsp::async_trait]
impl tower_lsp::LanguageServer for HarnLsp {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: semantic_token_legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            work_done_progress_options: Default::default(),
                        },
                    ),
                ),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![")".to_string()]),
                    work_done_progress_options: Default::default(),
                }),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                rename_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
                    InlayHintOptions {
                        work_done_progress_options: Default::default(),
                        resolve_provider: None,
                    },
                ))),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Harn LSP initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let source = params.text_document.text.clone();

        let state = DocumentState::new(source);
        let diagnostics = state.diagnostics.clone();
        self.documents.lock().unwrap().insert(uri.clone(), state);

        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        if let Some(change) = params.content_changes.into_iter().last() {
            let source = change.text;
            let diagnostics;
            {
                let mut docs = self.documents.lock().unwrap();
                let entry = docs
                    .entry(uri.clone())
                    .or_insert_with(|| DocumentState::new(String::new()));
                entry.update_source(source);
            }

            let version = {
                let mut versions = self.pending_reparse_versions.lock().unwrap();
                let next = versions.get(&uri).copied().unwrap_or(0) + 1;
                versions.insert(uri.clone(), next);
                next
            };

            tokio::time::sleep(Duration::from_millis(100)).await;

            {
                let versions = self.pending_reparse_versions.lock().unwrap();
                if versions.get(&uri).copied() != Some(version) {
                    return;
                }
            }

            {
                let mut docs = self.documents.lock().unwrap();
                let Some(entry) = docs.get_mut(&uri) else {
                    return;
                };
                entry.reparse_if_dirty();
                diagnostics = entry.diagnostics.clone();
            }
            self.client
                .publish_diagnostics(uri, diagnostics, None)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .lock()
            .unwrap()
            .remove(&params.text_document.uri);
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
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

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        for sym in &symbols {
            if sym.name == word
                && matches!(
                    sym.kind,
                    HarnSymbolKind::Pipeline
                        | HarnSymbolKind::Function
                        | HarnSymbolKind::Variable
                        | HarnSymbolKind::Parameter
                        | HarnSymbolKind::Enum
                        | HarnSymbolKind::Struct
                        | HarnSymbolKind::Interface
                )
            {
                let range = span_to_full_range(&sym.def_span, &source);
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: uri.clone(),
                    range,
                })));
            }
        }

        // Cross-file: the module graph transitively follows imports from
        // this file, so there's no need to pre-walk the AST here.
        if let Some(loc) = resolve_cross_file_definition(uri, &word) {
            return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
        }

        Ok(None)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let ast = state.cached_ast.clone();
        drop(docs);

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        let program = match ast {
            Some(p) => p,
            None => return Ok(None),
        };

        let ref_spans = find_references(&program, &word);
        if ref_spans.is_empty() {
            return Ok(None);
        }

        let locations: Vec<Location> = ref_spans
            .iter()
            .map(|span| Location {
                uri: uri.clone(),
                range: span_to_full_range(span, &source),
            })
            .collect();

        Ok(Some(locations))
    }

    #[allow(deprecated)]
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let mut doc_symbols = Vec::new();
        for sym in &symbols {
            let kind = match sym.kind {
                HarnSymbolKind::Pipeline => SymbolKind::FUNCTION,
                HarnSymbolKind::Function => SymbolKind::FUNCTION,
                HarnSymbolKind::Variable => SymbolKind::VARIABLE,
                HarnSymbolKind::Enum => SymbolKind::ENUM,
                HarnSymbolKind::Struct => SymbolKind::STRUCT,
                HarnSymbolKind::Interface => SymbolKind::INTERFACE,
                HarnSymbolKind::Parameter => continue, // skip params from outline
            };
            // Outline shows top-level symbols plus functions/variables one level deep.
            if sym.scope_span.is_some()
                && !matches!(
                    sym.kind,
                    HarnSymbolKind::Function | HarnSymbolKind::Variable
                )
            {
                continue;
            }
            let range = span_to_full_range(&sym.def_span, &source);
            let detail = match sym.kind {
                HarnSymbolKind::Pipeline => "pipeline",
                HarnSymbolKind::Function => "function",
                HarnSymbolKind::Variable => "variable",
                HarnSymbolKind::Enum => "enum",
                HarnSymbolKind::Struct => "struct",
                HarnSymbolKind::Interface => "interface",
                HarnSymbolKind::Parameter => "parameter",
            };
            doc_symbols.push(DocumentSymbol {
                name: sym.name.clone(),
                detail: Some(detail.to_string()),
                kind,
                range,
                selection_range: range,
                tags: None,
                deprecated: None,
                children: None,
            });
        }

        Ok(Some(DocumentSymbolResponse::Nested(doc_symbols)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        if let Some(doc) = builtin_doc(&word) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: doc,
                }),
                range: None,
            }));
        }

        if let Some(doc) = keyword_doc(&word) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: doc,
                }),
                range: None,
            }));
        }

        // Check user-defined symbols — prefer the innermost scope that
        // contains the cursor position so that shadowed bindings resolve
        // to the closest definition.
        let cursor_offset = lsp_position_to_offset(&source, position);
        let mut best: Option<&SymbolInfo> = None;
        for sym in &symbols {
            if sym.name != word {
                continue;
            }
            // Impl-block methods are globally visible via dot syntax — skip scope check.
            let in_scope = if sym.impl_type.is_some() {
                true
            } else {
                match sym.scope_span {
                    Some(sp) => cursor_offset >= sp.start && cursor_offset <= sp.end,
                    None => true,
                }
            };
            if !in_scope {
                continue;
            }
            // Tightest-scope wins on shadowing.
            match best {
                None => best = Some(sym),
                Some(prev) => {
                    let prev_scope_size = match prev.scope_span {
                        Some(sp) => sp.end.saturating_sub(sp.start),
                        None => usize::MAX,
                    };
                    let this_scope_size = match sym.scope_span {
                        Some(sp) => sp.end.saturating_sub(sp.start),
                        None => usize::MAX,
                    };
                    if this_scope_size < prev_scope_size {
                        best = Some(sym);
                    }
                }
            }
        }
        if let Some(sym) = best {
            let mut hover_text = String::new();

            if let Some(ref sig) = sym.signature {
                let display_sig = if let Some(ref impl_ty) = sym.impl_type {
                    format!("impl {impl_ty}\n{sig}")
                } else {
                    sig.clone()
                };
                hover_text.push_str(&format!("```harn\n{display_sig}\n```\n"));
            } else {
                let keyword = match sym.kind {
                    HarnSymbolKind::Variable => "let",
                    HarnSymbolKind::Parameter => "param",
                    _ => "",
                };
                if let Some(ref ty) = sym.type_info {
                    hover_text.push_str(&format!(
                        "```harn\n{keyword} {}: {}\n```\n",
                        sym.name,
                        format_type(ty)
                    ));
                } else {
                    let kind_str = match sym.kind {
                        HarnSymbolKind::Pipeline => "pipeline",
                        HarnSymbolKind::Function => "function",
                        HarnSymbolKind::Variable => "variable",
                        HarnSymbolKind::Parameter => "parameter",
                        HarnSymbolKind::Enum => "enum",
                        HarnSymbolKind::Struct => "struct",
                        HarnSymbolKind::Interface => "interface",
                    };
                    hover_text.push_str(&format!("**{kind_str}** `{}`", sym.name));
                }
            }

            // Signatures already show `-> type`; expand only shape types for
            // variables/params so complex shapes get a human-readable breakdown.
            // Tagged shape unions (union-of-shapes) also get an expanded view
            // so the variants are laid out vertically instead of collapsed
            // onto one line.
            if sym.signature.is_none() {
                if let Some(ref ty) = sym.type_info {
                    if matches!(ty, harn_parser::TypeExpr::Shape(_)) {
                        let expanded = format_shape_expanded(ty, 0);
                        if !expanded.is_empty() {
                            hover_text.push_str(&format!("\n{expanded}"));
                        }
                    } else if matches!(ty, harn_parser::TypeExpr::Union(_)) {
                        let expanded = format_union_shapes_expanded(ty);
                        if !expanded.is_empty() {
                            hover_text.push_str(&format!("\n{expanded}"));
                        }
                    }
                }
            }

            if let Some(ref doc) = sym.doc_comment {
                hover_text.push_str(&format!("\n---\n\n{doc}"));
            }

            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: hover_text,
                }),
                range: None,
            }));
        }

        Ok(None)
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let source = {
            let docs = self.documents.lock().unwrap();
            match docs.get(uri) {
                Some(s) => s.source.clone(),
                None => return Ok(None),
            }
        };

        let lines: Vec<&str> = source.lines().collect();
        let line = match lines.get(position.line as usize) {
            Some(l) => *l,
            None => return Ok(None),
        };
        let col = position.character as usize;
        let prefix = if col <= line.len() {
            &line[..col]
        } else {
            line
        };

        let mut depth = 0i32;
        let mut comma_count = 0u32;
        let mut open_paren_pos = None;
        for (i, ch) in prefix.char_indices().rev() {
            match ch {
                ')' => depth += 1,
                '(' => {
                    if depth == 0 {
                        open_paren_pos = Some(i);
                        break;
                    }
                    depth -= 1;
                }
                ',' if depth == 0 => comma_count += 1,
                _ => {}
            }
        }

        let paren_pos = match open_paren_pos {
            Some(p) => p,
            None => return Ok(None),
        };

        let before = &prefix[..paren_pos];
        let name: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect();

        if name.is_empty() {
            return Ok(None);
        }

        let sig_str = match BUILTINS.iter().find(|(n, _)| *n == name.as_str()) {
            Some((_, sig)) => *sig,
            None => return Ok(None),
        };

        // Extract parameter fragment from `name(p1, p2, ...) -> ret`.
        let params_str = sig_str
            .split('(')
            .nth(1)
            .and_then(|s| s.split(')').next())
            .unwrap_or("");

        let params_list: Vec<ParameterInformation> = if params_str.is_empty() {
            vec![]
        } else {
            params_str
                .split(',')
                .map(|p| ParameterInformation {
                    label: ParameterLabel::Simple(p.trim().to_string()),
                    documentation: None,
                })
                .collect()
        };

        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: sig_str.to_string(),
                documentation: builtin_doc(&name).map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d,
                    })
                }),
                parameters: Some(params_list.clone()),
                active_parameter: Some(if params_list.is_empty() {
                    0
                } else {
                    comma_count.min(params_list.len() as u32 - 1)
                }),
            }],
            active_signature: Some(0),
            active_parameter: Some(if params_list.is_empty() {
                0
            } else {
                comma_count.min(params_list.len() as u32 - 1)
            }),
        }))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        let docs = self.documents.lock().unwrap();
        let mut results = Vec::new();

        for (uri, state) in docs.iter() {
            for sym in &state.symbols {
                let kind = match sym.kind {
                    HarnSymbolKind::Pipeline => SymbolKind::FUNCTION,
                    HarnSymbolKind::Function => SymbolKind::FUNCTION,
                    HarnSymbolKind::Variable => SymbolKind::VARIABLE,
                    HarnSymbolKind::Enum => SymbolKind::ENUM,
                    HarnSymbolKind::Struct => SymbolKind::STRUCT,
                    HarnSymbolKind::Interface => SymbolKind::INTERFACE,
                    HarnSymbolKind::Parameter => continue,
                };
                let name_lower = sym.name.to_lowercase();
                if !query.is_empty() && !name_lower.contains(&query) {
                    continue;
                }
                let range = span_to_full_range(&sym.def_span, &state.source);
                #[allow(deprecated)]
                results.push(SymbolInformation {
                    name: sym.name.clone(),
                    kind,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range,
                    },
                    container_name: None,
                });
            }
        }

        Ok(Some(results))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let mut actions = Vec::new();

        let (source, lint_diags, type_diags) = {
            let docs = self.documents.lock().unwrap();
            let state = match docs.get(uri) {
                Some(s) => s,
                None => return Ok(Some(actions)),
            };
            (
                state.source.clone(),
                state.lint_diagnostics.clone(),
                state.type_diagnostics.clone(),
            )
        };

        for diag in &params.context.diagnostics {
            let msg = &diag.message;

            if let Some(ld) = lint_diags.iter().find(|ld| {
                msg.contains(&format!("[{}]", ld.rule)) && span_to_range(&ld.span) == diag.range
            }) {
                if let Some(ref fix_edits) = ld.fix {
                    let text_edits: Vec<TextEdit> = fix_edits
                        .iter()
                        .map(|fe| TextEdit {
                            range: Range {
                                start: offset_to_position(&source, fe.span.start),
                                end: offset_to_position(&source, fe.span.end),
                            },
                            new_text: fe.replacement.clone(),
                        })
                        .collect();

                    let title = match ld.rule {
                        "mutable-never-reassigned" => "Change `var` to `let`".to_string(),
                        "comparison-to-bool" => "Simplify boolean comparison".to_string(),
                        "unnecessary-else-return" => "Remove unnecessary else".to_string(),
                        "unused-import" => {
                            let name =
                                extract_backtick_name(msg).unwrap_or_else(|| "name".to_string());
                            format!("Remove unused import `{name}`")
                        }
                        "invalid-binary-op-literal" => {
                            "Convert to string interpolation".to_string()
                        }
                        _ => ld
                            .suggestion
                            .clone()
                            .unwrap_or_else(|| "Apply fix".to_string()),
                    };

                    let mut changes = HashMap::new();
                    changes.insert(uri.clone(), text_edits);
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title,
                        kind: Some(CodeActionKind::QUICKFIX),
                        diagnostics: Some(vec![diag.clone()]),
                        edit: Some(WorkspaceEdit {
                            changes: Some(changes),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }));
                    continue;
                }
            }

            if diag.source.as_deref() == Some("harn-typecheck") {
                if let Some(td) = type_diags.iter().find(|td| {
                    td.message == *msg && td.span.as_ref().map(span_to_range) == Some(diag.range)
                }) {
                    if let Some(ref fix_edits) = td.fix {
                        let text_edits: Vec<TextEdit> = fix_edits
                            .iter()
                            .map(|fe| TextEdit {
                                range: Range {
                                    start: offset_to_position(&source, fe.span.start),
                                    end: offset_to_position(&source, fe.span.end),
                                },
                                new_text: fe.replacement.clone(),
                            })
                            .collect();

                        let mut changes = HashMap::new();
                        changes.insert(uri.clone(), text_edits);
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: "Convert to string interpolation".to_string(),
                            kind: Some(CodeActionKind::QUICKFIX),
                            diagnostics: Some(vec![diag.clone()]),
                            edit: Some(WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }));
                        continue;
                    }

                    // Non-exhaustive match: synthesise an "Add missing
                    // arms" quick-fix from the structured details on
                    // the diagnostic. The diagnostic's span covers the
                    // whole `match` expression, so the closing `}`
                    // sits at `span.end - 1`. We insert `arm_indent`
                    // + pattern + `-> { unreachable(...) }` right
                    // before the `}`, using the closing brace's
                    // column as the reference indent.
                    if let (
                        Some(harn_parser::DiagnosticDetails::NonExhaustiveMatch { missing }),
                        Some(span),
                    ) = (td.details.as_ref(), td.span.as_ref())
                    {
                        if let Some(edit) = build_missing_arms_edit(&source, span, missing) {
                            let mut changes = HashMap::new();
                            changes.insert(uri.clone(), vec![edit]);
                            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                                title: if missing.len() == 1 {
                                    format!("Add missing match arm {}", missing[0])
                                } else {
                                    format!("Add missing match arms ({})", missing.len())
                                },
                                kind: Some(CodeActionKind::QUICKFIX),
                                diagnostics: Some(vec![diag.clone()]),
                                edit: Some(WorkspaceEdit {
                                    changes: Some(changes),
                                    ..Default::default()
                                }),
                                is_preferred: Some(true),
                                ..Default::default()
                            }));
                            continue;
                        }
                    }
                }
            }

            // Fallback manual code actions for rules without structured fixes.
            if msg.contains("[unused-variable]") || msg.contains("[unused-parameter]") {
                if let Some(name) = extract_backtick_name(msg) {
                    let offset = lsp_position_to_offset(&source, diag.range.start);
                    let end_offset = lsp_position_to_offset(&source, diag.range.end)
                        .max(offset + 1)
                        .min(source.len());
                    let search_region = &source[offset..end_offset];
                    if let Some(name_pos) = find_word_in_region(search_region, &name) {
                        let abs_pos = offset + name_pos;
                        let start = offset_to_position(&source, abs_pos);
                        let end = offset_to_position(&source, abs_pos + name.len());
                        let edit_range = Range { start, end };

                        let mut changes = HashMap::new();
                        changes.insert(
                            uri.clone(),
                            vec![TextEdit {
                                range: edit_range,
                                new_text: format!("_{name}"),
                            }],
                        );
                        let label = if msg.contains("[unused-variable]") {
                            "variable"
                        } else {
                            "parameter"
                        };
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: format!("Prefix {label} `{name}` with `_`"),
                            kind: Some(CodeActionKind::QUICKFIX),
                            diagnostics: Some(vec![diag.clone()]),
                            edit: Some(WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }));
                    }
                }
            }
        }

        Ok(Some(actions))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let source = {
            let docs = self.documents.lock().unwrap();
            match docs.get(uri) {
                Some(s) => s.source.clone(),
                None => return Ok(None),
            }
        };

        let formatted = match harn_fmt::format_source(&source) {
            Ok(f) => f,
            Err(_) => return Ok(None),
        };

        if formatted == source {
            return Ok(None);
        }

        let line_count = source.lines().count() as u32;
        let last_line_len = source.lines().last().map_or(0, |l| l.len()) as u32;
        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position::new(0, 0),
                end: Position::new(line_count, last_line_len),
            },
            new_text: formatted,
        }]))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = &params.new_name;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let ast = state.cached_ast.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let old_name = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        // Builtins must not be renamed.
        if BUILTINS.iter().any(|(n, _)| *n == old_name) {
            return Ok(None);
        }

        let symbol_exists = symbols.iter().any(|s| s.name == old_name);
        if !symbol_exists {
            return Ok(None);
        }

        let program = match ast {
            Some(p) => p,
            None => return Ok(None),
        };
        let ref_spans = find_references(&program, &old_name);
        if ref_spans.is_empty() {
            return Ok(None);
        }

        // AST reference spans cover whole declarations, so rescan the lexer
        // tokens within each span to pin down the exact identifier position.
        let mut edits = Vec::new();
        let mut seen_offsets = std::collections::HashSet::new();

        let mut lexer = Lexer::new(&source);
        if let Ok(tokens) = lexer.tokenize() {
            for token in &tokens {
                if let TokenKind::Identifier(ref name) = token.kind {
                    if name == &old_name && !seen_offsets.contains(&token.span.start) {
                        let in_ref = ref_spans
                            .iter()
                            .any(|rs| token.span.start >= rs.start && token.span.end <= rs.end);
                        if in_ref {
                            seen_offsets.insert(token.span.start);
                            let start = offset_to_position(&source, token.span.start);
                            let end = offset_to_position(&source, token.span.end);
                            edits.push(TextEdit {
                                range: Range { start, end },
                                new_text: new_name.clone(),
                            });
                        }
                    }
                }
            }
        }

        if edits.is_empty() {
            return Ok(None);
        }

        // Sort bottom-up so applying edits doesn't shift later offsets.
        edits.sort_by(|a, b| {
            b.range
                .start
                .line
                .cmp(&a.range.start.line)
                .then(b.range.start.character.cmp(&a.range.start.character))
        });

        let mut changes = HashMap::new();
        changes.insert(uri.clone(), edits);

        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        // Tokenize (lexer never fails fatally for semantic tokens — if it
        // errors we still have partial tokens up to the error point, but
        // the simple API returns Err. Re-lex and collect what we can.)
        let mut lexer = Lexer::new(&source);
        let tokens = match lexer.tokenize() {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };

        let semantic_tokens = build_semantic_tokens(&tokens, &symbols, &source);

        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: semantic_tokens,
        })))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let docs = self.documents.lock().unwrap();
        let Some(state) = docs.get(&uri) else {
            return Ok(None);
        };

        let range = params.range;
        let hints: Vec<InlayHint> = state
            .inlay_hints
            .iter()
            .filter(|h| {
                let line = h.line.saturating_sub(1) as u32;
                line >= range.start.line && line <= range.end.line
            })
            .map(|h| InlayHint {
                position: Position::new(
                    h.line.saturating_sub(1) as u32,
                    h.column.saturating_sub(1) as u32,
                ),
                label: InlayHintLabel::String(h.label.clone()),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: None,
                data: None,
            })
            .collect();

        Ok(if hints.is_empty() { None } else { Some(hints) })
    }
}

#[cfg(test)]
mod tests {
    use super::dot_completion_items;
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
        use crate::handlers::discriminator_value_completions;
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

    #[test]
    fn missing_arms_edit_inserts_each_variant_before_close_brace() {
        use crate::handlers::build_missing_arms_edit;
        use harn_lexer::Span;

        let source = "pipeline default() {\n  match v {\n    \"pass\" -> { }\n  }\n}\n";
        // Byte range covering `match v { ... }`.
        let start = source.find("match").unwrap();
        let end = source[start..].find('\n').unwrap();
        let match_block_start = start;
        let match_block_end_brace = source
            .match_indices('\n')
            .filter(|(idx, _)| *idx > start)
            .nth(2)
            .map(|(idx, _)| idx)
            .unwrap();
        // Find the actual `}` that closes the match block.
        let close_brace_pos = source[match_block_start..match_block_end_brace]
            .rfind('}')
            .map(|r| match_block_start + r)
            .unwrap();
        let span = Span {
            start: match_block_start,
            end: close_brace_pos + 1,
            line: 2,
            column: 3,
            end_line: 4,
        };
        let missing = vec!["\"fail\"".to_string(), "\"skip\"".to_string()];
        let _ = end;
        let edit = build_missing_arms_edit(source, &span, &missing)
            .expect("expected edit for well-formed match");
        assert!(edit.new_text.contains("\"fail\" -> "), "{:?}", edit);
        assert!(edit.new_text.contains("\"skip\" -> "), "{:?}", edit);
        assert!(
            edit.new_text.contains("unreachable"),
            "edit should scaffold with unreachable: {:?}",
            edit
        );
        // Indent should be 4 spaces for arms (brace at col 2 + 2).
        assert!(
            edit.new_text.contains("\n    \"fail\""),
            "expected 4-space arm indent, got: {:?}",
            edit.new_text
        );
    }

    #[test]
    fn missing_arms_edit_returns_none_when_close_brace_missing() {
        use crate::handlers::build_missing_arms_edit;
        use harn_lexer::Span;

        let source = "not a match expression";
        let span = Span {
            start: 0,
            end: source.len(),
            line: 1,
            column: 1,
            end_line: 1,
        };
        let edit = build_missing_arms_edit(source, &span, &["\"x\"".to_string()]);
        assert!(edit.is_none());
    }
}
