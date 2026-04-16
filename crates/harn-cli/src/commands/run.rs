use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use harn_parser::DiagnosticSeverity;

use crate::commands::mcp::{self, AuthResolution};
use crate::package;
use crate::parse_source_file;

/// Core builtins that are never denied, even when using `--allow`.
const CORE_BUILTINS: &[&str] = &[
    "println",
    "print",
    "log",
    "type_of",
    "to_string",
    "to_int",
    "to_float",
    "len",
    "assert",
    "assert_eq",
    "assert_ne",
    "json_parse",
    "json_stringify",
];

/// Build the set of denied builtin names from `--deny` or `--allow` flags.
///
/// - `--deny a,b,c` denies exactly those names.
/// - `--allow a,b,c` denies everything *except* the listed names and the core builtins.
pub(crate) fn build_denied_builtins(
    deny_csv: Option<&str>,
    allow_csv: Option<&str>,
) -> HashSet<String> {
    if let Some(csv) = deny_csv {
        csv.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else if let Some(csv) = allow_csv {
        // With --allow, we mark every registered stdlib builtin as denied
        // *except* those in the allow list and the core builtins.
        let allowed: HashSet<String> = csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let core: HashSet<&str> = CORE_BUILTINS.iter().copied().collect();

        // Create a temporary VM with stdlib registered to enumerate all builtin names.
        let mut tmp = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut tmp);
        harn_vm::register_store_builtins(&mut tmp, std::path::Path::new("."));
        harn_vm::register_metadata_builtins(&mut tmp, std::path::Path::new("."));

        tmp.builtin_names()
            .into_iter()
            .filter(|name| !allowed.contains(name) && !core.contains(name.as_str()))
            .collect()
    } else {
        HashSet::new()
    }
}

/// Run the static type checker against `program` with cross-module
/// import-aware call resolution when the file's imports all resolve. Used
/// by `run_file` and the MCP server entry so `harn run` catches undefined
/// cross-module calls before the VM starts.
fn typecheck_with_imports(
    program: &[harn_parser::SNode],
    path: &Path,
) -> Vec<harn_parser::TypeDiagnostic> {
    let graph = harn_modules::build(&[path.to_path_buf()]);
    let mut checker = harn_parser::TypeChecker::new();
    if let Some(imported) = graph.imported_names_for_file(path) {
        checker = checker.with_imported_names(imported);
    }
    checker.check(program)
}

pub(crate) async fn run_file(
    path: &str,
    trace: bool,
    denied_builtins: HashSet<String>,
    script_argv: Vec<String>,
) {
    let (source, program) = parse_source_file(path);

    let mut had_type_error = false;
    let type_diagnostics = typecheck_with_imports(&program, Path::new(path));
    for diag in &type_diagnostics {
        match diag.severity {
            DiagnosticSeverity::Error => {
                had_type_error = true;
                if let Some(span) = &diag.span {
                    let rendered = harn_parser::diagnostic::render_diagnostic(
                        &source,
                        path,
                        span,
                        "error",
                        &diag.message,
                        None,
                        diag.help.as_deref(),
                    );
                    eprint!("{rendered}");
                } else {
                    eprintln!("error: {}", diag.message);
                }
            }
            DiagnosticSeverity::Warning => {
                if let Some(span) = &diag.span {
                    let rendered = harn_parser::diagnostic::render_diagnostic(
                        &source,
                        path,
                        span,
                        "warning",
                        &diag.message,
                        None,
                        diag.help.as_deref(),
                    );
                    eprint!("{rendered}");
                } else {
                    eprintln!("warning: {}", diag.message);
                }
            }
        }
    }
    if had_type_error {
        process::exit(1);
    }

    let chunk = match harn_vm::Compiler::new().compile(&program) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: compile error: {e}");
            process::exit(1);
        }
    };

    if trace {
        harn_vm::llm::enable_tracing();
    }

    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    let source_parent = std::path::Path::new(path)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    // Metadata/store rooted at harn.toml when present; source dir otherwise.
    let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
    let store_base = project_root.as_deref().unwrap_or(source_parent);
    harn_vm::register_store_builtins(&mut vm, store_base);
    harn_vm::register_metadata_builtins(&mut vm, store_base);
    let pipeline_name = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
    vm.set_source_info(path, &source);
    if !denied_builtins.is_empty() {
        vm.set_denied_builtins(denied_builtins);
    }
    if let Some(ref root) = project_root {
        vm.set_project_root(root);
    }

    if let Some(p) = std::path::Path::new(path).parent() {
        if !p.as_os_str().is_empty() {
            vm.set_source_dir(p);
        }
    }

    // `harn run script.harn -- a b c` yields `argv == ["a", "b", "c"]`.
    // Always set so scripts can rely on `len(argv)`.
    let argv_values: Vec<harn_vm::VmValue> = script_argv
        .iter()
        .map(|s| harn_vm::VmValue::String(std::rc::Rc::from(s.as_str())))
        .collect();
    vm.set_global(
        "argv",
        harn_vm::VmValue::List(std::rc::Rc::new(argv_values)),
    );

    if let Some(manifest) = package::try_read_manifest_for(Path::new(path)) {
        if !manifest.mcp.is_empty() {
            connect_mcp_servers(&manifest.mcp, &mut vm).await;
        }
    }

    // Graceful shutdown: flush run records before exit on SIGINT/SIGTERM.
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
            let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");
            tokio::select! {
                _ = sigterm.recv() => {},
                _ = sigint.recv() => {},
            }
            cancelled_clone.store(true, Ordering::SeqCst);
            eprintln!("[harn] signal received, flushing state...");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            process::exit(124);
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            cancelled_clone.store(true, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            process::exit(124);
        }
    });

    // Run inside a LocalSet so spawn_local works for concurrency builtins.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            match vm.execute(&chunk).await {
                Ok(_) => {
                    let output = vm.output();
                    if !output.is_empty() {
                        io::stdout().write_all(output.as_bytes()).ok();
                    }
                }
                Err(e) => {
                    eprint!("{}", vm.format_runtime_error(&e));
                    if cancelled.load(Ordering::SeqCst) {
                        process::exit(124);
                    }
                    process::exit(1);
                }
            }
        })
        .await;

    if trace {
        print_trace_summary();
    }
}

