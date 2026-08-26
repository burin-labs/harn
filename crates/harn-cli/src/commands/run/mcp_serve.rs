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

    use crate::skill_loader::{
        emit_loader_warnings, install_skills_global, load_skills, SkillLoaderInputs,
    };

    use super::{compile_or_load_chunk_for_run, entry_source_dir, LoadedChunk};

    let mut diagnostics = String::new();
    let LoadedChunk {
        source,
        chunk,
        link_table,
    } = match compile_or_load_chunk_for_run(path, &mut diagnostics) {
        Ok(loaded) => loaded,
        Err(failure) => {
            eprint!("{diagnostics}");
            process::exit(failure.classification().exit_code());
        }
    };
    if !diagnostics.is_empty() {
        eprint!("{diagnostics}");
    }

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
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
    vm.set_source_info(path, &source);
    if let Some(ref root) = project_root {
        vm.set_project_root(root);
    }
    vm.set_source_dir(&entry_source_dir(path));

    let loaded = load_skills(&SkillLoaderInputs {
        cli_dirs: Vec::new(),
        source_path: Some(std::path::PathBuf::from(path)),
    });
    emit_loader_warnings(&loaded.loader_warnings);
    install_skills_global(&mut vm, &loaded);

    let _manifest_runtime = match super::manifest_runtime::install_manifest_runtime(
        Path::new(path),
        &mut vm,
        crate::package::ManifestHandlerInitialization::OnDispatch,
        true,
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: failed to install {}: {error}", error.label());
            process::exit(1);
        }
    };

    vm.set_source_dir(&entry_source_dir(path));
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            match vm.execute(&chunk).await {
                Ok(_) => {}
                Err(error) => crate::commands::serve::exit_after_mcp_pipeline_error(&vm, &error),
            }

            let output = vm.output();
            if !output.is_empty() {
                eprint!("{output}");
            }

            let registry = match harn_vm::take_mcp_serve_registry() {
                Some(registry) => registry,
                None => {
                    eprintln!("error: pipeline did not call mcp_serve(registry)");
                    eprintln!("hint: call mcp_serve(tools) at the end of your pipeline");
                    process::exit(1);
                }
            };
            let tools = match harn_vm::tool_registry_to_mcp_tools(&registry) {
                Ok(tools) => tools,
                Err(error) => {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            };
            let resources = harn_vm::take_mcp_serve_resources();
            let resource_templates = harn_vm::take_mcp_serve_resource_templates();
            let prompts = harn_vm::take_mcp_serve_prompts();
            let metadata = harn_vm::take_mcp_serve_metadata();

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
