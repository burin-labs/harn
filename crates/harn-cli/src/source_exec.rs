//! Turning a `.harn` source file into a running program: parsing it,
//! building the harness it executes against, and the staged error type that
//! reports where execution stopped.

use std::future::Future;

use crate::*;

/// Exits on error.
pub(crate) fn parse_source_file(path: &str) -> (String, Vec<harn_parser::SNode>) {
    ensure_builtin_signatures_installed();

    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {path}: {e}");
            process::exit(1);
        }
    };

    let mut lexer = Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => {
            let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                &source,
                path,
                &error_span_from_lex(&e),
                "error",
                harn_parser::diagnostic::lexer_error_code(&e),
                &e.to_string(),
                Some("here"),
                None,
            );
            eprint!("{diagnostic}");
            process::exit(1);
        }
    };

    let mut parser = Parser::new(tokens);
    let program = match parser.parse() {
        Ok(p) => p,
        Err(err) => {
            if parser.all_errors().is_empty() {
                let span = error_span_from_parse(&err);
                let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                    &source,
                    path,
                    &span,
                    "error",
                    harn_parser::diagnostic::parser_error_code(&err),
                    &harn_parser::diagnostic::parser_error_message(&err),
                    Some(harn_parser::diagnostic::parser_error_label(&err)),
                    harn_parser::diagnostic::parser_error_help(&err),
                );
                eprint!("{diagnostic}");
            } else {
                for e in parser.all_errors() {
                    let span = error_span_from_parse(e);
                    let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                        &source,
                        path,
                        &span,
                        "error",
                        harn_parser::diagnostic::parser_error_code(e),
                        &harn_parser::diagnostic::parser_error_message(e),
                        Some(harn_parser::diagnostic::parser_error_label(e)),
                        harn_parser::diagnostic::parser_error_help(e),
                    );
                    eprint!("{diagnostic}");
                }
            }
            process::exit(1);
        }
    };

    (source, program)
}

pub(crate) fn error_span_from_lex(e: &harn_lexer::LexerError) -> harn_lexer::Span {
    match e {
        harn_lexer::LexerError::UnexpectedCharacter(_, span)
        | harn_lexer::LexerError::UnterminatedString(span)
        | harn_lexer::LexerError::IntegerLiteralOutOfRange(_, span)
        | harn_lexer::LexerError::UnterminatedBlockComment(span) => *span,
    }
}

pub(crate) fn error_span_from_parse(e: &harn_parser::ParserError) -> harn_lexer::Span {
    match e {
        harn_parser::ParserError::Unexpected { span, .. } => *span,
        harn_parser::ParserError::UnexpectedEof { span, .. } => *span,
    }
}

/// The pipeline stage at which an `execute_*` call failed. Callers (the
/// conformance harness, the REPL) use this to label a failure accurately
/// instead of calling every failure a "runtime error" — a parse, typecheck,
/// or compile failure never reaches the VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecStage {
    Parse,
    Typecheck,
    Compile,
    Runtime,
}

impl ExecStage {
    /// Human-facing label for this stage, e.g. `"type error"`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            ExecStage::Parse => "parse error",
            ExecStage::Typecheck => "type error",
            ExecStage::Compile => "compile error",
            ExecStage::Runtime => "runtime error",
        }
    }
}

/// An `execute_*` failure tagged with the stage it came from. `Display`
/// renders only the bare message (matching the historical `String` error),
/// so callers that just print `{e}` are unaffected; the `stage` is available
/// for callers that want to label the failure.
#[derive(Debug, Clone)]
pub(crate) struct ExecError {
    pub(crate) stage: ExecStage,
    pub(crate) message: String,
}