/// Connect to MCP servers declared in `harn.toml` and register them as
/// `mcp.<name>` globals on the VM. Connection failures are warned but do
/// not abort execution.
pub(crate) async fn connect_mcp_servers(
    servers: &[package::McpServerConfig],
    vm: &mut harn_vm::Vm,
) {
    use std::collections::BTreeMap;
    use std::rc::Rc;

    let mut mcp_dict: BTreeMap<String, harn_vm::VmValue> = BTreeMap::new();

    for server in servers {
        let resolved_auth = match mcp::resolve_auth_for_server(server).await {
            Ok(resolution) => resolution,
            Err(error) => {
                eprintln!(
                    "warning: mcp: failed to load auth for '{}': {}",
                    server.name, error
                );
                AuthResolution::None
            }
        };
        let spec = serde_json::json!({
            "name": server.name,
            "transport": server.transport.clone().unwrap_or_else(|| "stdio".to_string()),
            "command": server.command,
            "args": server.args,
            "env": server.env,
            "url": server.url,
            "auth_token": match resolved_auth {
                AuthResolution::Bearer(token) => Some(token),
                AuthResolution::None => server.auth_token.clone(),
            },
            "protocol_version": server.protocol_version,
            "proxy_server_name": server.proxy_server_name,
        });
        match harn_vm::connect_mcp_server_from_json(&spec).await {
            Ok(handle) => {
                eprintln!("[harn] mcp: connected to '{}'", server.name);
                mcp_dict.insert(server.name.clone(), harn_vm::VmValue::McpClient(handle));
            }
            Err(e) => {
                eprintln!(
                    "warning: mcp: failed to connect to '{}': {}",
                    server.name, e
                );
            }
        }
    }

    if !mcp_dict.is_empty() {
        vm.set_global("mcp", harn_vm::VmValue::Dict(Rc::new(mcp_dict)));
    }
}

fn print_trace_summary() {
    let entries = harn_vm::llm::take_trace();
    if entries.is_empty() {
        return;
    }
    eprintln!("\n\x1b[2m─── LLM trace ───\x1b[0m");
    let mut total_input = 0i64;
    let mut total_output = 0i64;
    let mut total_ms = 0u64;
    for (i, entry) in entries.iter().enumerate() {
        eprintln!(
            "  #{}: {} | {} in + {} out tokens | {} ms",
            i + 1,
            entry.model,
            entry.input_tokens,
            entry.output_tokens,
            entry.duration_ms,
        );
        total_input += entry.input_tokens;
        total_output += entry.output_tokens;
        total_ms += entry.duration_ms;
    }
    let total_tokens = total_input + total_output;
    // Rough cost estimate using Sonnet 4 pricing ($3/MTok in, $15/MTok out).
    let cost = (total_input as f64 * 3.0 + total_output as f64 * 15.0) / 1_000_000.0;
    eprintln!(
        "  \x1b[1m{} call{}, {} tokens ({}in + {}out), {} ms, ~${:.4}\x1b[0m",
        entries.len(),
        if entries.len() == 1 { "" } else { "s" },
        total_tokens,
        total_input,
        total_output,
        total_ms,
        cost,
    );
}

