mod a2a;
mod acp;
mod commands;
mod package;
mod test_runner;

use std::path::{Path, PathBuf};
use std::{env, fs, process};

use harn_lexer::Lexer;
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2
        || matches!(
            args.get(1).map(|s| s.as_str()),
            Some("help" | "--help" | "-h")
        )
    {
        print_help();
        process::exit(if args.len() < 2 { 1 } else { 0 });
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
            let deny_csv = args
                .windows(2)
                .find(|w| w[0] == "--deny")
                .map(|w| w[1].clone());
            let allow_csv = args
                .windows(2)
                .find(|w| w[0] == "--allow")
                .map(|w| w[1].clone());

            if deny_csv.is_some() && allow_csv.is_some() {
                eprintln!("error: --deny and --allow cannot be used together");
                process::exit(1);
            }

            // Check for -e inline expression
            let inline_code = args.windows(2).find(|w| w[0] == "-e").map(|w| w[1].clone());

            // Find the .harn file (skip flag values)
            let flag_vals: std::collections::HashSet<&str> = args
                .windows(2)
                .filter(|w| {
                    w[0] == "--arg" || w[0] == "--deny" || w[0] == "--allow" || w[0] == "-e"
                })
                .map(|w| w[1].as_str())
                .collect();
            let file = args
                .iter()
                .skip(2)
                .find(|a| !a.starts_with("--") && *a != "-e" && !flag_vals.contains(a.as_str()));

            // Build the denied builtins set from --deny or --allow flags.
            let denied: std::collections::HashSet<String> =
                commands::run::build_denied_builtins(deny_csv.as_deref(), allow_csv.as_deref());

            if let Some(code) = inline_code {
                // Write inline code to a temp file and run it
                let wrapped = format!("pipeline main(task) {{\n{code}\n}}");
                let tmp_dir = std::env::temp_dir();
                let tmp_path = tmp_dir.join("__harn_eval__.harn");
                fs::write(&tmp_path, &wrapped).unwrap_or_else(|e| {
                    eprintln!("error: failed to write temp file: {e}");
                    process::exit(1);
                });
                let tmp_str = tmp_path.to_string_lossy().to_string();
                commands::run::run_file(&tmp_str, trace, denied).await;
                let _ = fs::remove_file(&tmp_path);
            } else {
                match file {
                    Some(f) => {
                        if bridge {
                            commands::run::run_file_bridge(f, arg_json.as_deref()).await;
                        } else {
                            commands::run::run_file(f, trace, denied).await;
                        }
                    }
                    None => {
                        eprintln!("Usage: harn run [--trace] [--deny <builtins>] [--allow <builtins>] [-e <code>] <file.harn>");
                        process::exit(1);
                    }
                }
            }
        }
        "check" => {
            let file = args
                .iter()
                .skip(2)
                .find(|a| a.ends_with(".harn") || !a.starts_with("--"));
            match file {
                Some(f) => {
                    let config = package::load_check_config(Some(std::path::Path::new(f.as_str())));
                    commands::check::check_file(f, &config);
                }
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
                Some(f) => {
                    let config = package::load_check_config(Some(std::path::Path::new(f.as_str())));
                    commands::check::lint_file(f, &config);
                }
                None => {
                    eprintln!("Usage: harn lint <file.harn>");
                    process::exit(1);
                }
            }
        }
        "fmt" => {
            let check_mode = args.iter().any(|a| a == "--check");
            let targets: Vec<&str> = args
                .iter()
                .skip(2)
                .filter(|a| !a.starts_with("--"))
                .map(|s| s.as_str())
                .collect();
            if targets.is_empty() {
                eprintln!("Usage: harn fmt [--check] <file.harn|dir> [...]");
                process::exit(1);
            }
            commands::check::fmt_targets(&targets, check_mode);
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
            let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
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
                            "--parallel" | "--watch" | "--verbose" | "-v" | "--record" | "--replay"
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
                    commands::test::run_conformance_tests(
                        t,
                        filter.as_deref(),
                        junit_path.as_deref(),
                        timeout_ms,
                        verbose,
                    )
                    .await;
                } else if watch {
                    commands::test::run_watch_tests(t, filter.as_deref(), timeout_ms, parallel)
                        .await;
                } else {
                    commands::test::run_user_tests(t, filter.as_deref(), timeout_ms, parallel)
                        .await;
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
                    commands::test::run_watch_tests(
                        &test_dir,
                        filter.as_deref(),
                        timeout_ms,
                        parallel,
                    )
                    .await;
                } else {
                    commands::test::run_user_tests(
                        &test_dir,
                        filter.as_deref(),
                        timeout_ms,
                        parallel,
                    )
                    .await;
                }
            }
        }
        "init" => {
            let name = args.get(2).map(|s| s.as_str());
            commands::init::init_project(name);
        }
        "serve" => {
            let port: u16 = args
                .windows(2)
                .find(|w| w[0] == "--port")
                .and_then(|w| w[1].parse().ok())
                .unwrap_or(8080);

            let flag_values: std::collections::HashSet<&str> = args
                .windows(2)
                .filter(|w| w[0] == "--port")
                .map(|w| w[1].as_str())
                .collect();

            let file = args
                .iter()
                .skip(2)
                .find(|a| !a.starts_with("--") && !flag_values.contains(a.as_str()));

            match file {
                Some(f) => a2a::run_a2a_server(f, port).await,
                None => {
                    eprintln!("Usage: harn serve [--port N] <file.harn>");
                    process::exit(1);
                }
            }
        }
        "acp" => {
            let pipeline = args.get(2).map(|s| s.as_str());
            acp::run_acp_server(pipeline).await;
        }
        "mcp-serve" => {
            let file = args.iter().skip(2).find(|a| !a.starts_with("--"));
            match file {
                Some(f) => commands::run::run_file_mcp_serve(f).await,
                None => {
                    eprintln!("Usage: harn mcp-serve <file.harn>");
                    process::exit(1);
                }
            }
        }
        "watch" => {
            if args.len() < 3 {
                eprintln!("Usage: harn watch [--deny <builtins>] [--allow <builtins>] <file.harn>");
                process::exit(1);
            }
            let deny_csv = args
                .windows(2)
                .find(|w| w[0] == "--deny")
                .map(|w| w[1].clone());
            let allow_csv = args
                .windows(2)
                .find(|w| w[0] == "--allow")
                .map(|w| w[1].clone());
            let flag_vals: std::collections::HashSet<&str> = args
                .windows(2)
                .filter(|w| w[0] == "--deny" || w[0] == "--allow")
                .map(|w| w[1].as_str())
                .collect();
            let file = args
                .iter()
                .skip(2)
                .find(|a| !a.starts_with("--") && !flag_vals.contains(a.as_str()));
            match file {
                Some(f) => {
                    let denied = commands::run::build_denied_builtins(
                        deny_csv.as_deref(),
                        allow_csv.as_deref(),
                    );
                    commands::run::run_watch(f, denied).await;
                }
                None => {
                    eprintln!(
                        "Usage: harn watch [--deny <builtins>] [--allow <builtins>] <file.harn>"
                    );
                    process::exit(1);
                }
            }
        }
        "repl" => commands::repl::run_repl().await,
        "install" => package::install_packages(),
        "add" => package::add_package(&args[2..]),
        _ => {
            if args[1].ends_with(".harn") {
                commands::run::run_file(&args[1], false, std::collections::HashSet::new()).await;
            } else {
                eprintln!(
                    "\x1b[31merror:\x1b[0m unknown command \x1b[1m{}\x1b[0m",
                    args[1]
                );
                eprintln!();
                eprintln!("Run \x1b[36mharn help\x1b[0m for a list of commands.");
                process::exit(1);
            }
        }
    }
}

