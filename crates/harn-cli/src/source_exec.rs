//! Turning a `.harn` source file into a running program: parsing it,
//! building the harness it executes against, and the staged error type that
//! reports where execution stopped.

use std::collections::{BTreeMap, BTreeSet};
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
                    default_harness().map_err(|error| {
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
                        install_connector_clients_for_vm(&mut vm, &extensions.provider_connectors)
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

/// The credentials a provider's manifest declared it needs, as secret ids the
/// runtime resolves at dispatch.
pub(crate) fn declared_connector_secrets(
    config: &package::ResolvedProviderConnectorConfig,
) -> Vec<harn_vm::secrets::SecretId> {
    let Some(setup) = config.setup.as_ref() else {
        return Vec::new();
    };
    harn_vm::declared_secret_ids(setup.required_secrets.iter().map(String::as_str))
}

pub(crate) async fn install_connector_clients_for_vm(
    vm: &mut harn_vm::Vm,
    provider_connectors: &[package::ResolvedProviderConnectorConfig],
) -> Result<harn_vm::ActiveConnectorClientsGuard, String> {
    let clients = initialized_connector_clients(provider_connectors).await?;
    vm.set_connector_clients(harn_vm::VmConnectorClients::new(clients.clone(), None));
    Ok(harn_vm::scope_active_connector_clients(clients))
}

async fn initialized_connector_clients(
    provider_connectors: &[package::ResolvedProviderConnectorConfig],
) -> Result<BTreeMap<harn_vm::ProviderId, Arc<dyn harn_vm::ConnectorClient>>, String> {
    let registry = build_connector_registry(provider_connectors).await?;
    initialized_registry_clients(&registry, connector_context().await?).await
}

async fn connector_context() -> Result<harn_vm::ConnectorCtx, String> {
    let event_log = harn_vm::event_log::active_event_log()
        .unwrap_or_else(|| harn_vm::event_log::install_memory_for_current_thread(64));
    connector_context_for_event_log(event_log).await
}

async fn connector_context_for_event_log(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
) -> Result<harn_vm::ConnectorCtx, String> {
    let secrets: Arc<dyn harn_vm::secrets::SecretProvider> = Arc::new(
        harn_vm::secrets::configured_secret_chain()
            .map_err(|error| format!("failed to configure secret providers: {error}"))?,
    );
    let metrics = Arc::new(harn_vm::MetricsRegistry::default());
    let inbox = Arc::new(
        harn_vm::InboxIndex::new(event_log.clone(), metrics.clone())
            .await
            .map_err(|error| error.to_string())?,
    );
    Ok(harn_vm::ConnectorCtx {
        event_log,
        secrets,
        inbox,
        metrics,
        rate_limiter: Arc::new(harn_vm::RateLimiterFactory::default()),
    })
}

async fn initialized_registry_clients(
    registry: &harn_vm::ConnectorRegistry,
    context: harn_vm::ConnectorCtx,
) -> Result<BTreeMap<harn_vm::ProviderId, Arc<dyn harn_vm::ConnectorClient>>, String> {
    registry
        .init_all(context)
        .await
        .map_err(|error| error.to_string())?;
    Ok(registry.client_map().await)
}

struct ProjectConnectorResolver {
    anchor: PathBuf,
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    state: tokio::sync::Mutex<ProjectConnectorState>,
}

#[derive(Default)]
struct ProjectConnectorState {
    root_connectors: Option<package::ResolvedProviderConnectors>,
    package_connectors: Option<package::ResolvedProviderConnectors>,
    context: Option<harn_vm::ConnectorCtx>,
    clients: BTreeMap<harn_vm::ProviderId, Arc<dyn harn_vm::ConnectorClient>>,
    initialized_project_providers: BTreeSet<harn_vm::ProviderId>,
    terminal_error: Option<harn_vm::ClientError>,
    package_error: Option<harn_vm::ClientError>,
    provider_errors: BTreeMap<harn_vm::ProviderId, harn_vm::ClientError>,
}

impl ProjectConnectorResolver {
    fn new(anchor: &Path) -> Self {
        let absolute = if anchor.is_absolute() {
            anchor.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(anchor))
                .unwrap_or_else(|_| anchor.to_path_buf())
        };
        Self {
            anchor: absolute.canonicalize().unwrap_or(absolute),
            event_log: harn_vm::event_log::active_event_log()
                .unwrap_or_else(|| harn_vm::event_log::install_memory_for_current_thread(64)),
            state: tokio::sync::Mutex::new(ProjectConnectorState::default()),
        }
    }
}

#[async_trait::async_trait]
impl harn_vm::ConnectorClientResolver for ProjectConnectorResolver {
    async fn resolve(
        &self,
        provider: &str,
    ) -> Result<Option<Arc<dyn harn_vm::ConnectorClient>>, harn_vm::ClientError> {
        let provider = harn_vm::ProviderId::from(provider);
        let mut state = self.state.lock().await;
        if let Some(error) = &state.terminal_error {
            return Err(error.clone());
        }
        if let Some(error) = state.provider_errors.get(&provider) {
            return Err(error.clone());
        }
        if state.root_connectors.is_none() {
            let anchor = self.anchor.clone();
            let connectors = match tokio::task::spawn_blocking(move || {
                package::try_load_root_provider_connectors(&anchor)
            })
            .await
            {
                Ok(Ok(connectors)) => connectors,
                Ok(Err(error)) => {
                    let error = harn_vm::ClientError::Other(error.to_string());
                    state.terminal_error = Some(error.clone());
                    return Err(error);
                }
                Err(error) => {
                    let error = harn_vm::ClientError::Other(format!(
                        "project connector metadata task failed: {error}"
                    ));
                    state.terminal_error = Some(error.clone());
                    return Err(error);
                }
            };
            state.root_connectors = Some(connectors);
        }

        let mut config = state
            .root_connectors
            .as_ref()
            .and_then(|connectors| {
                connectors
                    .configs
                    .iter()
                    .find(|config| config.id == provider)
            })
            .cloned();
        if config.is_none() {
            if let Some(error) = &state.package_error {
                return Err(error.clone());
            }
            if state.package_connectors.is_none() {
                let anchor = self.anchor.clone();
                let connectors = match tokio::task::spawn_blocking(move || {
                    package::try_load_provider_connectors(&anchor)
                })
                .await
                {
                    Ok(Ok(connectors)) => connectors,
                    Ok(Err(error)) => {
                        let error = harn_vm::ClientError::Other(error.to_string());
                        state.package_error = Some(error.clone());
                        return Err(error);
                    }
                    Err(error) => {
                        let error = harn_vm::ClientError::Other(format!(
                            "project package connector metadata task failed: {error}"
                        ));
                        state.package_error = Some(error.clone());
                        return Err(error);
                    }
                };
                state.package_connectors = Some(connectors);
            }
            config = state
                .package_connectors
                .as_ref()
                .and_then(|connectors| {
                    connectors
                        .configs
                        .iter()
                        .find(|config| config.id == provider)
                })
                .cloned();
        }
        let needs_project_initialization =
            config.is_some() && !state.initialized_project_providers.contains(&provider);
        if state.context.is_none() {
            let context = match connector_context_for_event_log(self.event_log.clone()).await {
                Ok(context) => context,
                Err(error) => {
                    let error = harn_vm::ClientError::Other(error);
                    state.terminal_error = Some(error.clone());
                    return Err(error);
                }
            };
            let registry = harn_vm::ConnectorRegistry::default();
            state.clients = match initialized_registry_clients(&registry, context.clone()).await {
                Ok(clients) => clients,
                Err(error) => {
                    let error = harn_vm::ClientError::Other(error);
                    state.terminal_error = Some(error.clone());
                    return Err(error);
                }
            };
            state.context = Some(context);
        }

        if needs_project_initialization {
            let config = config.as_ref().expect("project connector config");
            let mut registry = harn_vm::ConnectorRegistry::empty();
            if let Err(error) = register_provider_connector(&mut registry, config).await {
                let error = harn_vm::ClientError::Other(error);
                state
                    .provider_errors
                    .insert(provider.clone(), error.clone());
                return Err(error);
            }
            let mut initialized = match initialized_registry_clients(
                &registry,
                state.context.as_ref().expect("connector context").clone(),
            )
            .await
            {
                Ok(initialized) => initialized,
                Err(error) => {
                    let error = harn_vm::ClientError::Other(error);
                    state
                        .provider_errors
                        .insert(provider.clone(), error.clone());
                    return Err(error);
                }
            };
            if let Some(client) = initialized.remove(&provider) {
                state.clients.insert(provider.clone(), client);
            }
            state.initialized_project_providers.insert(provider.clone());
        }
        Ok(state.clients.get(&provider).cloned())
    }
}

/// Build an empty connector projection for a VM and defer the core registry,
/// project packages, manifest connector modules, and credential binding until
/// the first connector call. The resolver is shared by the VM tree, prepares
/// metadata once, and initializes each named project provider at most once.
pub(crate) fn project_connector_clients(anchor: &Path) -> harn_vm::VmConnectorClients {
    harn_vm::VmConnectorClients::new(
        BTreeMap::new(),
        Some(Arc::new(ProjectConnectorResolver::new(anchor))),
    )
}

pub(crate) async fn build_connector_registry(
    provider_connectors: &[package::ResolvedProviderConnectorConfig],
) -> Result<harn_vm::ConnectorRegistry, String> {
    let mut registry = harn_vm::ConnectorRegistry::default();
    for config in provider_connectors {
        register_provider_connector(&mut registry, config).await?;
    }
    Ok(registry)
}

async fn register_provider_connector(
    registry: &mut harn_vm::ConnectorRegistry,
    config: &package::ResolvedProviderConnectorConfig,
) -> Result<(), String> {
    match &config.connector {
        package::ResolvedProviderConnectorKind::RustBuiltin => registry
            .register_default_provider(&config.id)
            .map_err(|error| error.to_string())?,
        package::ResolvedProviderConnectorKind::Harn { .. }
        | package::ResolvedProviderConnectorKind::Invalid(_) => {
            let preparation = config.clone();
            tokio::task::spawn_blocking(move || {
                package::ensure_provider_connector_dependencies(&preparation)
            })
            .await
            .map_err(|error| format!("connector dependency preparation task failed: {error}"))?
            .map_err(|error| error.to_string())?;
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
    }
    registry.declare_secrets(config.id.clone(), declared_connector_secrets(config));
    Ok(())
}

pub(crate) fn default_harness() -> Result<harn_vm::Harness, String> {
    default_harness_for_secret_namespace(harn_vm::secrets::configured_secret_namespace())
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
    use super::{
        default_harness_for_secret_namespace, register_provider_connector,
        should_install_default_connector_clients, ProjectConnectorResolver,
    };
    use crate::package;
    use harn_vm::ConnectorClientResolver;
    use std::path::Path;
    use std::sync::Arc;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn project_connector_resolver_initializes_one_provider_once_across_tasks() {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::write(
            project.path().join("harn.toml"),
            r#"
[package]
name = "concurrent-connector-fixture"

[[providers]]
id = "concurrent_valid"
connector = { harn = "./connector.harn" }
"#,
        )
        .expect("write manifest");
        std::fs::write(
            project.path().join("connector.harn"),
            r#"
pub fn provider_id() { return "concurrent_valid" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "ConcurrentValidPayload" }
pub fn call(_harness: Harness, method, _args) { return method }
"#,
        )
        .expect("write connector");
        let entry = project.path().join("main.harn");
        std::fs::write(&entry, "pipeline main() {}\n").expect("write entry");
        let resolver = Arc::new(ProjectConnectorResolver::new(&entry));

        let tasks = (0..8)
            .map(|_| {
                let resolver = resolver.clone();
                tokio::spawn(async move {
                    let client = resolver
                        .resolve("concurrent_valid")
                        .await
                        .expect("resolve")
                        .expect("client");
                    client
                        .call("ping", serde_json::Value::Null)
                        .await
                        .expect("call")
                })
            })
            .collect::<Vec<_>>();
        for task in tasks {
            assert_eq!(
                task.await.expect("task"),
                serde_json::Value::String("ping".to_string())
            );
        }
        let state = resolver.state.lock().await;
        assert_eq!(state.initialized_project_providers.len(), 1);
        assert!(state
            .initialized_project_providers
            .contains(&harn_vm::ProviderId::from("concurrent_valid")));
    }

    #[tokio::test]
    async fn project_connector_resolver_caches_a_provider_initialization_failure() {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::write(
            project.path().join("harn.toml"),
            r#"
[package]
name = "failed-connector-fixture"

[[providers]]
id = "failed_provider"
connector = { harn = "./connector.harn" }
"#,
        )
        .expect("write manifest");
        let connector = project.path().join("connector.harn");
        std::fs::write(&connector, "fn broken(").expect("write broken connector");
        let entry = project.path().join("main.harn");
        std::fs::write(&entry, "pipeline main() {}\n").expect("write entry");
        let resolver = ProjectConnectorResolver::new(&entry);

        let first = match resolver.resolve("failed_provider").await {
            Err(error) => error,
            Ok(_) => panic!("broken provider must fail"),
        };
        std::fs::write(
            connector,
            r#"
pub fn provider_id() { return "failed_provider" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "RecoveredPayload" }
pub fn call(_harness: Harness, method, _args) { return method }
"#,
        )
        .expect("repair connector after terminal failure");
        let second = match resolver.resolve("failed_provider").await {
            Err(error) => error,
            Ok(_) => panic!("one resolver must retain its terminal provider failure"),
        };

        assert_eq!(first, second);
        let state = resolver.state.lock().await;
        assert_eq!(state.provider_errors.len(), 1);
        assert!(state.initialized_project_providers.is_empty());
    }

    #[tokio::test]
    async fn builtin_manifest_provider_keeps_its_declared_credentials() {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::write(
            project.path().join("harn.toml"),
            r#"
[package]
name = "builtin-connector-fixture"

[[providers]]
id = "webhook"
connector = { rust = "builtin" }

[providers.setup]
required_secrets = ["webhook/signing-secret"]
"#,
        )
        .expect("write manifest");
        let entry = project.path().join("main.harn");
        std::fs::write(&entry, "pipeline main() {}\n").expect("write entry");
        let connectors = package::try_load_provider_connectors(&entry)
            .expect("resolve built-in manifest provider");
        let config = connectors.configs.first().expect("provider config");
        let mut registry = harn_vm::ConnectorRegistry::empty();

        register_provider_connector(&mut registry, config)
            .await
            .expect("register configured built-in provider");

        assert!(registry.get(&config.id).is_some());
        assert_eq!(
            registry.declared_secrets_for(&config.id),
            harn_vm::declared_secret_ids(["webhook/signing-secret"])
        );
    }

    #[tokio::test]
    async fn root_provider_does_not_prepare_an_unrelated_broken_dependency() {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::write(
            project.path().join("harn.toml"),
            r#"
[package]
name = "root-provider-precedence-fixture"

[dependencies]
missing = { git = "https://example.invalid/missing.git" }

[[providers]]
id = "webhook"
connector = { rust = "builtin" }
"#,
        )
        .expect("write manifest");
        let entry = project.path().join("main.harn");
        std::fs::write(&entry, "pipeline main() {}\n").expect("write entry");
        let resolver = ProjectConnectorResolver::new(&entry);

        let package_error = match resolver.resolve("missing_package_provider").await {
            Err(error) => error,
            Ok(_) => panic!("a package lookup must still validate dependencies"),
        };
        assert!(package_error.to_string().contains("harn.lock"));

        assert!(
            resolver
                .resolve("webhook")
                .await
                .expect("root provider must bypass the failed package layer")
                .is_some(),
            "the configured built-in provider must remain available"
        );
        assert!(
            !project.path().join("harn.lock").exists(),
            "root provider resolution must not materialize dependencies"
        );
    }

    #[tokio::test]
    async fn root_harn_provider_prepares_only_its_reachable_dependency() {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::write(
            project.path().join("harn.toml"),
            r#"
[package]
name = "root-harn-provider-fixture"

[dependencies]
fixture_dep = { path = "./vendor/fixture_dep" }

[[providers]]
id = "root_dep"
connector = { harn = "./connector.harn" }
"#,
        )
        .expect("write manifest");
        let dependency = project.path().join("vendor/fixture_dep");
        std::fs::create_dir_all(&dependency).expect("create dependency");
        std::fs::write(
            dependency.join("harn.toml"),
            "[package]\nname = \"fixture_dep\"\n\n[dependencies]\ntransitive_dep = { path = \"../transitive_dep\" }\n",
        )
        .expect("write dependency manifest");
        std::fs::write(
            dependency.join("value.harn"),
            "import { transitive_value } from \"transitive_dep/value\"\n\npub fn package_value() -> int { return transitive_value() }\n",
        )
        .expect("write dependency module");
        let transitive = project.path().join("vendor/transitive_dep");
        std::fs::create_dir_all(&transitive).expect("create transitive dependency");
        std::fs::write(
            transitive.join("harn.toml"),
            "[package]\nname = \"transitive_dep\"\n",
        )
        .expect("write transitive manifest");
        std::fs::write(
            transitive.join("value.harn"),
            "pub fn transitive_value() -> int { return 42 }\n",
        )
        .expect("write transitive module");
        std::fs::write(
            project.path().join("connector.harn"),
            r#"
import { package_value } from "fixture_dep/value"

pub fn provider_id() { return "root_dep" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "RootDependencyPayload" }
pub fn call(_harness: Harness, _method, _args) { return package_value() }
"#,
        )
        .expect("write connector");
        let cache = tempfile::tempdir().expect("package cache");
        crate::package::install_packages_in(
            &crate::package::PackageWorkspace::for_test(project.path(), cache.path()),
            false,
            None,
            false,
        )
        .expect("install initial generation");
        let installed_generation =
            harn_modules::package_snapshot::PackageSnapshot::acquire(project.path())
                .expect("acquire installed generation")
                .expect("installed generation")
                .generation()
                .to_string();
        let root_config =
            crate::package::try_load_root_provider_connectors(&project.path().join("main.harn"))
                .expect("load root connector metadata")
                .configs
                .into_iter()
                .next()
                .expect("root connector config");
        crate::package::ensure_provider_connector_dependencies(&root_config)
            .expect("reuse a full generation for a reachable subset");
        let reused_generation =
            harn_modules::package_snapshot::PackageSnapshot::acquire(project.path())
                .expect("reacquire installed generation")
                .expect("reused generation")
                .generation()
                .to_string();
        assert_eq!(
            reused_generation, installed_generation,
            "selective preparation must not republish an already-sufficient generation"
        );
        std::fs::write(
            project.path().join("harn.toml"),
            r#"
[package]
name = "root-harn-provider-fixture"

[dependencies]
fixture_dep = { path = "./vendor/fixture_dep" }
unrelated = { path = "./vendor/unrelated" }

[[providers]]
id = "root_dep"
connector = { harn = "./connector.harn" }
"#,
        )
        .expect("add an unrelated dependency without changing the lock");
        std::fs::remove_dir_all(project.path().join(".harn")).expect("remove initial generation");
        let entry = project.path().join("main.harn");
        std::fs::write(&entry, "pipeline main() {}\n").expect("write entry");
        let resolver = ProjectConnectorResolver::new(&entry);

        let client = resolver
            .resolve("root_dep")
            .await
            .expect("prepare root Harn connector dependency")
            .expect("root Harn connector client");
        assert_eq!(
            client
                .call("value", serde_json::Value::Null)
                .await
                .expect("call connector"),
            serde_json::json!(42)
        );
        assert!(project.path().join(".harn/package-current.toml").is_file());
    }

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
