//! Folding range support.

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::folding::build_folding_ranges;
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
        let source = state.source.clone();
        let ast = state.cached_ast.clone();
        drop(docs);

        let ranges = build_folding_ranges(&source, ast.as_deref());
        Ok(Some(ranges))
    }
}