impl ExecError {
    pub(crate) fn new(stage: ExecStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Used by REPL and conformance tests.
pub(crate) async fn execute(source: &str, source_path: Option<&Path>) -> Result<String, ExecError> {
    execute_with_skill_dirs(source, source_path, &[]).await
}

pub(crate) async fn execute_with_skill_dirs(
    source: &str,
    source_path: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
) -> Result<String, ExecError> {
    execute_with_skill_dirs_and_options(
        source,
        source_path,
        cli_skill_dirs,
        SourceExecutionOptions::default(),
    )
    .await
}

/// Host-owned configuration for one source execution.
///
/// Keeping the capability ceiling beside the harness makes embedders pass
/// execution authority structurally. The VM installs it inside the `LocalSet`
/// that owns the run, rather than relying on a thread-local guard surviving an
/// executor boundary.
#[derive(Default)]
pub(crate) struct SourceExecutionOptions {
    pub(crate) harness: Option<harn_vm::Harness>,
    pub(crate) execution_policy: Option<harn_vm::orchestration::CapabilityPolicy>,
}

async fn scope_source_execution<F: Future>(
    policy: Option<harn_vm::orchestration::CapabilityPolicy>,
    inner: F,
) -> F::Output {
    match policy {
        Some(policy) => harn_vm::orchestration::scope_execution_policy(policy, inner).await,
        None => inner.await,
    }
}

pub(crate) async fn execute_with_skill_dirs_and_options(
    source: &str,
    source_path: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
    options: SourceExecutionOptions,
) -> Result<String, ExecError> {
    let SourceExecutionOptions {
        harness,
        execution_policy,
    } = options;
    let mut lexer = Lexer::new(source);
    let tokens = lexer
        .tokenize()
        .map_err(|e| ExecError::new(ExecStage::Parse, e.to_string()))?;
    let mut parser = Parser::new(tokens);
    let program = parser
        .parse()
        .map_err(|e| ExecError::new(ExecStage::Parse, e.to_string()))?;

    // Static cross-module resolution: when executed from a file, derive the
    // import graph so `execute` catches undefined calls at typecheck time.
    // The REPL / `-e` path invokes this without `source_path`, where there
    // is no importing file context; we fall back to no-imports checking.
    let mut checker = TypeChecker::new();
    if let Some(path) = source_path {
        checker = crate::typecheck_imports::checker_with_resolved_imports(checker, path);
    }
    let type_diagnostics = checker.check(&program);
    let mut warning_lines = Vec::new();
    for diag in &type_diagnostics {
        match diag.severity {
            DiagnosticSeverity::Error => {
                return Err(ExecError::new(ExecStage::Typecheck, diag.message.clone()))
            }
            DiagnosticSeverity::Warning => {
                warning_lines.push(format!("warning: {}", diag.message));
            }
        }
    }

    let compiler = source_path
        .map(|path| compiler_for_source(path, source))
        .unwrap_or_default();
    let chunk = compiler
        .compile(&program)
        .map_err(|e| ExecError::new(ExecStage::Compile, e.to_string()))?;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(scope_source_execution(execution_policy, async {
            let mut vm = harn_vm::Vm::new();
            harn_vm::register_vm_stdlib(&mut vm);
            install_default_hostlib(&mut vm);
            // Compiling for trusted host dispatch only lowers the call; the VM
            // still refuses it at runtime unless the same authority is enabled
            // here. This must happen before the first import, which is why it
            // sits immediately after stdlib registration.
            if let Some(path) = source_path {
                crate::compiler_context::enable_trusted_host_dispatch_for_source(&mut vm, path)
                    .map_err(|error| {
                        ExecError::new(
                            ExecStage::Runtime,
                            format!("failed to enable trusted host dispatch: {error}"),
                        )
                    })?;
            }
            let source_parent = source_path
                .and_then(|p| p.parent())
                .unwrap_or(std::path::Path::new("."));
            let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
            let store_base = project_root.as_deref().unwrap_or(source_parent);
            let execution_cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .to_string_lossy()
                .into_owned();
            let source_dir = source_parent.to_string_lossy().into_owned();
            if source_path.is_some_and(is_conformance_path) {
                harn_vm::event_log::install_memory_for_current_thread(64);
            }
            harn_vm::register_store_builtins(&mut vm, store_base);
            harn_vm::register_metadata_builtins(&mut vm, store_base);
            let pipeline_name = source_path
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .unwrap_or("default");
            harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
            harn_vm::stdlib::process::set_thread_execution_context(Some(
                harn_vm::orchestration::RunExecutionRecord {
                    cwd: Some(execution_cwd),
                    project_root: project_root
                        .as_ref()
                        .map(|root| root.to_string_lossy().into_owned()),
                    source_dir: Some(source_dir),
                    ..Default::default()
                },
            ));
            if let Some(ref root) = project_root {
                vm.set_project_root(root);
            }
            if let Some(path) = source_path {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        vm.set_source_dir(parent);
                    }
                }
            }
            // Conformance tests land here via `run_conformance_tests`; for
            // `skill_fs_*` fixtures to see the bundled `skills/` folder
            // we run the same layered discovery as `harn run`.
            let loaded = skill_loader::load_skills(&skill_loader::SkillLoaderInputs {
                cli_dirs: cli_skill_dirs.to_vec(),
                source_path: source_path.map(Path::to_path_buf),
            });
            skill_loader::emit_loader_warnings(&loaded.loader_warnings);
            skill_loader::install_skills_global(&mut vm, &loaded);
            let runtime_harness = match harness {
                Some(harness) => harness,
                None => {
                    let resolved = match source_path {
                        Some(path) => default_harness_for_manifest_or_base_dir(path, store_base),
                        None => default_harness_for_base_dir(store_base),
                    };
                    resolved.map_err(|error| {
                        ExecError::new(
                            ExecStage::Runtime,
                            format!("failed to configure harness secret provider: {error}"),
                        )
                    })
                }?,
            };
            vm.set_harness(runtime_harness);
            let extensions = source_path
                .map(package::load_runtime_extensions)
                .unwrap_or_default();
            if source_path.is_some() {
                package::install_runtime_extensions(&extensions);
                package::install_manifest_triggers(&mut vm, &extensions)
                    .await
                    .map_err(|error| {
                        ExecError::new(
                            ExecStage::Runtime,
                            format!("failed to install manifest triggers: {error}"),
                        )
                    })?;
                package::install_manifest_hooks(&mut vm, &extensions)
                    .await
                    .map_err(|error| {
                        ExecError::new(
                            ExecStage::Runtime,
                            format!("failed to install manifest hooks: {error}"),
                        )
                    })?;
            }
            let _event_log = harn_vm::event_log::active_event_log()
                .unwrap_or_else(|| harn_vm::event_log::install_memory_for_current_thread(64));
            let _connector_clients =
                if should_install_default_connector_clients(source, source_path) {
                    Some(
                        install_connector_clients(store_base, &extensions.provider_connectors)
                            .await
                            .map_err(|error| {
                                ExecError::new(
                                    ExecStage::Runtime,
                                    format!("failed to initialize connector clients: {error}"),
                                )
                            })?,
                    )
                } else {
                    None
                };
            let execution_result = vm
                .execute(&chunk)
                .await
                .map_err(|e| ExecError::new(ExecStage::Runtime, e.to_string()));
            harn_vm::egress::reset_egress_policy_for_host();
            harn_vm::stdlib::process::set_thread_execution_context(None);
            execution_result?;
            let mut output = String::new();
            for wl in &warning_lines {
                output.push_str(wl);
                output.push('\n');
            }
            output.push_str(vm.output());
            Ok(output)
        }))
        .await
}

