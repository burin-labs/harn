pub(crate) enum RunFileMcpServeMode {
    Stdio { watch: bool },
    Http(Box<RunFileMcpServeHttp>),
    App(Box<RunFileAppServe>),
}

pub(crate) struct RunFileMcpServeHttp {
    pub options: harn_serve::McpHttpServeOptions,
    pub auth_policy: harn_serve::AuthPolicy,
}

pub(crate) struct RunFileAppServe {
    pub bind: std::net::SocketAddr,
    pub resource: Option<String>,
    pub open: bool,
}

/// Executable registry loaded from the same script path used by MCP serving.
/// The connector guard must live as long as the VM because handlers may defer
/// connector initialization until their first dispatch.
pub(crate) struct LoadedToolRegistry {
    pub(crate) vm: harn_vm::Vm,
    pub(crate) registry: harn_vm::VmValue,
    pub(crate) diagnostics: String,
    pub(crate) resources: Vec<harn_vm::McpResourceDef>,
    pub(crate) resource_templates: Vec<harn_vm::McpResourceTemplateDef>,
    pub(crate) prompts: Vec<harn_vm::McpPromptDef>,
    pub(crate) metadata: Option<harn_vm::McpServerMetadata>,
    _connector_clients: harn_vm::ActiveConnectorClientsGuard,
}

#[derive(Debug)]
pub(crate) struct ToolRegistryLoadError {
    pub(crate) message: String,
    pub(crate) exit_code: i32,
}

// Registry publication still enters through VM thread-local slots. Serialize
// script initialization until the VM can return a publication bundle directly;
// otherwise two in-process adapters scheduled on one runtime thread could
// clear or consume each other's registry between await points.
static TOOL_REGISTRY_LOAD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Compile and execute a script until it publishes its tool registry.
/// Presentation adapters call this once and then consume the same VM-owned
/// handlers; no adapter is allowed to rebuild or proxy the dispatch table.
pub(crate) async fn load_file_tool_registry(
    path: &str,
) -> Result<LoadedToolRegistry, ToolRegistryLoadError> {
    tokio::task::LocalSet::new()
        .run_until(load_file_tool_registry_local(path))
        .await
}

