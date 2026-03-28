mod package;
mod test_runner;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::{env, fs, process};

use harn_fmt::format_source;
use harn_lexer::Lexer;
use harn_lint::{lint, LintSeverity};
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: harn <command> [args]");
        eprintln!("Commands:");
        eprintln!("  run [--trace] <file>   Execute a Harn file");
        eprintln!("  check <file.harn>      Type-check and lint without executing");
        eprintln!("  lint <file.harn>       Lint a Harn file");
        eprintln!("  fmt [--check] <file>   Format a Harn file");
        eprintln!("  test [path]            Run test_* pipelines (auto-discovers tests/)");
        eprintln!("  test conformance       Run conformance test suite");
        eprintln!("  init [name]            Scaffold a new Harn project");
        eprintln!("  add <name> --git <url> Add a dependency to harn.toml");
        eprintln!("  install                Install dependencies from harn.toml");
        eprintln!("  repl                   Interactive REPL");
        eprintln!();
        eprintln!("Test flags: --filter <pattern> --watch --record --replay --parallel");
        process::exit(1);
    }

    match args[1].as_str() {
        "version" | "--version" | "-v" => {
            println!(
                r#"
  ╱▔▔╲
 ╱    ╲    harn v{}
 │ ◆  │    the agent harness language
 │    │
 ╰──╯╱    by burin
   ╱╱
"#,
                env!("CARGO_PKG_VERSION")
            );
        }
        "run" => {
            let trace = args.iter().any(|a| a == "--trace");
            let bridge = args.iter().any(|a| a == "--bridge");
            let arg_json = args
                .windows(2)
                .find(|w| w[0] == "--arg")
                .map(|w| w[1].clone());

            // Find the .harn file (skip flag values)
            let flag_vals: std::collections::HashSet<&str> = args
                .windows(2)
                .filter(|w| w[0] == "--arg")
                .map(|w| w[1].as_str())
                .collect();
            let file = args
                .iter()
                .skip(2)
                .find(|a| !a.starts_with("--") && !flag_vals.contains(a.as_str()));

            match file {
                Some(f) => {
                    if bridge {
                        run_file_bridge(f, arg_json.as_deref()).await;
                    } else {
                        run_file(f, trace).await;
                    }
                }
                None => {
                    eprintln!("Usage: harn run [--trace] [--bridge --arg <json>] <file.harn>");
                    process::exit(1);
                }
            }
        }
        "check" => {
            let file = args
                .iter()
                .skip(2)
                .find(|a| a.ends_with(".harn") || !a.starts_with("--"));
            match file {
                Some(f) => check_file(f),
                None => {
                    eprintln!("Usage: harn check <file.harn>");
                    process::exit(1);
                }
            }
        }
        "lint" => {
            let file = args
                .iter()
                .skip(2)
                .find(|a| a.ends_with(".harn") || !a.starts_with("--"));
            match file {
                Some(f) => lint_file(f),
                None => {
                    eprintln!("Usage: harn lint <file.harn>");
                    process::exit(1);
                }
            }
        }
        "fmt" => {
            let check_mode = args.iter().any(|a| a == "--check");
            let file = args
                .iter()
                .skip(2)
                .find(|a| a.ends_with(".harn") || !a.starts_with("--"));
            match file {
                Some(f) => fmt_file(f, check_mode),
                None => {
                    eprintln!("Usage: harn fmt [--check] <file.harn>");
                    process::exit(1);
                }
            }
        }
        "test" => {
            // Parse test flags
            let filter = args
                .windows(2)
                .find(|w| w[0] == "--filter")
                .map(|w| w[1].clone());
            let junit_path = args
                .windows(2)
                .find(|w| w[0] == "--junit")
                .map(|w| w[1].clone());
            let timeout_ms: u64 = args
                .windows(2)
                .find(|w| w[0] == "--timeout")
                .and_then(|w| w[1].parse().ok())
                .unwrap_or(30_000);
            let parallel = args.iter().any(|a| a == "--parallel");
            let watch = args.iter().any(|a| a == "--watch");
            let record = args.iter().any(|a| a == "--record");
            let replay = args.iter().any(|a| a == "--replay");

            // Set up LLM replay mode
            if record {
                harn_vm::llm::set_replay_mode(
                    harn_vm::llm::LlmReplayMode::Record,
                    ".harn-fixtures",
                );
            } else if replay {
                harn_vm::llm::set_replay_mode(
                    harn_vm::llm::LlmReplayMode::Replay,
                    ".harn-fixtures",
                );
            }

            // Collect flag values to exclude from target search
            let flag_values: std::collections::HashSet<&str> = args
                .windows(2)
                .filter(|w| {
                    w[0].starts_with("--")
                        && !matches!(
                            w[0].as_str(),
                            "--parallel" | "--watch" | "--record" | "--replay"
                        )
                })
                .map(|w| w[1].as_str())
                .collect();

            let target = args
                .iter()
                .skip(2)
                .find(|a| !a.starts_with("--") && !flag_values.contains(a.as_str()));

            if let Some(t) = target {
                if t == "conformance" {
                    run_conformance_tests(t, filter.as_deref(), junit_path.as_deref(), timeout_ms)
                        .await;
                } else if watch {
                    run_watch_tests(t, filter.as_deref(), timeout_ms, parallel).await;
                } else {
                    run_user_tests(t, filter.as_deref(), timeout_ms, parallel).await;
                }
            } else {
                // Auto-discover tests/ directory
                let test_dir = if PathBuf::from("tests").is_dir() {
                    "tests".to_string()
                } else {
                    eprintln!(
                        "Usage: harn test [path] [--filter <pattern>] [--watch] [--parallel]"
                    );
                    eprintln!("       harn test conformance");
                    eprintln!("\nNo path specified and no tests/ directory found.");
                    process::exit(1);
                };
                if watch {
                    run_watch_tests(&test_dir, filter.as_deref(), timeout_ms, parallel).await;
                } else {
                    run_user_tests(&test_dir, filter.as_deref(), timeout_ms, parallel).await;
                }
            }
        }
        "init" => {
            let name = args.get(2).map(|s| s.as_str());
            init_project(name);
        }
        "repl" => run_repl().await,
        "install" => package::install_packages(),
        "add" => package::add_package(&args[2..]),
        _ => {
            if args[1].ends_with(".harn") {
                run_file(&args[1], false).await;
            } else {
                eprintln!("Unknown command: {}", args[1]);
                process::exit(1);
            }
        }
    }
}

