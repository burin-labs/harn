use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;
use std::process;

use harn_parser::DiagnosticSeverity;

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
        harn_vm::register_http_builtins(&mut tmp);
        harn_vm::register_llm_builtins(&mut tmp);
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

pub(crate) async fn run_file(path: &str, trace: bool, denied_builtins: HashSet<String>) {
    let (source, program) = parse_source_file(path);

    // Static type checking
    let type_diagnostics = harn_parser::TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        if diag.severity == DiagnosticSeverity::Error {
            eprintln!("error: {}", diag.message);
            process::exit(1);
        }
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
    harn_vm::register_http_builtins(&mut vm);
    harn_vm::register_llm_builtins(&mut vm);
    let store_base = std::path::Path::new(path)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    harn_vm::register_store_builtins(&mut vm, store_base);
    harn_vm::register_metadata_builtins(&mut vm, store_base);
    vm.set_source_info(path, &source);
    if !denied_builtins.is_empty() {
        vm.set_denied_builtins(denied_builtins);
    }

    if let Some(p) = std::path::Path::new(path).parent() {
        if !p.as_os_str().is_empty() {
            vm.set_source_dir(p);
        }
    }

    // Auto-connect MCP servers declared in harn.toml
    if let Some(manifest) = package::try_read_manifest_for(Path::new(path)) {
        if !manifest.mcp.is_empty() {
            connect_mcp_servers(&manifest.mcp, &mut vm).await;
        }
    }

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
async fn connect_mcp_servers(servers: &[package::McpServerConfig], vm: &mut harn_vm::Vm) {
    use std::collections::BTreeMap;
    use std::rc::Rc;

    let mut mcp_dict: BTreeMap<String, harn_vm::VmValue> = BTreeMap::new();

    for server in servers {
        match harn_vm::connect_mcp_server(&server.name, &server.command, &server.args).await {
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
    // Rough cost estimate (Sonnet 4 pricing: $3/MTok input, $15/MTok output)
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

pub(crate) async fn run_file_bridge(path: &str, arg_json: Option<&str>) {
    let (_source, program) = parse_source_file(path);

    // In bridge mode, compile a specific pipeline or default
    let chunk = match harn_vm::Compiler::new().compile(&program) {
        Ok(c) => c,
        Err(e) => {
            // Send error as JSON-RPC notification so the host can detect it
            let msg = format!("compile error: {e}");
            let notification = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "error",
                "params": {"message": msg},
            });
            println!(
                "{}",
                serde_json::to_string(&notification).unwrap_or_default()
            );
            process::exit(1);
        }
    };

    let local = tokio::task::LocalSet::new();
    let path_owned = path.to_string();
    let source_owned = _source;
    let arg_owned = arg_json.map(|s| s.to_string());

    let exit_code = local
        .run_until(async move {
            let bridge = std::rc::Rc::new(harn_vm::bridge::HostBridge::new());

            let mut vm = harn_vm::Vm::new();

            // Register language builtins (string ops, math, json, etc.)
            harn_vm::register_vm_stdlib(&mut vm);

            // Register store builtins (before bridge so host can override)
            let store_base = std::path::Path::new(&path_owned)
                .parent()
                .unwrap_or(std::path::Path::new("."));
            harn_vm::register_store_builtins(&mut vm, store_base);
            harn_vm::register_metadata_builtins(&mut vm, store_base);

            // Override with bridge builtins (llm_call, file I/O, etc.)
            harn_vm::bridge_builtins::register_bridge_builtins(&mut vm, bridge.clone());

            // Set bridge for delegating unknown builtins to the host
            vm.set_bridge(bridge.clone());

            vm.set_source_info(&path_owned, &source_owned);
            if let Some(p) = std::path::Path::new(&path_owned).parent() {
                if !p.as_os_str().is_empty() {
                    vm.set_source_dir(p);
                }
            }

            // If --arg was provided, inject it as the pipeline parameter
            if let Some(arg_str) = &arg_owned {
                match serde_json::from_str::<serde_json::Value>(arg_str) {
                    Ok(val) => {
                        let vm_val = harn_vm::bridge::json_result_to_vm_value(&val);
                        vm.set_global("task", vm_val);
                    }
                    Err(e) => {
                        bridge.send_output(&format!("error: invalid --arg JSON: {e}\n"));
                        return 1;
                    }
                }
            }

            match vm.execute(&chunk).await {
                Ok(_) => {
                    // Send any buffered output
                    let output = vm.output();
                    if !output.is_empty() {
                        bridge.send_output(output);
                    }
                    0
                }
                Err(e) => {
                    let formatted = vm.format_runtime_error(&e);
                    bridge.notify("error", serde_json::json!({"message": formatted}));
                    1
                }
            }
        })
        .await;

    process::exit(exit_code);
}

pub(crate) async fn run_watch(path: &str) {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    let abs_path = std::fs::canonicalize(path).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        process::exit(1);
    });
    let watch_dir = abs_path.parent().unwrap_or(Path::new("."));

    // Initial run
    eprintln!("\x1b[2m[watch] running {path}...\x1b[0m");
    run_file(path, false, HashSet::new()).await;

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

    // Debounce: wait for events, then re-run after a short pause
    loop {
        rx.recv().await;
        // Drain any additional events within 200ms
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}

        eprintln!();
        eprintln!("\x1b[2m[watch] change detected, re-running {path}...\x1b[0m");
        run_file(path, false, HashSet::new()).await;
    }
}
