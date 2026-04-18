//! LSP request handlers, split by concern.
//!
//! `tower_lsp::LanguageServer` must be implemented in a single `impl`
//! block, so this module's job is to define one trait impl that delegates
//! each method to an inherent `handle_*` method on `HarnLsp` defined in a
//! topic-specific submodule.

mod completion;
mod definition;
mod formatting;
mod hover;
mod lifecycle;
mod symbols;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::HarnLsp;

#[tower_lsp::async_trait]
impl tower_lsp::LanguageServer for HarnLsp {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        self.handle_initialize(params).await
    }

    async fn initialized(&self, params: InitializedParams) {
        self.handle_initialized(params).await
    }

    async fn shutdown(&self) -> Result<()> {
        self.handle_shutdown().await
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.handle_did_open(params).await
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        self.handle_did_change(params).await
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.handle_did_close(params).await
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        self.handle_completion(params).await
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        self.handle_goto_definition(params).await
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        self.handle_references(params).await
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        self.handle_rename(params).await
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        self.handle_document_symbol(params).await
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        self.handle_workspace_symbol(params).await
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        self.handle_semantic_tokens_full(params).await
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        self.handle_hover(params).await
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        self.handle_signature_help(params).await
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        self.handle_inlay_hint(params).await
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        self.handle_code_action(params).await
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        self.handle_formatting(params).await
    }
}
