pub(crate) enum RunFileMcpServeMode {
    Stdio,
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

/// Compile and execute a script until it publishes its tool registry.
/// Presentation adapters call this once and then consume the same VM-owned
/// handlers; no adapter is allowed to rebuild or proxy the dispatch table.
pub(crate) async fn load_file_tool_registry(
    path: &str,
) -> Result<LoadedToolRegistry, ToolRegistryLoadError> {
    use std::path::Path;

    use crate::skill_loader::{
        emit_loader_warnings, install_skills_global, load_skills, SkillLoaderInputs,
    };

    use super::{compile_or_load_chunk_for_run, entry_source_dir, LoadedChunk};

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

    let mut vm = harn_vm::Vm::new();
    vm.set_graph_link_table(link_table);
    harn_vm::register_vm_stdlib(&mut vm);
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
    let _stale_registry = harn_vm::take_mcp_serve_registry();
    let _stale_resources = harn_vm::take_mcp_serve_resources();
    let _stale_resource_templates = harn_vm::take_mcp_serve_resource_templates();
    let _stale_prompts = harn_vm::take_mcp_serve_prompts();
    let _stale_metadata = harn_vm::take_mcp_serve_metadata();
    tokio::task::LocalSet::new()
        .run_until(async {
            vm.execute(&chunk)
                .await
                .map_err(|error| ToolRegistryLoadError {
                    message: format!(
                        "{diagnostics}{}{}",
                        vm.output(),
                        vm.format_runtime_error(&error)
                    ),
                    exit_code: error.process_exit_code().unwrap_or(1),
                })?;
            if !vm.output().is_empty() {
                diagnostics.push_str(vm.output());
            }
            let registry =
                harn_vm::take_mcp_serve_registry().ok_or_else(|| ToolRegistryLoadError {
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
        })
        .await
}

pub(super) async fn run_server(
    server: harn_vm::McpServer,
    mut vm: harn_vm::Vm,
    mode: RunFileMcpServeMode,
) {
    let result = match mode {
        RunFileMcpServeMode::Stdio => server.run(&mut vm).await.map_err(|error| error.to_string()),
        RunFileMcpServeMode::Http(http) => {
            let RunFileMcpServeHttp {
                options,
                auth_policy,
            } = *http;
            crate::commands::serve::run_script_mcp_http_server(server, vm, options, auth_policy)
                .await
        }
        RunFileMcpServeMode::App(app) => {
            let RunFileAppServe {
                bind,
                resource,
                open,
            } = *app;
            crate::commands::app::run_script_app_server(server, vm, bind, resource, open).await
        }
    };
    if let Err(error) = result {
        eprintln!("error: MCP server error: {error}");
        std::process::exit(1);
    }
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
) {
    use std::path::Path;
    use std::process;

    let loaded = match load_file_tool_registry(path).await {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("error: {}", error.message);
            process::exit(error.exit_code);
        }
    };
    if !loaded.diagnostics.is_empty() {
        eprint!("{}", loaded.diagnostics);
    }
    let LoadedToolRegistry {
        vm,
        registry,
        diagnostics: _,
        resources,
        resource_templates,
        prompts,
        mut metadata,
        _connector_clients,
    } = loaded;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let tools = match harn_vm::tool_registry_to_mcp_tools(&registry) {
                Ok(tools) => tools,
                Err(error) => {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            };
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
                .and_then(|s| s.to_str())
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
            eprintln!(
                "[harn] serve mcp: serving {} as '{server_name}'",
                capabilities.join(", ")
            );

            let mut server =
                harn_vm::McpServer::new(server_name, tools, resources, resource_templates, prompts);
            if let Some(metadata) = metadata {
                server = server.with_metadata(metadata);
            }
            if let Some(source) = card_source {
                match resolve_card_source(source) {
                    Ok(card) => server = server.with_server_card(card),
                    Err(error) => {
                        eprintln!("error: --card: {error}");
                        process::exit(1);
                    }
                }
            }
            run_server(server, vm, mode).await;
            drop(_connector_clients);
        })
        .await;
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
