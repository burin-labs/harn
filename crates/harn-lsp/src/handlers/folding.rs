//! Folding range support.

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::document_kind::DocumentKind;
use crate::folding::{build_folding_ranges, build_prompt_folding_ranges};
use crate::HarnLsp;

impl HarnLsp {
    pub(super) async fn handle_folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let kind = state.kind;
        let source = state.source.clone();
        let ast = state.cached_ast.clone();
        let outline = state.prompt_outline.clone();
        drop(docs);

        let ranges = match kind {
            DocumentKind::Harn => build_folding_ranges(&source, ast.as_deref()),
            DocumentKind::Prompt => build_prompt_folding_ranges(&source, &outline),
            // Harn's lexer has nothing true to say about a document in
            // another language.
            DocumentKind::Other => Vec::new(),
        };
        Ok(Some(ranges))
    }
}