/// Local-task variant used by a live stdio server when a watched source tree
/// changes. Keeping compilation and execution in this one loader preserves the
/// exact same validation and connector setup as initial startup and CLI calls.
pub(crate) async fn load_file_tool_registry_local(
    path: &str,
) -> Result<LoadedToolRegistry, ToolRegistryLoadError> {
    use std::path::Path;

    use crate::skill_loader::{
        emit_loader_warnings, install_skills_global, load_skills, SkillLoaderInputs,
    };

    use super::{compile_or_load_chunk_for_run, entry_source_dir, LoadedChunk};

    let _load_guard = TOOL_REGISTRY_LOAD.lock().await;

    let mut diagnostics = String::new();
    let LoadedChunk {
        source,
        chunk,
        link_table,
    } = compile_or_load_chunk_for_run(path, &mut diagnostics).map_err(|_| {
        ToolRegistryLoadError {
            message: diagnostics.clone(),
            exit_code: 1,
        }
    })?;

    let mut vm = harn_vm::new_vm();
    vm.set_graph_link_table(link_table);
    harn_runtime::register_vm_stdlib(&mut vm);
    crate::install_default_hostlib(&mut vm);
    let source_parent = Path::new(path).parent().unwrap_or(Path::new("."));
    let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
    let store_base = project_root.as_deref().unwrap_or(source_parent);
    harn_vm::register_store_builtins(&mut vm, store_base);
    harn_vm::register_metadata_builtins(&mut vm, store_base);
    let pipeline_name = Path::new(path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("default");
    harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
    vm.set_source_info(path, &source);
    if let Some(root) = project_root.as_ref() {
        vm.set_project_root(root);
    }
    vm.set_source_dir(&entry_source_dir(path));

    let skills = load_skills(&SkillLoaderInputs {
        cli_dirs: Vec::new(),
        source_path: Some(path.into()),
    });
    emit_loader_warnings(&skills.loader_warnings);
    install_skills_global(&mut vm, &skills);

    let connector_clients = super::manifest_runtime::install_manifest_runtime(
        Path::new(path),
        &mut vm,
        crate::package::ManifestHandlerInitialization::OnDispatch,
        true,
    )
    .await
    .map_err(|error| ToolRegistryLoadError {
        message: format!("failed to install {}: {error}", error.label()),
        exit_code: 1,
    })?;

    vm.set_source_dir(&entry_source_dir(path));
    // A missing publication must never read as success because another
    // in-process adapter left a registry in thread-local state.
    harn_vm::reset_mcp_serve_publication();
    vm.execute(&chunk)
        .await
        .map_err(|error| ToolRegistryLoadError {
            // A top-level `throw` is still caller-authored data. Until the
            // registry exists there is no error schema that could authorize
            // exposing its value, so startup follows the same value-free
            // runtime summary as invocation adapters.
            message: format!(
                "{diagnostics}{}{}",
                vm.output(),
                if matches!(
                    &error,
                    harn_vm::VmError::Thrown(_) | harn_vm::VmError::DeclaredThrown(_)
                ) || matches!(
                    harn_vm::error_to_category(&error),
                    harn_vm::ErrorCategory::Auth
                        | harn_vm::ErrorCategory::BudgetExceeded
                        | harn_vm::ErrorCategory::Cancelled
                        | harn_vm::ErrorCategory::RateLimit
                ) {
                    format!(
                        "Runtime error: {}\n",
                        harn_vm::tool_registry::tool_runtime_error_summary(&error)
                    )
                } else {
                    vm.format_runtime_error(&error)
                }
            ),
            exit_code: error.process_exit_code().unwrap_or(1),
        })?;
    if !vm.output().is_empty() {
        diagnostics.push_str(vm.output());
    }
    if let Some(rejection) = harn_vm::take_mcp_serve_rejection() {
        return Err(ToolRegistryLoadError {
            message: format!("{diagnostics}invalid MCP publication: {rejection}\n"),
            exit_code: 1,
        });
    }
    let registry = harn_vm::take_mcp_serve_registry().ok_or_else(|| ToolRegistryLoadError {
        message: format!(
            "{diagnostics}pipeline did not publish a tool registry\n\
                         hint: call harness.tools.mcp_tools(tools) from main"
        ),
        exit_code: 1,
    })?;
    harn_vm::tool_registry::tool_registry_catalog(&registry).map_err(|error| {
        ToolRegistryLoadError {
            message: format!("invalid tool registry: {error}"),
            exit_code: 1,
        }
    })?;
    Ok(LoadedToolRegistry {
        vm,
        registry,
        diagnostics,
        resources: harn_vm::take_mcp_serve_resources(),
        resource_templates: harn_vm::take_mcp_serve_resource_templates(),
        prompts: harn_vm::take_mcp_serve_prompts(),
        metadata: harn_vm::take_mcp_serve_metadata(),
        _connector_clients: connector_clients,
    })
}

/// Serve a loaded script registry through the one MCP server. Must run inside
/// the `LocalSet` that owns the registry's VM.
async fn run_server(loaded: LoadedMcpRuntime, mode: RunFileMcpServeMode) {
    let LoadedMcpRuntime {
        registry,
        card,
        vm,
        connector_clients,
        ..
    } = loaded;
    let runtime = harn_serve::McpScriptRuntime::spawn_local(registry, vm, connector_clients);
    let result = match mode {
        RunFileMcpServeMode::Stdio { watch: false } => {
            script_server(runtime, card, harn_serve::AuthPolicy::default())
                .run_stdio()
                .await
        }
        RunFileMcpServeMode::Stdio { watch: true } => {
            unreachable!("watch mode is owned by run_file_mcp_serve")
        }
        RunFileMcpServeMode::Http(http) => {
            let RunFileMcpServeHttp {
                options,
                auth_policy,
            } = *http;
            script_server(runtime, card, auth_policy)
                .run_http(options)
                .await
        }
        RunFileMcpServeMode::App(app) => {
            let RunFileAppServe {
                bind,
                resource,
                open,
            } = *app;
            let server = script_server(runtime, card, harn_serve::AuthPolicy::default());
            crate::commands::app::run_script_app_server(server, bind, resource, open).await
        }
    };
    if let Err(error) = result {
        crate::fatal_error(format!("MCP server error: {error}"));
    }
}

fn script_server(
    runtime: harn_serve::McpScriptRuntime,
    card: Option<serde_json::Value>,
    auth_policy: harn_serve::AuthPolicy,
) -> std::sync::Arc<harn_serve::McpServer> {
    let mut config = harn_serve::McpServerConfig::script(runtime).with_auth_policy(auth_policy);
    if let Some(card) = card {
        config = config.with_server_card(card);
    }
    std::sync::Arc::new(harn_serve::McpServer::new(config))
}

/// Run a .harn file as an MCP server using the script-driven surface.
///
/// The pipeline must publish a registry with `mcp_tools(...)` (or the legacy
/// `mcp_serve(...)` alias). Resources, templates, prompts, and optional server
/// card metadata are collected from the same execution before transport starts.
pub(crate) async fn run_file_mcp_serve(
    path: &str,
    card_source: Option<&str>,
    mode: RunFileMcpServeMode,
    host_boundary: Option<harn_vm::llm::api::InferenceBoundary>,
) {
    let _environment = super::environment::host_environment_scope(host_boundary);
    let watch = matches!(&mode, RunFileMcpServeMode::Stdio { watch: true });
    let loaded = match load_mcp_runtime(path, card_source).await {
        Ok(runtime) => runtime,
        Err(error) => {
            crate::fatal_error_with_code(error.message, error.exit_code);
        }
    };
    if !loaded.diagnostics.is_empty() {
        eprint!("{}", loaded.diagnostics);
    }
    harn_vm::events::log_info(
        "mcp",
        &format!(
            "serve mcp: serving {} as '{}'{}",
            loaded.capability_summary,
            loaded.server_name,
            if watch { " with source reload" } else { "" },
        ),
    );

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            if watch {
                run_reloadable_stdio(path, loaded).await;
            } else {
                run_server(loaded, mode).await;
            }
        })
        .await;
}

