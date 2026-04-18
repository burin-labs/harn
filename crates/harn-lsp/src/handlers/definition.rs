//! Go-to-definition, find-references, and rename.

use std::collections::HashMap;

use harn_lexer::{Lexer, TokenKind};
use harn_modules::DefKind;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::constants::BUILTINS;
use crate::helpers::{offset_to_position, span_to_full_range, word_at_position};
use crate::references::find_references;
use crate::symbols::HarnSymbolKind;
use crate::HarnLsp;

impl HarnLsp {
    pub(super) async fn handle_goto_definition(
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

    pub(super) async fn handle_references(
        &self,
        params: ReferenceParams,
    ) -> Result<Option<Vec<Location>>> {
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

    pub(super) async fn handle_rename(
        &self,
        params: RenameParams,
    ) -> Result<Option<WorkspaceEdit>> {
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
}

/// Resolve the symbol through the current document's imported modules using
/// `harn-modules`, and return its definition location when available.
///
/// `harn_modules::build` recursively follows import paths, so seeding it
/// with the current file is enough to discover every module reachable via
/// imports.
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
