//! Server lifecycle and document-sync notifications: `initialize`,
//! `initialized`, `shutdown`, `did_open`, `did_change`, `did_close`.

use std::time::Duration;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::document::DocumentState;
use crate::semantic_tokens::semantic_token_legend;
use crate::HarnLsp;

impl HarnLsp {
    pub(super) async fn handle_initialize(
        &self,
        _params: InitializeParams,
    ) -> Result<InitializeResult> {
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

    pub(super) async fn handle_initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Harn LSP initialized")
            .await;
    }

    pub(super) async fn handle_shutdown(&self) -> Result<()> {
        Ok(())
    }

    pub(super) async fn handle_did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let source = params.text_document.text.clone();

        let state = DocumentState::new(source);
        let diagnostics = state.diagnostics.clone();
        self.documents.lock().unwrap().insert(uri.clone(), state);

        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    pub(super) async fn handle_did_change(&self, params: DidChangeTextDocumentParams) {
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

    pub(super) async fn handle_did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .lock()
            .unwrap()
            .remove(&params.text_document.uri);
    }
}
