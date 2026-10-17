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
        // Register a workspace file watcher for the sources we serve so
        // external changes (git checkout, another editor, a codegen step)
        // re-validate the open documents that may depend on them. A
        // `.harn.prompt` matches the `**/*.prompt` glob too. Only
        // attempted when the client advertised dynamic-registration
        // support during `initialize`.
        if self
            .watched_files_dynamic_registration
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let options = DidChangeWatchedFilesRegistrationOptions {
                watchers: ["**/*.harn", "**/*.prompt"]
                    .into_iter()
                    .map(|glob| FileSystemWatcher {
                        glob_pattern: GlobPattern::String(glob.to_string()),
                        kind: None,
                    })
                    .collect(),
            };
            let registration = Registration {
                id: "harn-watch-source-files".to_string(),
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
        if let Ok(path) = uri.to_file_path() {
            self.workspace.set_buffer(path, source.clone());
        }
        let state = DocumentState::new_for_language(source, language_id, &uri);
        self.documents.lock().unwrap().insert(uri.clone(), state);
        if self.reanalyze_and_publish(&uri).await {
            self.recheck_importers_of(&uri).await;
        }
    }

    pub(super) async fn handle_did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        if let Some(change) = params.content_changes.into_iter().last() {
            let source = change.text;
            if let Ok(path) = uri.to_file_path() {
                self.workspace.set_buffer(path, source.clone());
            }
            let language_id = self
                .documents
                .lock()
                .unwrap()
                .get(&uri)
                .map(|state| state.language_id.clone())
                .unwrap_or_default();
            let prepared = DocumentState::new_for_language(source, language_id, &uri);
            {
                let mut docs = self.documents.lock().unwrap();
                let entry = docs
                    .entry(uri.clone())
                    .or_insert_with(|| DocumentState::placeholder(&uri));
                entry.update_prepared_source(prepared);
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

            self.reanalyze_and_publish(&uri).await;
            self.recheck_importers_of(&uri).await;
        }
    }

    pub(super) async fn handle_did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.lock().unwrap().remove(&uri);
        if let Ok(path) = uri.to_file_path() {
            // The file on disk is authoritative again for its importers.
            self.workspace.remove_buffer(&path);
        }
        self.recheck_importers_of(&uri).await;
    }

    pub(super) async fn handle_did_change_configuration(
        &self,
        params: DidChangeConfigurationParams,
    ) {
        {
            let mut workspace = self.rule_workspace.lock().unwrap();
            workspace.reconfigure(Some(&params.settings));
        }
        self.workspace.invalidate_all();
        self.reanalyze_all_open().await;
    }

    pub(super) async fn handle_did_change_watched_files(
        &self,
        params: DidChangeWatchedFilesParams,
    ) {
        // An external change to a `.harn` file (git checkout, another editor,
        // a codegen step) can invalidate the diagnostics of open documents
        // that import it, and a changed `harn.toml` changes lint settings. We
        // don't own the on-disk buffer for open documents — the editor's text
        // is authoritative — so we drop the cached graphs and configuration
        // and re-run every open document's analysis.
        if params.changes.is_empty() {
            return;
        }
        self.workspace.invalidate_all();
        self.reanalyze_all_open().await;
    }

    /// Analyze `uri` without holding the document lock, so hover, completion,
    /// and every other open document stay readable while it runs, then
    /// publish its diagnostics. An edit that lands mid-analysis runs another
    /// pass.
    pub(crate) async fn reanalyze_and_publish(&self, uri: &Url) -> bool {
        loop {
            let input_generation = self.workspace.generation();
            let rule_workspace = self.rule_workspace.lock().unwrap().clone();
            let Some(job) = self
                .documents
                .lock()
                .unwrap()
                .get_mut(uri)
                .and_then(DocumentState::begin_analysis)
            else {
                return false;
            };
            let workspace = std::sync::Arc::clone(&self.workspace);
            let job_uri = uri.clone();
            #[cfg(test)]
            let probe = self.analysis_probe.lock().unwrap().clone();
            let analyzed = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                if let Some(probe) = probe {
                    probe();
                }
                let mut job = job;
                job.reparse_if_dirty_with_rules(
                    Some(&job_uri),
                    Some(&rule_workspace),
                    Some(&workspace),
                );
                job
            })
            .await
            .expect("document analysis panicked");
            let diagnostics = {
                let mut docs = self.documents.lock().unwrap();
                let Some(entry) = docs.get_mut(uri) else {
                    return false;
                };
                if !entry.finish_analysis(analyzed, input_generation == self.workspace.generation())
                {
                    continue;
                }
                entry.diagnostics.clone()
            };
            self.client
                .publish_diagnostics(uri.clone(), diagnostics, None)
                .await;
            return true;
        }
    }

    /// Re-check every open document whose module graph reached `uri`, after
    /// `uri`'s text changed.
    async fn recheck_importers_of(&self, uri: &Url) {
        let Ok(path) = uri.to_file_path() else {
            return;
        };
        let importers = self.workspace.invalidate_module(&path);
        if importers.is_empty() {
            return;
        }
        let uris: Vec<Url> = {
            let mut docs = self.documents.lock().unwrap();
            docs.iter_mut()
                .filter(|(open, _)| {
                    open.to_file_path()
                        .is_ok_and(|open_path| importers.contains(&open_path))
                })
                .map(|(open, state)| {
                    state.dirty = true;
                    open.clone()
                })
                .collect()
        };
        for importer in uris {
            self.reanalyze_and_publish(&importer).await;
        }
    }

    async fn reanalyze_all_open(&self) {
        let uris: Vec<Url> = {
            let mut docs = self.documents.lock().unwrap();
            docs.iter_mut()
                .map(|(uri, state)| {
                    state.dirty = true;
                    uri.clone()
                })
                .collect()
        };
        for uri in uris {
            self.reanalyze_and_publish(&uri).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use tower_lsp::lsp_types::*;
    use tower_lsp::LspService;

    use crate::HarnLsp;

    /// Hover, completion, and every other request read the shared document
    /// map. Analysis must not hold it, or one slow file stalls all of them.
    #[tokio::test(flavor = "multi_thread")]
    async fn analysis_runs_without_holding_the_document_lock() {
        let (service, _socket) = LspService::new(HarnLsp::new);
        let server = service.inner();
        let documents = Arc::clone(&server.documents);
        let analyses = Arc::new(AtomicUsize::new(0));
        let lock_was_free = Arc::new(AtomicBool::new(true));
        let probe: Arc<dyn Fn() + Send + Sync> = {
            let analyses = Arc::clone(&analyses);
            let lock_was_free = Arc::clone(&lock_was_free);
            Arc::new(move || {
                analyses.fetch_add(1, Ordering::SeqCst);
                if documents.try_lock().is_err() {
                    lock_was_free.store(false, Ordering::SeqCst);
                }
            })
        };

        // The probe can see a held lock, so a pass below is not vacuous.
        {
            let _held = server.documents.lock().unwrap();
            probe();
            assert!(!lock_was_free.swap(true, Ordering::SeqCst));
        }
        analyses.store(0, Ordering::SeqCst);

        *server.analysis_probe.lock().unwrap() = Some(probe);
        let uri = Url::parse("file:///analysis-lock/main.harn").unwrap();
        server
            .handle_did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "harn".to_string(),
                    version: 1,
                    text: "pipeline main() {\n  log(1)\n}\n".to_string(),
                },
            })
            .await;

        assert_eq!(analyses.load(Ordering::SeqCst), 1);
        assert!(
            lock_was_free.load(Ordering::SeqCst),
            "analysis ran while the document lock was held"
        );
        let docs = server.documents.lock().unwrap();
        let state = docs.get(&uri).expect("document stays open");
        assert!(!state.dirty);
        assert!(
            state.cached_ast.is_some(),
            "the detached analysis was installed"
        );
    }

    #[tokio::test]
    async fn syntax_features_are_ready_while_semantic_analysis_is_pending() {
        let (service, _socket) = LspService::new(HarnLsp::new);
        let server = service.inner();
        let reached = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        *server.analysis_probe.lock().unwrap() = Some({
            let reached = Arc::clone(&reached);
            let gate = Arc::clone(&gate);
            Arc::new(move || {
                reached.notify_one();
                let (ready, wake) = &*gate;
                let mut ready = ready.lock().unwrap();
                while !*ready {
                    ready = wake.wait(ready).unwrap();
                }
            })
        });
        let uri = Url::parse("file:///syntax-ready/main.harn").unwrap();
        let text = "pub fn helper(value) {\n  return value\n}\npipeline main() {\n  helper(1)\n}\n";
        let opening = server.handle_did_open(open(&uri, text));
        let requesting = async {
            reached.notified().await;
            let folds = server
                .handle_folding_range(FoldingRangeParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                })
                .await
                .unwrap()
                .unwrap();
            let hierarchy = server
                .handle_prepare_call_hierarchy(CallHierarchyPrepareParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: Position::new(0, 8),
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                })
                .await
                .unwrap();
            let pending = server.documents.lock().unwrap()[&uri].dirty;
            let (ready, wake) = &*gate;
            *ready.lock().unwrap() = true;
            wake.notify_one();
            (folds, hierarchy, pending)
        };
        let (_, (folds, hierarchy, pending)) = tokio::join!(opening, requesting);
        assert!(
            pending,
            "the requests must reach the pending-analysis state"
        );
        assert!(
            folds
                .iter()
                .any(|fold| fold.start_line == 3 && fold.end_line == 5),
            "{folds:?}"
        );
        assert_eq!(hierarchy.unwrap()[0].name, "helper");
    }

    fn open(uri: &Url, text: &str) -> DidOpenTextDocumentParams {
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "harn".to_string(),
                version: 1,
                text: text.to_string(),
            },
        }
    }

    fn type_errors(server: &HarnLsp, uri: &Url) -> Vec<String> {
        server.documents.lock().unwrap()[uri]
            .diagnostics
            .iter()
            .filter(|d| d.source.as_deref() == Some("harn-typecheck"))
            .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
            .map(|d| d.message.clone())
            .collect()
    }

    #[tokio::test]
    async fn a_reopened_document_keeps_its_own_detached_analysis() {
        let (service, _socket) = LspService::new(HarnLsp::new);
        let server = service.inner();
        let uri = Url::parse("file:///analysis-reopen/main.harn").unwrap();
        let reached = Arc::new([tokio::sync::Notify::new(), tokio::sync::Notify::new()]);
        let gates = Arc::new([
            (std::sync::Mutex::new(false), std::sync::Condvar::new()),
            (std::sync::Mutex::new(false), std::sync::Condvar::new()),
        ]);
        let analyses = Arc::new(AtomicUsize::new(0));
        *server.analysis_probe.lock().unwrap() = Some({
            let reached = Arc::clone(&reached);
            let gates = Arc::clone(&gates);
            let analyses = Arc::clone(&analyses);
            Arc::new(move || {
                let index = analyses.fetch_add(1, Ordering::SeqCst);
                if let Some((ready, wake)) = gates.get(index) {
                    reached[index].notify_one();
                    let mut ready = ready.lock().unwrap();
                    while !*ready {
                        ready = wake.wait(ready).unwrap();
                    }
                }
            })
        });
        let release = |index: usize| {
            let (ready, wake) = &gates[index];
            *ready.lock().unwrap() = true;
            wake.notify_one();
        };
        let old_finished = tokio::sync::Notify::new();
        let original = async {
            server
                .handle_did_open(open(&uri, "pub fn value() -> int { return 1 }\n"))
                .await;
            old_finished.notify_one();
        };
        let replacement = async {
            reached[0].notified().await;
            server
                .handle_did_close(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                })
                .await;
            let text = "pub fn value() -> int { return \"new\" }\n";
            let reopen = server.handle_did_open(open(&uri, text));
            let finish_original = async {
                reached[1].notified().await;
                release(0);
                old_finished.notified().await;
                let still_pending = {
                    let docs = server.documents.lock().unwrap();
                    let state = &docs[&uri];
                    state.dirty
                        && state.cached_ast.is_some()
                        && &*state.source == text
                        && state.type_diagnostics.is_empty()
                };
                // Release before asserting so a failed control cannot strand
                // a blocking analysis worker inside the test runtime.
                release(1);
                still_pending
            };
            tokio::join!(reopen, finish_original).1
        };
        let (_, still_pending) = tokio::join!(original, replacement);
        assert!(
            still_pending,
            "the old job released or installed over the new job"
        );
        assert_eq!(analyses.load(Ordering::SeqCst), 2);
        assert_eq!(type_errors(server, &uri).len(), 1);
        assert!(!server.documents.lock().unwrap()[&uri].dirty);
    }

    /// Editing an imported module re-checks the open documents that import
    /// it, against the edited buffer rather than the file on disk.
    #[tokio::test(start_paused = true)]
    async fn editing_an_imported_module_rechecks_its_importers() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("lib.harn");
        let app_path = dir.path().join("app.harn");
        let lib_source = "pub fn helper() -> int {\n  return 1\n}\n";
        let app_source =
            "import { helper } from \"./lib\"\n\npub fn total() -> int {\n  const n: int = helper()\n  return n\n}\n";
        std::fs::write(&lib_path, lib_source).unwrap();
        std::fs::write(&app_path, app_source).unwrap();
        let lib = Url::from_file_path(&lib_path).unwrap();
        let app = Url::from_file_path(&app_path).unwrap();

        let (service, _socket) = LspService::new(HarnLsp::new);
        let server = service.inner();
        server.handle_did_open(open(&lib, lib_source)).await;
        server.handle_did_open(open(&app, app_source)).await;
        assert_eq!(type_errors(server, &app), Vec::<String>::new());

        server
            .handle_did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: lib.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "pub fn helper() -> string {\n  return \"one\"\n}\n".to_string(),
                }],
            })
            .await;
        let errors = type_errors(server, &app);
        assert!(
            !errors.is_empty(),
            "the importer must see the unsaved return type change"
        );
    }
}