pub(crate) fn should_install_default_connector_clients(
    source: &str,
    source_path: Option<&Path>,
) -> bool {
    if !source_path.is_some_and(is_conformance_path) {
        return true;
    }
    source.contains("connector_call")
        || source.contains("std/connectors")
        || source.contains("connectors/")
}

pub(crate) fn is_conformance_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "conformance")
}

pub(crate) struct ActiveConnectorClientsGuard;

impl Drop for ActiveConnectorClientsGuard {
    fn drop(&mut self) {
        harn_vm::clear_active_connector_clients();
    }
}

pub(crate) async fn install_connector_clients(
    base_dir: &Path,
    provider_connectors: &[package::ResolvedProviderConnectorConfig],
) -> Result<ActiveConnectorClientsGuard, String> {
    let event_log = harn_vm::event_log::active_event_log()
        .unwrap_or_else(|| harn_vm::event_log::install_memory_for_current_thread(64));
    let secret_namespace = connector_secret_namespace(base_dir);
    let secrets: Arc<dyn harn_vm::secrets::SecretProvider> = Arc::new(
        harn_vm::secrets::configured_default_chain(secret_namespace)
            .map_err(|error| format!("failed to configure secret providers: {error}"))?,
    );

    let mut registry = harn_vm::ConnectorRegistry::default();
    for config in provider_connectors {
        if let Some(connector) = package::load_provider_connector(config)
            .await
            .map_err(|error| error.to_string())?
        {
            registry.remove(&config.id);
            registry
                .register(connector)
                .map_err(|error| error.to_string())?;
        }
    }
    let metrics = Arc::new(harn_vm::MetricsRegistry::default());
    let inbox = Arc::new(
        harn_vm::InboxIndex::new(event_log.clone(), metrics.clone())
            .await
            .map_err(|error| error.to_string())?,
    );
    registry
        .init_all(harn_vm::ConnectorCtx {
            event_log,
            secrets,
            inbox,
            metrics,
            rate_limiter: Arc::new(harn_vm::RateLimiterFactory::default()),
        })
        .await
        .map_err(|error| error.to_string())?;
    let clients = registry.client_map().await;
    harn_vm::install_active_connector_clients(clients);
    Ok(ActiveConnectorClientsGuard)
}