/// Parse a .harn file, returning (source, AST). Exits on error.
fn parse_source_file(path: &str) -> (String, Vec<harn_parser::SNode>) {
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
            let diagnostic = harn_parser::diagnostic::render_diagnostic(
                &source,
                path,
                &error_span_from_lex(&e),
                "error",
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
        Err(_) => {
            for e in parser.all_errors() {
                let span = error_span_from_parse(e);
                let diagnostic = harn_parser::diagnostic::render_diagnostic(
                    &source,
                    path,
                    &span,
                    "error",
                    &e.to_string(),
                    Some("unexpected token"),
                    None,
                );
                eprint!("{diagnostic}");
            }
            process::exit(1);
        }
    };

    (source, program)
}

fn print_lint_diagnostics(path: &str, diagnostics: &[harn_lint::LintDiagnostic]) -> bool {
    let mut has_error = false;
    for diag in diagnostics {
        let severity = match diag.severity {
            LintSeverity::Warning => "warning",
            LintSeverity::Error => {
                has_error = true;
                "error"
            }
        };
        println!(
            "{path}:{}:{}: {severity}[{}]: {}",
            diag.span.line, diag.span.column, diag.rule, diag.message
        );
        if let Some(ref suggestion) = diag.suggestion {
            println!("  suggestion: {suggestion}");
        }
    }
    has_error
}