fn print_help() {
    let v = env!("CARGO_PKG_VERSION");
    println!("\x1b[1;36mharn\x1b[0m v{v} — the agent harness language\n");
    println!("\x1b[1;33mUSAGE:\x1b[0m");
    println!("    harn <command> [options]\n");
    println!("\x1b[1;33mCOMMANDS:\x1b[0m");
    println!("    \x1b[1;32mrun\x1b[0m <file>             Execute a .harn file");
    println!(
        "    \x1b[1;32mtest\x1b[0m [path]            Run test_* pipelines (auto-discovers tests/)"
    );
    println!(
        "    \x1b[1;32mrepl\x1b[0m                   Interactive REPL with syntax highlighting"
    );
    println!("    \x1b[1;32minit\x1b[0m [name]            Scaffold a new project with harn.toml");
    println!(
        "    \x1b[1;32mfmt\x1b[0m [--check] <files..> Format source code (files or directories)"
    );
    println!("    \x1b[1;32mlint\x1b[0m <file>             Lint for common issues");
    println!("    \x1b[1;32mcheck\x1b[0m <file>            Type-check without executing");
    println!("    \x1b[1;32mwatch\x1b[0m <file>            Watch for changes and re-run");
    println!("    \x1b[1;32mserve\x1b[0m [--port N] <file> Serve as an A2A agent over HTTP");
    println!("    \x1b[1;32macp\x1b[0m [file]              Start ACP server on stdio");
    println!("    \x1b[1;32mmcp-serve\x1b[0m <file>        Serve tools as MCP server on stdio");
    println!("    \x1b[1;32madd\x1b[0m <name> --git <url>  Add a dependency to harn.toml");
    println!("    \x1b[1;32minstall\x1b[0m                 Install dependencies from harn.toml");
    println!("    \x1b[1;32mversion\x1b[0m                 Show version info");
    println!("    \x1b[1;32mhelp\x1b[0m                    Show this help");
    println!();
    println!("\x1b[1;33mRUN OPTIONS:\x1b[0m");
    println!("    --trace              Print LLM trace summary after execution");
    println!("    --deny <builtins>    Deny specific builtins (comma-separated)");
    println!("    --allow <builtins>   Allow only specific builtins (comma-separated)");
    println!("    --sandbox            Restrict file/network access");
    println!();
    println!("\x1b[1;33mTEST OPTIONS:\x1b[0m");
    println!("    --filter <pattern>   Only run tests matching pattern");
    println!("    --watch              Re-run tests on file changes");
    println!("    --parallel           Run tests concurrently");
    println!("    --verbose / -v       Show per-test timing and detailed failures");
    println!("    --record / --replay  Record or replay LLM fixtures");
    println!();
    println!("\x1b[1;33mEXAMPLES:\x1b[0m");
    println!("    harn run main.harn");
    println!("    harn test tests/");
    println!("    harn init my-project");
    println!("    harn fmt --check src/");
    println!();
    println!("Docs: \x1b[4;36mhttps://github.com/burin-labs/harn\x1b[0m");
}

/// Parse a .harn file, returning (source, AST). Exits on error.
pub(crate) fn parse_source_file(path: &str) -> (String, Vec<harn_parser::SNode>) {
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

/// Execute source code and return the output. Used by REPL and conformance tests.
pub(crate) async fn execute(source: &str, source_path: Option<&Path>) -> Result<String, String> {
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
            let source_parent = source_path
                .and_then(|p| p.parent())
                .unwrap_or(std::path::Path::new("."));
            let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
            let store_base = project_root.as_deref().unwrap_or(source_parent);
            harn_vm::register_store_builtins(&mut vm, store_base);
            harn_vm::register_metadata_builtins(&mut vm, store_base);
            let pipeline_name = source_path
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .unwrap_or("default");
            harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
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
            vm.execute(&chunk).await.map_err(|e| e.to_string())?;
            Ok(vm.output().to_string())
        })
        .await
}
