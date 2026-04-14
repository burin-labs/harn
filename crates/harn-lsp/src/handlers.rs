use std::collections::HashMap;
use std::time::Duration;

use harn_lexer::{Lexer, TokenKind};
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
use crate::symbols::{format_shape_expanded, EnumVariantInfo, HarnSymbolKind, SymbolInfo};
use crate::HarnLsp;

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
        drop(docs);

        let mut items = Vec::new();

        if char_before_position(&source, position) == Some('.') {
            return Ok(Some(CompletionResponse::Array(dot_completion_items(
                &source, position, &symbols,
            ))));
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
            if sym.signature.is_none() {
                if let Some(ref ty) = sym.type_info {
                    if matches!(ty, harn_parser::TypeExpr::Shape(_)) {
                        let expanded = format_shape_expanded(ty, 0);
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
}