fn check_file(path: &str) {
    let (source, program) = parse_source_file(path);

    let mut has_error = false;
    let mut diagnostic_count = 0;

    // Type checking
    let type_diagnostics = TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        let severity = match diag.severity {
            DiagnosticSeverity::Error => {
                has_error = true;
                "error"
            }
            DiagnosticSeverity::Warning => "warning",
        };
        diagnostic_count += 1;
        if let Some(span) = &diag.span {
            let rendered = harn_parser::diagnostic::render_diagnostic(
                &source,
                path,
                span,
                severity,
                &diag.message,
                None,
                None,
            );
            eprint!("{rendered}");
        } else {
            eprintln!("{severity}: {}", diag.message);
        }
    }

    // Linting
    let lint_diagnostics = lint(&program);
    diagnostic_count += lint_diagnostics.len();
    if print_lint_diagnostics(path, &lint_diagnostics) {
        has_error = true;
    }

    if diagnostic_count == 0 {
        println!("{path}: ok");
    }

    if has_error {
        process::exit(1);
    }
}

fn lint_file(path: &str) {
    let (_source, program) = parse_source_file(path);

    let diagnostics = lint(&program);

    if diagnostics.is_empty() {
        println!("{path}: no issues found");
        return;
    }

    if print_lint_diagnostics(path, &diagnostics) {
        process::exit(1);
    }
}

fn fmt_file(path: &str, check_mode: bool) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {path}: {e}");
            process::exit(1);
        }
    };

    let formatted = match format_source(&source) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{path}: {e}");
            process::exit(1);
        }
    };

    if check_mode {
        if source != formatted {
            eprintln!("{path}: would be reformatted");
            process::exit(1);
        }
        println!("{path}: ok");
    } else if source != formatted {
        match fs::write(path, &formatted) {
            Ok(()) => println!("formatted {path}"),
            Err(e) => {
                eprintln!("Error writing {path}: {e}");
                process::exit(1);
            }
        }
    } else {
        println!("{path}: already formatted");
    }
}

async fn run_file(path: &str, trace: bool) {
    let (source, program) = parse_source_file(path);

    // Static type checking
    let type_diagnostics = TypeChecker::new().check(&program);
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
    vm.set_source_info(path, &source);

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

async fn run_file_bridge(path: &str, arg_json: Option<&str>) {
    let (source, program) = parse_source_file(path);

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
    let source_owned = source;
    let arg_owned = arg_json.map(|s| s.to_string());

    let exit_code = local
        .run_until(async move {
            let bridge = std::rc::Rc::new(harn_vm::bridge::HostBridge::new());

            let mut vm = harn_vm::Vm::new();

            // Register language builtins (string ops, math, json, etc.)
            harn_vm::register_vm_stdlib(&mut vm);

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

fn error_span_from_lex(e: &harn_lexer::LexerError) -> harn_lexer::Span {
    match e {
        harn_lexer::LexerError::UnexpectedCharacter(_, span)
        | harn_lexer::LexerError::UnterminatedString(span)
        | harn_lexer::LexerError::UnterminatedBlockComment(span) => *span,
    }
}

fn error_span_from_parse(e: &harn_parser::ParserError) -> harn_lexer::Span {
    match e {
        harn_parser::ParserError::Unexpected { span, .. } => *span,
        harn_parser::ParserError::UnexpectedEof { .. } => harn_lexer::Span::dummy(),
    }
}

async fn execute(source: &str, source_path: Option<&Path>) -> Result<String, String> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| e.to_string())?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| e.to_string())?;

    // Static type checking (same as interpreter path)
    let type_diagnostics = TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        if diag.severity == DiagnosticSeverity::Error {
            return Err(diag.message.clone());
        }
    }

    let chunk = harn_vm::Compiler::new()
        .compile(&program)
        .map_err(|e| e.to_string())?;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut vm = harn_vm::Vm::new();
            harn_vm::register_vm_stdlib(&mut vm);
            harn_vm::register_http_builtins(&mut vm);
            harn_vm::register_llm_builtins(&mut vm);
            if let Some(path) = source_path {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        vm.set_source_dir(parent);
                    }
                }
            }
            vm.execute(&chunk).await.map_err(|e| e.to_string())?;
            Ok(vm.output().to_string())
        })
        .await
}

