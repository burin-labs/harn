use std::collections::HashMap;

use harn_lexer::{Lexer, TokenKind};
use harn_parser::format_type;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::constants::{
    builtin_doc, keyword_doc, BUILTINS, DICT_METHODS, KEYWORDS, LIST_METHODS, STRING_METHODS,
};
use crate::document::DocumentState;
use crate::helpers::{
    char_before_position, extract_backtick_name, find_word_in_region, infer_dot_receiver_type,
    lsp_position_to_offset, offset_to_position, position_in_span, simplify_bool_comparison,
    span_to_full_range, word_at_position,
};
use crate::references::find_references;
use crate::semantic_tokens::{build_semantic_tokens, semantic_token_legend};
use crate::symbols::{format_shape_expanded, HarnSymbolKind, SymbolInfo};
use crate::HarnLsp;

// ---------------------------------------------------------------------------
// LanguageServer trait implementation
// ---------------------------------------------------------------------------

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
                entry.update(source);
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

    // -----------------------------------------------------------------------
    // Completion (scope-aware + method completion)
    // -----------------------------------------------------------------------
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

        // Check if this is a dot-completion
        if char_before_position(&source, position) == Some('.') {
            let type_name = infer_dot_receiver_type(&source, position, &symbols);
            let methods = match type_name.as_deref() {
                Some("string") => STRING_METHODS,
                Some("list") => LIST_METHODS,
                Some("dict") => DICT_METHODS,
                _ => {
                    // Unknown type — offer all methods
                    for m in STRING_METHODS
                        .iter()
                        .chain(LIST_METHODS.iter())
                        .chain(DICT_METHODS.iter())
                    {
                        items.push(CompletionItem {
                            label: m.to_string(),
                            kind: Some(CompletionItemKind::METHOD),
                            ..Default::default()
                        });
                    }
                    // Deduplicate by label
                    items.sort_by(|a, b| a.label.cmp(&b.label));
                    items.dedup_by(|a, b| a.label == b.label);
                    return Ok(Some(CompletionResponse::Array(items)));
                }
            };
            for m in methods {
                items.push(CompletionItem {
                    label: m.to_string(),
                    kind: Some(CompletionItemKind::METHOD),
                    ..Default::default()
                });
            }
            return Ok(Some(CompletionResponse::Array(items)));
        }

        // Scope-aware: find symbols visible at cursor position
        for sym in &symbols {
            // A symbol is visible if:
            // 1. It has no scope_span (top-level), or
            // 2. The cursor is inside its scope_span
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
            };
            items.push(CompletionItem {
                label: sym.name.clone(),
                kind: Some(kind),
                detail: Some(sym.signature.as_deref().unwrap_or(detail).to_string()),
                ..Default::default()
            });
        }

        // Add builtins
        for &(name, detail) in BUILTINS {
            items.push(CompletionItem {
                label: name.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(detail.to_string()),
                ..Default::default()
            });
        }

        // Add keywords
        for kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    // -----------------------------------------------------------------------
    // Go-to-definition (AST-based symbol table)
    // -----------------------------------------------------------------------
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

        // Look up the name in the symbol table — find the first definition-like symbol
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

    // -----------------------------------------------------------------------
    // Find references (AST-based)
    // -----------------------------------------------------------------------
    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let ast = state.ast.clone();
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

    // -----------------------------------------------------------------------
    // Document symbols (AST-based with proper spans)
    // -----------------------------------------------------------------------
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
            // Only include top-level definitions for document symbols
            let kind = match sym.kind {
                HarnSymbolKind::Pipeline => SymbolKind::FUNCTION,
                HarnSymbolKind::Function => SymbolKind::FUNCTION,
                HarnSymbolKind::Variable => SymbolKind::VARIABLE,
                HarnSymbolKind::Enum => SymbolKind::ENUM,
                HarnSymbolKind::Struct => SymbolKind::STRUCT,
                HarnSymbolKind::Parameter => continue, // skip params from outline
            };
            // Only show top-level and direct-child symbols
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

    // -----------------------------------------------------------------------
    // Hover (with type information)
    // -----------------------------------------------------------------------
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

        // Check builtins first
        if let Some(doc) = builtin_doc(&word) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: doc,
                }),
                range: None,
            }));
        }

        // Check keywords
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
            // If the symbol has a scope_span, check if the cursor byte
            // offset falls within it.
            let in_scope = match sym.scope_span {
                Some(sp) => cursor_offset >= sp.start && cursor_offset <= sp.end,
                None => true, // top-level symbol is always visible
            };
            if !in_scope {
                continue;
            }
            // Prefer the symbol with the narrowest (innermost) scope.
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

            // Show signature if available (functions, pipelines, structs, enums)
            if let Some(ref sig) = sym.signature {
                hover_text.push_str(&format!("```harn\n{sig}\n```\n"));
            } else {
                // For variables/parameters, build a code-block declaration
                // with the type annotation when known.
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
                    };
                    hover_text.push_str(&format!("**{kind_str}** `{}`", sym.name));
                }
            }

            // For functions with a return type, show it below the signature
            // (signatures already include "-> type", so only add for
            // variables/params where the type is a shape and worth expanding).
            if sym.signature.is_none() {
                if let Some(ref ty) = sym.type_info {
                    if matches!(ty, harn_parser::TypeExpr::Shape(_)) {
                        // Already shown in the code block above; add a
                        // human-readable breakdown for complex shapes.
                        let expanded = format_shape_expanded(ty, 0);
                        if !expanded.is_empty() {
                            hover_text.push_str(&format!("\n{expanded}"));
                        }
                    }
                }
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

    // -----------------------------------------------------------------------
    // Signature help (shows parameter info as you type)
    // -----------------------------------------------------------------------
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

        // Walk backwards to find the matching `(` and the function name
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

        // Extract function name before the `(`
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

        // Look up in BUILTINS
        let sig_str = match BUILTINS.iter().find(|(n, _)| *n == name.as_str()) {
            Some((_, sig)) => *sig,
            None => return Ok(None),
        };

        // Parse parameters from "name(param1, param2, ...) -> ret"
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
                parameters: Some(params_list),
                active_parameter: Some(comma_count),
            }],
            active_signature: Some(0),
            active_parameter: Some(comma_count),
        }))
    }

    // -----------------------------------------------------------------------
    // Workspace symbol search (cross-file pipeline/function search)
    // -----------------------------------------------------------------------
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

    // -----------------------------------------------------------------------
    // Code actions (quick-fix for lint diagnostics)
    // -----------------------------------------------------------------------
    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let mut actions = Vec::new();

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(Some(actions)),
        };
        let source = state.source.clone();
        drop(docs);

        for diag in &params.context.diagnostics {
            let msg = &diag.message;

            // --- [mutable-never-reassigned]: replace `var` with `let` ---
            if msg.contains("[mutable-never-reassigned]") {
                // Find the `var` keyword at the start of the diagnostic range
                let offset = lsp_position_to_offset(&source, diag.range.start);
                // Scan forward from the line start to find "var" keyword
                if let Some(var_pos) = source[offset..].find("var") {
                    let abs_pos = offset + var_pos;
                    // Verify it's actually the `var` keyword (next char is space or newline)
                    if abs_pos + 3 <= source.len()
                        && (abs_pos == 0 || !source.as_bytes()[abs_pos - 1].is_ascii_alphanumeric())
                        && (abs_pos + 3 == source.len()
                            || !source.as_bytes()[abs_pos + 3].is_ascii_alphanumeric())
                    {
                        let start = offset_to_position(&source, abs_pos);
                        let end = offset_to_position(&source, abs_pos + 3);
                        let edit_range = Range { start, end };

                        let mut changes = HashMap::new();
                        changes.insert(
                            uri.clone(),
                            vec![TextEdit {
                                range: edit_range,
                                new_text: "let".to_string(),
                            }],
                        );
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: "Change `var` to `let`".to_string(),
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

            // --- [unused-variable] / [unused-parameter]: add `_` prefix ---
            if msg.contains("[unused-variable]") || msg.contains("[unused-parameter]") {
                // Extract the variable name from the message: "variable `foo` is declared..."
                // or "parameter `foo` is declared..."
                if let Some(name) = extract_backtick_name(msg) {
                    // Find the name in the source around the diagnostic range
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

            // --- [comparison-to-bool]: simplify boolean comparison ---
            if msg.contains("[comparison-to-bool]") {
                let offset = lsp_position_to_offset(&source, diag.range.start);
                let end_offset = lsp_position_to_offset(&source, diag.range.end)
                    .max(offset + 1)
                    .min(source.len());
                let expr_text = &source[offset..end_offset];

                // Try to produce a simplified replacement
                let replacement = simplify_bool_comparison(expr_text);
                if let Some(new_text) = replacement {
                    let mut changes = HashMap::new();
                    changes.insert(
                        uri.clone(),
                        vec![TextEdit {
                            range: diag.range,
                            new_text,
                        }],
                    );
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: "Simplify boolean comparison".to_string(),
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

        Ok(Some(actions))
    }

    // -----------------------------------------------------------------------
    // Rename (document-wide symbol rename)
    // -----------------------------------------------------------------------
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
        let ast = state.ast.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let old_name = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        // Verify the name refers to a known symbol (definition or builtin).
        // Builtins should not be renamed.
        if BUILTINS.iter().any(|(n, _)| *n == old_name) {
            return Ok(None);
        }

        // Check that the symbol exists in the symbol table
        let symbol_exists = symbols.iter().any(|s| s.name == old_name);
        if !symbol_exists {
            return Ok(None);
        }

        // Collect all references from the AST
        let program = match ast {
            Some(p) => p,
            None => return Ok(None),
        };
        let ref_spans = find_references(&program, &old_name);
        if ref_spans.is_empty() {
            return Ok(None);
        }

        // For each reference span, find the exact position of the name within
        // the span text. Definition nodes have spans covering the whole
        // declaration, so we search within each span for the identifier token.
        let mut edits = Vec::new();
        let mut seen_offsets = std::collections::HashSet::new();

        // Also scan lexer tokens for precise identifier positions
        let mut lexer = Lexer::new(&source);
        if let Ok(tokens) = lexer.tokenize() {
            for token in &tokens {
                if let TokenKind::Identifier(ref name) = token.kind {
                    if name == &old_name && !seen_offsets.contains(&token.span.start) {
                        // Verify this token falls within one of the reference spans
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

        // Sort edits by position (bottom-up) to avoid offset shifting issues
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

    // -----------------------------------------------------------------------
    // Semantic tokens (lexer-based with symbol table enhancement)
    // -----------------------------------------------------------------------
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
            Err(_) => {
                // On lex error, we can't produce tokens reliably
                return Ok(None);
            }
        };

        let semantic_tokens = build_semantic_tokens(&tokens, &symbols, &source);

        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: semantic_tokens,
        })))
    }
}
