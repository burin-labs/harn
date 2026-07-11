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
        params: InitializeParams,
    ) -> Result<InitializeResult> {
        *self.rule_workspace.lock().unwrap() =
            crate::rules::RuleWorkspace::from_initialize(&params);
        // Remember whether the client can dynamically register the
        // `workspace/didChangeWatchedFiles` capability; we act on it in
        // `initialized`.
        let supports_watched_files = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.did_change_watched_files.as_ref())
            .and_then(|d| d.dynamic_registration)
            .unwrap_or(false);
        self.watched_files_dynamic_registration
            .store(supports_watched_files, std::sync::atomic::Ordering::Relaxed);
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    // Completion items carry only a label/detail up front;
                    // the (potentially large) builtin/keyword documentation
                    // markdown is attached lazily via `completionItem/resolve`.
                    resolve_provider: Some(true),
                    ..Default::default()
                }),
                definition_provider: Some(OneOf::Left(true)),
                declaration_provider: Some(DeclarationCapability::Simple(true)),
                type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
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
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        // Advertise both per-diagnostic quick fixes and a
                        // bulk `source.fixAll.harn` action so VS Code's
                        // `editor.codeActionsOnSave` can apply every safe
                        // autofix on save without prompting for each one.
                        code_action_kinds: Some(vec![
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::SOURCE_FIX_ALL,
                            CodeActionKind::new("source.fixAll.harn"),
                        ]),
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    // `harn.applyRepair` resolves a `data.repair_id` from a
                    // code action back into a `WorkspaceEdit`. It exists so a
                    // repair-backed code action that ships *without* an inline
                    // `edit` (the rule-engine codemod path, where precomputing
                    // every fix is too expensive) can still be applied: the
                    // client calls `workspace/executeCommand` and the server
                    // returns the edit on demand.
                    commands: vec![crate::handlers::APPLY_REPAIR_COMMAND.to_string()],
                    work_done_progress_options: Default::default(),
                }),
                rename_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                document_on_type_formatting_provider: Some(DocumentOnTypeFormattingOptions {
                    first_trigger_character: ";".to_string(),
                    more_trigger_character: Some(vec!["}".to_string()]),
                }),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
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
        // Register a workspace file watcher for `.harn` sources so external
        // changes (git checkout, another editor, a codegen step) re-validate
        // the open documents that may depend on them. Only attempted when the
        // client advertised dynamic-registration support during `initialize`.
        if self
            .watched_files_dynamic_registration
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let options = DidChangeWatchedFilesRegistrationOptions {
                watchers: vec![FileSystemWatcher {
                    glob_pattern: GlobPattern::String("**/*.harn".to_string()),
                    kind: None,
                }],
            };
            let registration = Registration {
                id: "harn-watch-harn-files".to_string(),
                method: "workspace/didChangeWatchedFiles".to_string(),
                register_options: serde_json::to_value(options).ok(),
            };
            if let Err(err) = self.client.register_capability(vec![registration]).await {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("Harn LSP: failed to register file watcher: {err}"),
                    )
                    .await;
            }
        }

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
        let language_id = params.text_document.language_id.clone();

        let rule_workspace = self.rule_workspace.lock().unwrap().clone();
        let state =
            DocumentState::new_for_language_with_rules(source, language_id, &uri, &rule_workspace);
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
                let rule_workspace = self.rule_workspace.lock().unwrap().clone();
                entry.reparse_if_dirty_with_rules(Some(&uri), Some(&rule_workspace));
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

    pub(super) async fn handle_did_change_configuration(
        &self,
        params: DidChangeConfigurationParams,
    ) {
        let rule_workspace = {
            let mut workspace = self.rule_workspace.lock().unwrap();
            workspace.reconfigure(Some(&params.settings));
            workspace.clone()
        };

        let updates = {
            let mut docs = self.documents.lock().unwrap();
            docs.iter_mut()
                .map(|(uri, state)| {
                    state.dirty = true;
                    state.reparse_if_dirty_with_rules(Some(uri), Some(&rule_workspace));
                    (uri.clone(), state.diagnostics.clone())
                })
                .collect::<Vec<_>>()
        };

        for (uri, diagnostics) in updates {
            self.client
                .publish_diagnostics(uri, diagnostics, None)
                .await;
        }
    }

    pub(super) async fn handle_did_change_watched_files(
        &self,
        params: DidChangeWatchedFilesParams,
    ) {
        // An external change to a `.harn` file (git checkout, another editor,
        // a codegen step) can invalidate the diagnostics of open documents
        // that import it. We don't own the on-disk buffer for open documents
        // — the editor's text is authoritative — so we don't re-read files
        // here; instead we mark every open document dirty and re-run its
        // analysis so cross-file diagnostics stop being stale. Ignore the
        // notification entirely when nothing is open.
        if params.changes.is_empty() {
            return;
        }
        let rule_workspace = self.rule_workspace.lock().unwrap().clone();
        let updates = {
            let mut docs = self.documents.lock().unwrap();
            if docs.is_empty() {
                return;
            }
            docs.iter_mut()
                .map(|(uri, state)| {
                    state.dirty = true;
                    state.reparse_if_dirty_with_rules(Some(uri), Some(&rule_workspace));
                    (uri.clone(), state.diagnostics.clone())
                })
                .collect::<Vec<_>>()
        };

        for (uri, diagnostics) in updates {
            self.client
                .publish_diagnostics(uri, diagnostics, None)
                .await;
        }
    }
}