/// Produce a simple line diff between expected and actual.
fn simple_diff(expected: &str, actual: &str) -> String {
    let mut result = String::new();
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let max = expected_lines.len().max(actual_lines.len());
    for i in 0..max {
        let exp = expected_lines.get(i).copied().unwrap_or("");
        let act = actual_lines.get(i).copied().unwrap_or("");
        if exp == act {
            result.push_str(&format!("  {exp}\n"));
        } else {
            result.push_str(&format!("\x1b[31m- {exp}\x1b[0m\n"));
            result.push_str(&format!("\x1b[32m+ {act}\x1b[0m\n"));
        }
    }
    result
}

/// Write JUnit XML report.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn write_junit_xml(path: &str, results: &[(String, bool, String, u64)]) {
    let total = results.len();
    let failures = results.iter().filter(|r| !r.1).count();
    let total_time: f64 = results.iter().map(|r| r.3 as f64 / 1000.0).sum();

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"harn\" tests=\"{total}\" failures=\"{failures}\" time=\"{total_time:.3}\">\n"
    ));
    for (name, passed, error_msg, duration_ms) in results {
        let time = *duration_ms as f64 / 1000.0;
        let escaped_name = xml_escape(name);
        xml.push_str(&format!(
            "  <testcase name=\"{escaped_name}\" time=\"{time:.3}\""
        ));
        if *passed {
            xml.push_str(" />\n");
        } else {
            xml.push_str(">\n");
            let escaped = xml_escape(error_msg);
            xml.push_str(&format!(
                "    <failure message=\"test failed\">{escaped}</failure>\n"
            ));
            xml.push_str("  </testcase>\n");
        }
    }
    xml.push_str("</testsuite>\n");

    if let Err(e) = fs::write(path, &xml) {
        eprintln!("Failed to write JUnit XML to {path}: {e}");
    } else {
        println!("JUnit XML written to {path}");
    }
}