struct LoadedMcpRuntime {
    registry: harn_vm::McpScriptRegistry,
    card: Option<serde_json::Value>,
    vm: harn_vm::Vm,
    diagnostics: String,
    server_name: String,
    capability_summary: String,
    connector_clients: harn_vm::ActiveConnectorClientsGuard,
}

async fn load_mcp_runtime(
    path: &str,
    card_source: Option<&str>,
) -> Result<LoadedMcpRuntime, ToolRegistryLoadError> {
    let loaded = load_file_tool_registry(path).await?;
    project_mcp_runtime(path, card_source, loaded)
}

/// Reload a watched source. The server card is server configuration fixed at
/// startup, so a reload never resolves one.
async fn load_mcp_runtime_local(path: &str) -> Result<LoadedMcpRuntime, ToolRegistryLoadError> {
    let loaded = load_file_tool_registry_local(path).await?;
    project_mcp_runtime(path, None, loaded)
}

fn project_mcp_runtime(
    path: &str,
    card_source: Option<&str>,
    loaded: LoadedToolRegistry,
) -> Result<LoadedMcpRuntime, ToolRegistryLoadError> {
    use std::path::Path;

    let LoadedToolRegistry {
        vm,
        registry,
        diagnostics,
        resources,
        resource_templates,
        prompts,
        mut metadata,
        _connector_clients: connector_clients,
    } = loaded;
    let tools =
        harn_vm::tool_registry_to_mcp_tools(&registry).map_err(|error| ToolRegistryLoadError {
            message: error.to_string(),
            exit_code: 1,
        })?;
    let catalog = harn_vm::tool_registry::tool_registry_catalog(&registry)
        .expect("registry was validated by the shared loader");
    if let Some(info) = catalog.info {
        let metadata = metadata.get_or_insert_default();
        if metadata.name.is_none() {
            metadata.name = Some(info.name);
        }
        if metadata.version.is_none() {
            metadata.version = info.version;
        }
        if metadata.instructions.is_none() {
            metadata.instructions = info.description;
        }
    }

    let mut server_name = Path::new(path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("harn")
        .to_string();
    if let Some(name) = metadata
        .as_ref()
        .and_then(|metadata| metadata.name.as_ref())
    {
        server_name = name.clone();
    }

    let mut capabilities = Vec::new();
    if !tools.is_empty() {
        capabilities.push(format!(
            "{} tool{}",
            tools.len(),
            if tools.len() == 1 { "" } else { "s" }
        ));
    }
    let total_resources = resources.len() + resource_templates.len();
    if total_resources > 0 {
        capabilities.push(format!(
            "{total_resources} resource{}",
            if total_resources == 1 { "" } else { "s" }
        ));
    }
    if !prompts.is_empty() {
        capabilities.push(format!(
            "{} prompt{}",
            prompts.len(),
            if prompts.len() == 1 { "" } else { "s" }
        ));
    }

    let mut registry = harn_vm::McpScriptRegistry::new(
        server_name.clone(),
        tools,
        resources,
        resource_templates,
        prompts,
    );
    if let Some(metadata) = metadata {
        registry = registry.with_metadata(metadata);
    }
    let card = card_source
        .map(|source| {
            resolve_card_source(source).map_err(|error| ToolRegistryLoadError {
                message: format!("--card: {error}"),
                exit_code: 1,
            })
        })
        .transpose()?;

    Ok(LoadedMcpRuntime {
        registry,
        card,
        vm,
        diagnostics,
        server_name,
        capability_summary: capabilities.join(", "),
        connector_clients,
    })
}

async fn run_reloadable_stdio(path: &str, loaded: LoadedMcpRuntime) {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::path::Path;
    use std::time::Duration;

    // The channel is a dirty bit, not an event log. A bounded slot prevents a
    // rapid editor-save burst from accumulating while a replacement compiles.
    let (source_tx, mut source_rx) = tokio::sync::mpsc::channel(1);
    let mut watcher =
        match notify::recommended_watcher(move |result: notify::Result<notify::Event>| match result
        {
            Ok(event)
                if !matches!(event.kind, EventKind::Access(_))
                    && event.paths.iter().any(|path| {
                        path.extension().and_then(|extension| extension.to_str()) == Some("harn")
                            || path.file_name().and_then(|name| name.to_str()) == Some("harn.toml")
                    }) =>
            {
                let _ = source_tx.try_send(());
            }
            Ok(_) => {}
            Err(error) => {
                harn_vm::events::log_warn("mcp", &format!("source watcher error: {error}"));
            }
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                crate::fatal_error(format!("failed to create MCP source watcher: {error}"));
            }
        };
    // A bare filename has an empty parent, which notify cannot watch.
    let watch_root = Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if let Err(error) = watcher.watch(watch_root, RecursiveMode::Recursive) {
        crate::fatal_error(format!(
            "failed to watch MCP source root {}: {error}",
            watch_root.display()
        ));
    }

    let LoadedMcpRuntime {
        registry,
        card,
        vm,
        connector_clients,
        ..
    } = loaded;
    let (runtime, reloader) =
        harn_serve::McpScriptRuntime::spawn_local_reloadable(registry, vm, connector_clients);
    let reload_path = path.to_string();
    tokio::task::spawn_local(async move {
        while source_rx.recv().await.is_some() {
            // One editor save can produce create, rename, and modify events.
            // Wait for that burst, then load the final source state once.
            tokio::time::sleep(Duration::from_millis(75)).await;
            while source_rx.try_recv().is_ok() {}
            match load_mcp_runtime_local(&reload_path).await {
                Ok(runtime) => {
                    if !runtime.diagnostics.is_empty() {
                        eprint!("{}", runtime.diagnostics);
                    }
                    harn_vm::events::log_info(
                        "mcp",
                        &format!(
                            "serve mcp: reloaded {} as '{}'",
                            runtime.capability_summary, runtime.server_name
                        ),
                    );
                    if !reloader.replace(runtime.registry, runtime.vm, runtime.connector_clients) {
                        break;
                    }
                }
                Err(error) => harn_vm::events::log_warn(
                    "mcp",
                    &format!(
                        "serve mcp: reload failed; keeping previous registry: {}",
                        error.message
                    ),
                ),
            }
        }
    });

    let result = script_server(runtime, card, harn_serve::AuthPolicy::default())
        .run_stdio()
        .await;
    drop(watcher);
    if let Err(error) = result {
        crate::fatal_error(format!("MCP server error: {error}"));
    }
}

/// Parse `--card` as inline JSON when it starts with an object or array;
/// otherwise load it from a filesystem path.
pub(crate) fn resolve_card_source(source: &str) -> Result<serde_json::Value, String> {
    let trimmed = source.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str(source)
            .map_err(|error| format!("inline JSON parse error: {error}"));
    }
    harn_vm::load_server_card_from_path(std::path::Path::new(source))
        .map_err(|error| format!("{error}"))
}
