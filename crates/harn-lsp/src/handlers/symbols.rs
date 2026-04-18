//! Document symbols, workspace symbol search, and semantic tokens.

use harn_lexer::Lexer;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::helpers::span_to_full_range;
use crate::semantic_tokens::build_semantic_tokens;
use crate::symbols::HarnSymbolKind;
use crate::HarnLsp;

impl HarnLsp {
    #[allow(deprecated)]
    pub(super) async fn handle_document_symbol(
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

    pub(super) async fn handle_workspace_symbol(
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

    pub(super) async fn handle_semantic_tokens_full(
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
}