async fn run_conformance_tests(
    dir: &str,
    filter: Option<&str>,
    junit_path: Option<&str>,
    timeout_ms: u64,
) {
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        eprintln!("Directory not found: {dir}");
        process::exit(1);
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut errors: Vec<String> = Vec::new();
    // (name, passed, error_msg, duration_ms)
    let mut junit_results: Vec<(String, bool, String, u64)> = Vec::new();

    let mut test_dirs = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir_path) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                test_dirs.push(entry.path());
            }
        }
    }
    test_dirs.sort();
    test_dirs.insert(0, dir_path.clone());

    for test_dir in &test_dirs {
        let mut harn_files: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = fs::read_dir(test_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "harn").unwrap_or(false) {
                    harn_files.push(path);
                }
            }
        }
        harn_files.sort();

        for harn_file in &harn_files {
            let expected_file = harn_file.with_extension("expected");
            let error_file = harn_file.with_extension("error");

            let rel_path = harn_file
                .strip_prefix(&dir_path)
                .unwrap_or(harn_file)
                .display()
                .to_string();

            // Apply filter
            if let Some(pattern) = filter {
                if !rel_path.contains(pattern) {
                    continue;
                }
            }

            if expected_file.exists() {
                let source = match fs::read_to_string(harn_file) {
                    Ok(s) => s,
                    Err(e) => {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        let msg = format!("{rel_path}: IO error reading source: {e}");
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, 0));
                        failed += 1;
                        continue;
                    }
                };
                let expected = match fs::read_to_string(&expected_file) {
                    Ok(s) => s.trim_end().to_string(),
                    Err(e) => {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        let msg = format!("{rel_path}: IO error reading expected: {e}");
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, 0));
                        failed += 1;
                        continue;
                    }
                };

                let start = std::time::Instant::now();
                let result = tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms),
                    execute(&source, Some(harn_file.as_path())),
                )
                .await;
                let duration_ms = start.elapsed().as_millis() as u64;

                match result {
                    Ok(Ok(output)) => {
                        let actual = output.trim_end().to_string();
                        if actual == expected {
                            println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
                            junit_results.push((rel_path, true, String::new(), duration_ms));
                            passed += 1;
                        } else {
                            println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                            let diff = simple_diff(&expected, &actual);
                            let msg = format!("{rel_path}:\n{diff}");
                            errors.push(msg.clone());
                            junit_results.push((rel_path, false, msg, duration_ms));
                            failed += 1;
                        }
                    }
                    Ok(Err(e)) => {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        let msg = format!("{rel_path}: runtime error: {e}");
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, duration_ms));
                        failed += 1;
                    }
                    Err(_) => {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        let msg = format!("{rel_path}: timed out after {timeout_ms}ms");
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, timeout_ms));
                        failed += 1;
                    }
                }
            } else if error_file.exists() {
                let source = match fs::read_to_string(harn_file) {
                    Ok(s) => s,
                    Err(e) => {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        let msg = format!("{rel_path}: IO error reading source: {e}");
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, 0));
                        failed += 1;
                        continue;
                    }
                };
                let expected_error = match fs::read_to_string(&error_file) {
                    Ok(s) => s.trim_end().to_string(),
                    Err(e) => {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        let msg = format!("{rel_path}: IO error reading expected error: {e}");
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, 0));
                        failed += 1;
                        continue;
                    }
                };

                let start = std::time::Instant::now();
                let result = tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms),
                    execute(&source, Some(harn_file.as_path())),
                )
                .await;
                let duration_ms = start.elapsed().as_millis() as u64;

                match result {
                    Ok(Err(ref err)) if err.contains(&expected_error) => {
                        println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
                        junit_results.push((rel_path, true, String::new(), duration_ms));
                        passed += 1;
                    }
                    Ok(Err(err)) => {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        let msg = format!(
                            "{rel_path}:\n  expected error containing: {expected_error}\n  actual error: {err}"
                        );
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, duration_ms));
                        failed += 1;
                    }
                    Ok(Ok(_)) => {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        let msg = format!(
                            "{rel_path}: expected error containing '{expected_error}', but succeeded"
                        );
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, duration_ms));
                        failed += 1;
                    }
                    Err(_) => {
                        println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                        let msg = format!("{rel_path}: timed out after {timeout_ms}ms");
                        errors.push(msg.clone());
                        junit_results.push((rel_path, false, msg, timeout_ms));
                        failed += 1;
                    }
                }
            }
        }
    }

    println!();
    if failed > 0 {
        println!(
            "\x1b[31m{passed} passed, {failed} failed, {} total\x1b[0m",
            passed + failed
        );
    } else {
        println!(
            "\x1b[32m{passed} passed, {failed} failed, {} total\x1b[0m",
            passed + failed
        );
    }

    if let Some(path) = junit_path {
        write_junit_xml(path, &junit_results);
    }

    if !errors.is_empty() {
        println!();
        println!("Failures:");
        for err in &errors {
            println!("  {err}");
        }
        process::exit(1);
    }
}