/// Run a .harn file as an MCP server over stdio. The pipeline must call
/// `mcp_serve(registry)` so the CLI can expose its tools.
pub(crate) async fn run_file_mcp_serve(path: &str) {
    let (source, program) = crate::parse_source_file(path);

    let type_diagnostics = typecheck_with_imports(&program, Path::new(path));
    for diag in &type_diagnostics {
        match diag.severity {
            DiagnosticSeverity::Error => {
                eprintln!("error: {}", diag.message);
                process::exit(1);
            }
            DiagnosticSeverity::Warning => {
                if let Some(span) = &diag.span {
                    eprintln!("warning: {} (line {})", diag.message, span.line);
                } else {
                    eprintln!("warning: {}", diag.message);
                }
            }
        }
    }

    let chunk = match harn_vm::Compiler::new().compile(&program) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: compile error: {e}");
            process::exit(1);
        }
    };

    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    let source_parent = std::path::Path::new(path)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
    let store_base = project_root.as_deref().unwrap_or(source_parent);
    harn_vm::register_store_builtins(&mut vm, store_base);
    harn_vm::register_metadata_builtins(&mut vm, store_base);
    let pipeline_name = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
    vm.set_source_info(path, &source);
    if let Some(ref root) = project_root {
        vm.set_project_root(root);
    }
    if let Some(p) = std::path::Path::new(path).parent() {
        if !p.as_os_str().is_empty() {
            vm.set_source_dir(p);
        }
    }

    if let Some(manifest) = package::try_read_manifest_for(Path::new(path)) {
        if !manifest.mcp.is_empty() {
            connect_mcp_servers(&manifest.mcp, &mut vm).await;
        }
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            match vm.execute(&chunk).await {
                Ok(_) => {}
                Err(e) => {
                    eprint!("{}", vm.format_runtime_error(&e));
                    process::exit(1);
                }
            }

            // Pipeline output goes to stderr — stdout is the MCP transport.
            let output = vm.output();
            if !output.is_empty() {
                eprint!("{output}");
            }

            let registry = match harn_vm::take_mcp_serve_registry() {
                Some(r) => r,
                None => {
                    eprintln!("error: pipeline did not call mcp_serve(registry)");
                    eprintln!("hint: call mcp_serve(tools) at the end of your pipeline");
                    process::exit(1);
                }
            };

            let tools = match harn_vm::tool_registry_to_mcp_tools(&registry) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: {e}");
                    process::exit(1);
                }
            };

            let resources = harn_vm::take_mcp_serve_resources();
            let resource_templates = harn_vm::take_mcp_serve_resource_templates();
            let prompts = harn_vm::take_mcp_serve_prompts();

            let server_name = std::path::Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("harn")
                .to_string();

            let mut caps = Vec::new();
            if !tools.is_empty() {
                caps.push(format!(
                    "{} tool{}",
                    tools.len(),
                    if tools.len() == 1 { "" } else { "s" }
                ));
            }
            let total_resources = resources.len() + resource_templates.len();
            if total_resources > 0 {
                caps.push(format!(
                    "{total_resources} resource{}",
                    if total_resources == 1 { "" } else { "s" }
                ));
            }
            if !prompts.is_empty() {
                caps.push(format!(
                    "{} prompt{}",
                    prompts.len(),
                    if prompts.len() == 1 { "" } else { "s" }
                ));
            }
            eprintln!(
                "[harn] mcp-serve: serving {} as '{server_name}'",
                caps.join(", ")
            );

            let server =
                harn_vm::McpServer::new(server_name, tools, resources, resource_templates, prompts);
            if let Err(e) = server.run(&mut vm).await {
                eprintln!("error: MCP server error: {e}");
                process::exit(1);
            }
        })
        .await;
}

pub(crate) async fn run_watch(path: &str, denied_builtins: HashSet<String>) {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    let abs_path = std::fs::canonicalize(path).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        process::exit(1);
    });
    let watch_dir = abs_path.parent().unwrap_or(Path::new("."));

    eprintln!("\x1b[2m[watch] running {path}...\x1b[0m");
    run_file(path, false, denied_builtins.clone(), Vec::new()).await;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let _watcher = {
        let tx = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    let has_harn = event
                        .paths
                        .iter()
                        .any(|p| p.extension().is_some_and(|ext| ext == "harn"));
                    if has_harn {
                        let _ = tx.blocking_send(());
                    }
                }
            }
        })
        .unwrap_or_else(|e| {
            eprintln!("Error setting up file watcher: {e}");
            process::exit(1);
        });
        watcher
            .watch(watch_dir, RecursiveMode::Recursive)
            .unwrap_or_else(|e| {
                eprintln!("Error watching directory: {e}");
                process::exit(1);
            });
        watcher // keep alive
    };

    eprintln!(
        "\x1b[2m[watch] watching {} for .harn changes (ctrl-c to stop)\x1b[0m",
        watch_dir.display()
    );

    loop {
        rx.recv().await;
        // Debounce: let bursts of events settle for 200ms before re-running.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}

        eprintln!();
        eprintln!("\x1b[2m[watch] change detected, re-running {path}...\x1b[0m");
        run_file(path, false, denied_builtins.clone(), Vec::new()).await;
    }
}