pub(crate) fn connector_secret_namespace(base_dir: &Path) -> String {
    match std::env::var("HARN_SECRET_NAMESPACE") {
        Ok(namespace) if !namespace.trim().is_empty() => namespace,
        _ => {
            let leaf = base_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("workspace");
            format!("harn/{leaf}")
        }
    }
}

pub(crate) fn default_harness_for_base_dir(base_dir: &Path) -> Result<harn_vm::Harness, String> {
    let secret_namespace = connector_secret_namespace(base_dir);
    default_harness_for_secret_namespace(secret_namespace)
}

pub(crate) fn default_harness_for_manifest_or_base_dir(
    source_path: &Path,
    base_dir: &Path,
) -> Result<harn_vm::Harness, String> {
    let secret_namespace = package::find_nearest_manifest_dir(source_path)
        .map(|manifest_dir| connector_secret_namespace(&manifest_dir))
        .unwrap_or_else(|| connector_secret_namespace(base_dir));
    default_harness_for_secret_namespace(secret_namespace)
}

pub(crate) fn default_harness_for_secret_namespace(
    secret_namespace: String,
) -> Result<harn_vm::Harness, String> {
    let secret_provider = Arc::new(
        harn_vm::secrets::configured_default_chain(secret_namespace)
            .map_err(|error| error.to_string())?,
    );
    // Testbench installs its clock before entering the canonical CLI run
    // boundary. Bind that clock into the newly constructed Harness explicitly
    // so capability methods, legacy runtime internals, and tape recording all
    // observe one timeline. Production has no active override and receives the
    // ordinary real clock.
    let harness = match harn_vm::clock_mock::active_mock_clock() {
        Some(clock) => harn_vm::Harness::with_clock(clock.auto_advancing()),
        None => harn_vm::Harness::real(),
    };
    Ok(harness.with_secret_provider(secret_provider))
}

#[cfg(test)]
mod tests {
    use super::{default_harness_for_secret_namespace, should_install_default_connector_clients};
    use std::path::Path;

    #[test]
    fn conformance_skips_connector_clients_unless_fixture_uses_connectors() {
        let path = Path::new("conformance/tests/language/basic.harn");
        assert!(!should_install_default_connector_clients(
            "__io_println(1)",
            Some(path)
        ));
        assert!(!should_install_default_connector_clients(
            "trust_graph_verify_chain()",
            Some(path)
        ));
        assert!(should_install_default_connector_clients(
            "import { post_message } from \"std/connectors/slack\"",
            Some(path)
        ));
        assert!(should_install_default_connector_clients(
            "__io_println(1)",
            Some(Path::new("examples/demo.harn"))
        ));
    }

    #[test]
    fn default_harness_binds_the_active_testbench_clock() {
        let start_ms = 1_700_000_000_000;
        let _clock = harn_vm::clock_mock::install_override(
            harn_vm::clock_mock::MockClock::at_wall_ms(start_ms),
        );
        let harness = default_harness_for_secret_namespace("harn/test".to_string()).unwrap();

        assert_eq!(
            harness.clock().clock().now_utc().unix_timestamp_nanos() / 1_000_000,
            i128::from(start_ms)
        );
    }
}