fn print_test_results(summary: &test_runner::TestSummary) {
    // Count unique files
    let file_count = summary
        .results
        .iter()
        .map(|r| r.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();

    // Test count header
    if summary.total > 0 {
        println!(
            "Running {} test{} from {} file{}...\n",
            summary.total,
            if summary.total == 1 { "" } else { "s" },
            file_count,
            if file_count == 1 { "" } else { "s" },
        );
    }

    for result in &summary.results {
        if result.passed {
            println!(
                "  \x1b[32mPASS\x1b[0m  {} [{}] ({} ms)",
                result.name, result.file, result.duration_ms
            );
        } else {
            println!("  \x1b[31mFAIL\x1b[0m  {} [{}]", result.name, result.file);
            if let Some(err) = &result.error {
                // Indent multi-line errors
                for line in err.lines() {
                    println!("        {line}");
                }
            }
        }
    }

    println!();
    if summary.failed > 0 {
        println!(
            "\x1b[31m{} passed, {} failed, {} total ({} ms)\x1b[0m",
            summary.passed, summary.failed, summary.total, summary.duration_ms
        );
    } else if summary.total == 0 {
        println!("No test pipelines found");
    } else {
        println!(
            "\x1b[32m{} passed, {} total ({} ms)\x1b[0m",
            summary.passed, summary.total, summary.duration_ms
        );
    }
}

async fn run_user_tests(path_str: &str, filter: Option<&str>, timeout_ms: u64, parallel: bool) {
    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }
    let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
    print_test_results(&summary);
    if summary.failed > 0 {
        process::exit(1);
    }
}

async fn run_watch_tests(path_str: &str, filter: Option<&str>, timeout_ms: u64, parallel: bool) {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::Duration;

    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }

    println!("Watching {path_str} for changes... (Ctrl+C to stop)\n");

    // Initial run
    let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
    print_test_results(&summary);

    // Set up file watcher
    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default()).unwrap_or_else(|e| {
        eprintln!("Failed to create file watcher: {e}");
        process::exit(1);
    });
    watcher
        .watch(&path, RecursiveMode::Recursive)
        .unwrap_or_else(|e| {
            eprintln!("Failed to watch {path_str}: {e}");
            process::exit(1);
        });

    loop {
        // Wait for a file change event
        match rx.recv() {
            Ok(Ok(event)) => {
                // Only re-run for .harn file modifications
                let is_harn = event
                    .paths
                    .iter()
                    .any(|p| p.extension().is_some_and(|e| e == "harn"));
                if !is_harn {
                    continue;
                }

                // Debounce: drain any queued events
                while rx.recv_timeout(Duration::from_millis(100)).is_ok() {}

                println!("\n\x1b[2m─── file changed, re-running tests ───\x1b[0m\n");
                let summary = test_runner::run_tests(&path, filter, timeout_ms, parallel).await;
                print_test_results(&summary);
            }
            Ok(Err(e)) => {
                eprintln!("Watch error: {e}");
            }
            Err(_) => break,
        }
    }
}

fn init_project(name: Option<&str>) {
    let dir = match name {
        Some(n) => {
            let dir = PathBuf::from(n);
            if dir.exists() {
                eprintln!("Directory '{}' already exists", n);
                process::exit(1);
            }
            fs::create_dir_all(&dir).unwrap_or_else(|e| {
                eprintln!("Failed to create directory: {e}");
                process::exit(1);
            });
            println!("Creating project '{}'...", n);
            dir
        }
        None => {
            println!("Initializing harn project in current directory...");
            PathBuf::from(".")
        }
    };

    // Create directories
    fs::create_dir_all(dir.join("lib")).ok();
    fs::create_dir_all(dir.join("tests")).ok();

    // main.harn
    let main_content = r#"import "lib/helpers"

pipeline default(task) {
  let greeting = greet("world")
  log(greeting)
}
"#;

    // lib/helpers.harn
    let helpers_content = r#"fn greet(name) {
  return "Hello, " + name + "!"
}

fn add(a, b) {
  return a + b
}
"#;

    // tests/test_main.harn
    let test_content = r#"import "../lib/helpers"

pipeline test_greet(task) {
  assert_eq(greet("world"), "Hello, world!")
  assert_eq(greet("Harn"), "Hello, Harn!")
}

