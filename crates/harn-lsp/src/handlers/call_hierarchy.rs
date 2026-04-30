//! Call hierarchy request handlers.

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::call_hierarchy::{
    incoming_call_hierarchy, outgoing_call_hierarchy, prepare_call_hierarchy,
};
use crate::HarnLsp;

impl HarnLsp {
    pub(super) async fn handle_prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        Ok(prepare_call_hierarchy(
            uri,
            &state.source,
            &state.symbols,
            position,
        ))
    }

    pub(super) async fn handle_incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let docs = self.documents.lock().unwrap();
        Ok(incoming_call_hierarchy(&params.item, &docs))
    }

    pub(super) async fn handle_outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let docs = self.documents.lock().unwrap();
        Ok(outgoing_call_hierarchy(&params.item, &docs))
    }
}