pipeline test_add(task) {
  assert_eq(add(2, 3), 5)
  assert_eq(add(-1, 1), 0)
  assert_eq(add(0, 0), 0)
}
"#;

    // Write files (don't overwrite existing)
    write_if_new(&dir.join("main.harn"), main_content);
    write_if_new(&dir.join("lib/helpers.harn"), helpers_content);
    write_if_new(&dir.join("tests/test_main.harn"), test_content);

    println!();
    if let Some(n) = name {
        println!("  cd {}", n);
    }
    println!("  harn run main.harn       # run the program");
    println!("  harn test tests/         # run the tests");
    println!("  harn fmt main.harn       # format code");
    println!("  harn lint main.harn      # lint code");
}

fn write_if_new(path: &Path, content: &str) {
    if path.exists() {
        println!("  skip  {} (already exists)", path.display());
    } else {
        fs::write(path, content).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {e}", path.display());
        });
        println!("  create  {}", path.display());
    }
}

/// Harn REPL keyword completer.
struct HarnCompleter {
    keywords: Vec<String>,
}

impl reedline::Completer for HarnCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<reedline::Suggestion> {
        let text = &line[..pos];
        let word_start = text
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prefix = &text[word_start..];
        if prefix.is_empty() {
            return Vec::new();
        }

        self.keywords
            .iter()
            .filter(|kw| kw.starts_with(prefix) && kw.as_str() != prefix)
            .map(|kw| reedline::Suggestion {
                value: kw.clone(),
                description: None,
                style: None,
                extra: None,
                span: reedline::Span::new(word_start, pos),
                append_whitespace: true,
            })
            .collect()
    }
}

/// Harn REPL syntax highlighter.
struct HarnHighlighter {
    keywords: Vec<String>,
}

impl reedline::Highlighter for HarnHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> reedline::StyledText {
        let mut styled = reedline::StyledText::new();
        let mut remaining = line;

        while !remaining.is_empty() {
            if remaining.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                let end = remaining
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(remaining.len());
                let word = &remaining[..end];
                if self.keywords.contains(&word.to_string()) {
                    styled.push((
                        nu_ansi_term::Style::new()
                            .fg(nu_ansi_term::Color::Blue)
                            .bold(),
                        word.to_string(),
                    ));
                } else if word == "true" || word == "false" || word == "nil" {
                    styled.push((
                        nu_ansi_term::Style::new().fg(nu_ansi_term::Color::Yellow),
                        word.to_string(),
                    ));
                } else {
                    styled.push((nu_ansi_term::Style::new(), word.to_string()));
                }
                remaining = &remaining[end..];
            } else if remaining.starts_with('"') {
                let end = remaining[1..]
                    .find('"')
                    .map(|i| i + 2)
                    .unwrap_or(remaining.len());
                let s = &remaining[..end];
                styled.push((
                    nu_ansi_term::Style::new().fg(nu_ansi_term::Color::Green),
                    s.to_string(),
                ));
                remaining = &remaining[end..];
            } else if remaining.starts_with("//") {
                styled.push((
                    nu_ansi_term::Style::new().fg(nu_ansi_term::Color::DarkGray),
                    remaining.to_string(),
                ));
                remaining = "";
            } else if remaining.starts_with(|c: char| c.is_ascii_digit()) {
                let end = remaining
                    .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '_')
                    .unwrap_or(remaining.len());
                let num = &remaining[..end];
                styled.push((
                    nu_ansi_term::Style::new().fg(nu_ansi_term::Color::Cyan),
                    num.to_string(),
                ));
                remaining = &remaining[end..];
            } else {
                let ch = &remaining[..remaining.ceil_char_boundary(1)];
                styled.push((nu_ansi_term::Style::new(), ch.to_string()));
                remaining = &remaining[ch.len()..];
            }
        }
        styled
    }
}

/// Harn REPL validator for multi-line input.
struct HarnValidator;

impl reedline::Validator for HarnValidator {
    fn validate(&self, line: &str) -> reedline::ValidationResult {
        let open_braces = line.chars().filter(|c| *c == '{').count();
        let close_braces = line.chars().filter(|c| *c == '}').count();
        let open_parens = line.chars().filter(|c| *c == '(').count();
        let close_parens = line.chars().filter(|c| *c == ')').count();
        let open_brackets = line.chars().filter(|c| *c == '[').count();
        let close_brackets = line.chars().filter(|c| *c == ']').count();

        if open_braces > close_braces
            || open_parens > close_parens
            || open_brackets > close_brackets
        {
            reedline::ValidationResult::Incomplete
        } else {
            reedline::ValidationResult::Complete
        }
    }
}

async fn run_repl() {
    use reedline::{DefaultPrompt, DefaultPromptSegment, FileBackedHistory, Reedline, Signal};

    println!("Harn REPL v0.1.0");
    println!("Type expressions or statements. Ctrl+D to exit.");

    let harn_keywords: Vec<String> = [
        "pipeline",
        "fn",
        "let",
        "var",
        "if",
        "else",
        "for",
        "in",
        "while",
        "match",
        "return",
        "break",
        "continue",
        "import",
        "from",
        "try",
        "catch",
        "throw",
        "spawn",
        "parallel",
        "parallel_map",
        "retry",
        "guard",
        "deadline",
        "mutex",
        "enum",
        "struct",
        "type",
        "pub",
        "extends",
        "override",
        "true",
        "false",
        "nil",
        "log",
        "print",
        "println",
        "assert",
        "assert_eq",
        "assert_ne",
        "type_of",
        "to_string",
        "to_int",
        "to_float",
        "json_stringify",
        "json_parse",
        "read_file",
        "write_file",
        "file_exists",
        "exec",
        "env",
        "timestamp",
        "abs",
        "min",
        "max",
        "floor",
        "ceil",
        "round",
        "sqrt",
        "pow",
        "random",
        "regex_match",
        "regex_replace",
        "http_get",
        "http_post",
        "llm_call",
        "llm_stream",
        "channel",
        "send",
        "receive",
        "close",
        "sleep",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let completer = Box::new(HarnCompleter {
        keywords: harn_keywords.clone(),
    });
    let highlighter = Box::new(HarnHighlighter {
        keywords: harn_keywords,
    });
    let validator = Box::new(HarnValidator);

    let history_path = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".harn_history"))
        .unwrap_or_else(|| PathBuf::from(".harn_history"));

    let history = Box::new(
        FileBackedHistory::with_file(1000, history_path)
            .unwrap_or_else(|_| FileBackedHistory::new(1000).expect("history")),
    );

    let mut line_editor = Reedline::create()
        .with_completer(completer)
        .with_highlighter(highlighter)
        .with_validator(validator)
        .with_history(history);

    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("harn".to_string()),
        DefaultPromptSegment::Empty,
    );

    loop {
        // Run reedline in spawn_blocking since it blocks on terminal input
        let input = tokio::task::spawn_blocking({
            let mut editor = std::mem::replace(&mut line_editor, Reedline::create());
            let prompt = prompt.clone();
            move || {
                let result = editor.read_line(&prompt);
                (editor, result)
            }
        })
        .await;

        match input {
            Ok((editor, Ok(Signal::Success(line)))) => {
                line_editor = editor;
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let source = format!("pipeline repl(task) {{\n{line}\n}}");
                match execute(&source, None).await {
                    Ok(output) => {
                        if !output.is_empty() {
                            io::stdout().write_all(output.as_bytes()).ok();
                        }
                    }
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            Ok((_, Ok(Signal::CtrlC))) => continue,
            Ok((_, Ok(Signal::CtrlD))) => {
                println!("Goodbye!");
                break;
            }
            Ok((_editor, Err(e))) => {
                eprintln!("Read error: {e}");
                break;
            }
            Err(e) => {
                eprintln!("Runtime error: {e}");
                break;
            }
        }
    }
}
