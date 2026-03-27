mod test_runner;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::{env, fs, process};

use harn_fmt::format_source;
use harn_lexer::Lexer;
use harn_lint::{lint, LintSeverity};
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};
use harn_runtime::{HarnError, Interpreter};
use harn_stdlib::{register_async_builtins, register_llm_builtins, register_stdlib};

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: harn <command> [args]");
        eprintln!("Commands:");
        eprintln!("  run <file.harn>        Execute a Harn file");
        eprintln!("  lint <file.harn>       Lint a Harn file");
        eprintln!("  fmt <file.harn>        Format a Harn file");
        eprintln!("  test <file|dir>        Run test_* pipelines in file or directory");
        eprintln!("  test conformance       Run conformance test suite");
        eprintln!("  init [name]            Scaffold a new Harn project");
        eprintln!("  install                Install dependencies from harn.toml");
        eprintln!("  repl                   Interactive REPL");
        process::exit(1);
    }

    match args[1].as_str() {
        "version" | "--version" | "-v" => {
            println!(
                r#"
  ╱▔▔╲
 ╱    ╲    harn v0.1.0
 │ ◆  │    the agent harness language
 │    │
 ╰──╯╱    by burin
   ╱╱
"#
            );
        }
        "run" => {
            let use_vm = args.iter().any(|a| a == "--vm");
            let file = args
                .iter()
                .skip(2)
                .find(|a| a.ends_with(".harn") || !a.starts_with("--"));
            match file {
                Some(f) => {
                    if use_vm {
                        run_file_vm(f);
                    } else {
                        run_file(f).await;
                    }
                }
                None => {
                    eprintln!("Usage: harn run [--vm] <file.harn>");
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
            if args.len() < 3 {
                eprintln!("Usage: harn test <file.harn|directory|conformance>");
                process::exit(1);
            }
            if args[2] == "conformance" {
                run_conformance_tests(&args[2]).await;
            } else {
                run_user_tests(&args[2]).await;
            }
        }
        "init" => {
            let name = args.get(2).map(|s| s.as_str());
            init_project(name);
        }
        "repl" => run_repl().await,
        "install" => install_packages(),
        _ => {
            if args[1].ends_with(".harn") {
                run_file(&args[1]).await;
            } else {
                eprintln!("Unknown command: {}", args[1]);
                process::exit(1);
            }
        }
    }
}

fn lint_file(path: &str) {
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
            eprintln!("{path}: lex error: {e}");
            process::exit(1);
        }
    };

    let mut parser = Parser::new(tokens);
    let program = match parser.parse() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}: parse error: {e}");
            process::exit(1);
        }
    };

    let diagnostics = lint(&program);

    if diagnostics.is_empty() {
        println!("{path}: no issues found");
        return;
    }

    let mut has_error = false;
    for diag in &diagnostics {
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

    if has_error {
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

async fn run_file(path: &str) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {path}: {e}");
            process::exit(1);
        }
    };

    match execute(&source, Some(Path::new(path))).await {
        Ok(output) => {
            io::stdout().write_all(&output).ok();
        }
        Err(e) => {
            render_error(&e, &source, path);
            process::exit(1);
        }
    }
}

fn run_file_vm(path: &str) {
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
            eprintln!("{path}: lex error: {e}");
            process::exit(1);
        }
    };

    let mut parser = Parser::new(tokens);
    let program = match parser.parse() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}: parse error: {e}");
            process::exit(1);
        }
    };

    let chunk = match harn_vm::Compiler::new().compile(&program) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{path}: compile error: {e}");
            process::exit(1);
        }
    };

    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);

    match vm.execute(&chunk) {
        Ok(_) => {
            let output = vm.output();
            if !output.is_empty() {
                io::stdout().write_all(output.as_bytes()).ok();
            }
        }
        Err(e) => {
            eprintln!("VM error: {e}");
            process::exit(1);
        }
    }
}

fn render_error(err: &HarnError, source: &str, filename: &str) {
    if let Some(span) = err.span() {
        // Build label and help from the error details
        let (label, help) = match err {
            HarnError::Runtime(harn_runtime::RuntimeError::UndefinedVariable {
                suggestion,
                ..
            }) => (
                Some("not found in this scope"),
                suggestion.as_ref().map(|s| format!("did you mean `{s}`?")),
            ),
            HarnError::Runtime(harn_runtime::RuntimeError::ImmutableAssignment { .. }) => {
                (Some("cannot assign to immutable binding"), None)
            }
            HarnError::Runtime(harn_runtime::RuntimeError::UndefinedBuiltin {
                suggestion, ..
            }) => (
                Some("not found"),
                suggestion.as_ref().map(|s| format!("did you mean `{s}`?")),
            ),
            HarnError::Lexer(_) => (Some("here"), None),
            HarnError::Parser(_) => (Some("unexpected token"), None),
            _ => (None, None),
        };

        let diagnostic = harn_parser::diagnostic::render_diagnostic(
            source,
            filename,
            &span,
            "error",
            &err.to_string(),
            label,
            help.as_deref(),
        );
        eprint!("{diagnostic}");
    } else {
        eprintln!("{err}");
    }
}

async fn execute(source: &str, source_path: Option<&Path>) -> Result<Vec<u8>, HarnError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize()?;

    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    // Static type checking (pre-execution)
    let type_diagnostics = TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        if diag.severity == DiagnosticSeverity::Error {
            return Err(HarnError::Runtime(harn_runtime::RuntimeError::thrown(
                diag.message.clone(),
            )));
        }
    }

    let mut interp = Interpreter::new();
    register_stdlib(&mut interp);
    register_async_builtins(&mut interp);
    register_llm_builtins(&mut interp);

    if let Some(path) = source_path {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                interp.set_source_dir(parent);
            }
        }
    }

    // Use a LocalSet because Interpreter is not Send (contains non-Send futures)
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            interp.run(&program).await?;
            Ok(interp.take_output())
        })
        .await
}

async fn run_conformance_tests(dir: &str) {
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        eprintln!("Directory not found: {dir}");
        process::exit(1);
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut errors: Vec<String> = Vec::new();

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
                .display();

            if expected_file.exists() {
                let source = fs::read_to_string(harn_file).unwrap();
                let expected = fs::read_to_string(&expected_file)
                    .unwrap()
                    .trim_end()
                    .to_string();

                match execute(&source, Some(harn_file.as_path())).await {
                    Ok(output) => {
                        let actual = String::from_utf8_lossy(&output).trim_end().to_string();
                        if actual == expected {
                            println!("  PASS  {rel_path}");
                            passed += 1;
                        } else {
                            println!("  FAIL  {rel_path}");
                            errors.push(format!(
                                "{rel_path}:\n  expected: {expected}\n  actual:   {actual}"
                            ));
                            failed += 1;
                        }
                    }
                    Err(e) => {
                        println!("  FAIL  {rel_path}");
                        errors.push(format!("{rel_path}: runtime error: {e}"));
                        failed += 1;
                    }
                }
            } else if error_file.exists() {
                let source = fs::read_to_string(harn_file).unwrap();
                let expected_error = fs::read_to_string(&error_file)
                    .unwrap()
                    .trim_end()
                    .to_string();

                match execute(&source, Some(harn_file.as_path())).await {
                    Err(ref err) if err.to_string().contains(&expected_error) => {
                        println!("  PASS  {rel_path}");
                        passed += 1;
                    }
                    Err(err) => {
                        println!("  FAIL  {rel_path}");
                        errors.push(format!(
                            "{rel_path}:\n  expected error containing: {expected_error}\n  actual error: {err}"
                        ));
                        failed += 1;
                    }
                    Ok(_) => {
                        println!("  FAIL  {rel_path}");
                        errors.push(format!(
                            "{rel_path}: expected error containing '{expected_error}', but succeeded"
                        ));
                        failed += 1;
                    }
                }
            }
        }
    }

    println!();
    println!(
        "{passed} passed, {failed} failed, {} total",
        passed + failed
    );

    if !errors.is_empty() {
        println!();
        println!("Failures:");
        for err in &errors {
            println!("  {err}");
        }
        process::exit(1);
    }
}

async fn run_user_tests(path_str: &str) {
    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }
    let summary = test_runner::run_tests(&path).await;

    for result in &summary.results {
        if result.passed {
            println!(
                "  \x1b[32mPASS\x1b[0m  {} [{}] ({} ms)",
                result.name, result.file, result.duration_ms
            );
        } else {
            println!("  \x1b[31mFAIL\x1b[0m  {} [{}]", result.name, result.file);
            if let Some(err) = &result.error {
                println!("        {err}");
            }
        }
    }

    println!();
    if summary.failed > 0 {
        println!(
            "{} passed, {} failed, {} total ({} ms)",
            summary.passed, summary.failed, summary.total, summary.duration_ms
        );
        process::exit(1);
    } else if summary.total == 0 {
        println!("No test pipelines found");
    } else {
        println!(
            "{} passed, {} total ({} ms)",
            summary.passed, summary.total, summary.duration_ms
        );
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

async fn run_repl() {
    println!("Harn REPL v0.1.0");
    println!("Type expressions or statements. Ctrl+D to exit.");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("> ");
        stdout.flush().ok();

        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let source = format!("pipeline repl(task) {{\n{line}\n}}");
                match execute(&source, None).await {
                    Ok(output) => {
                        stdout.write_all(&output).ok();
                    }
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            Err(e) => {
                eprintln!("Read error: {e}");
                break;
            }
        }
    }
}

/// Install packages from harn.toml
fn install_packages() {
    let manifest_path = Path::new("harn.toml");
    if !manifest_path.exists() {
        eprintln!("No harn.toml found in current directory.");
        eprintln!("Create one with:");
        eprintln!();
        eprintln!("  [package]");
        eprintln!("  name = \"my-project\"");
        eprintln!("  version = \"0.1.0\"");
        eprintln!();
        eprintln!("  [dependencies]");
        eprintln!("  # name = \"path/to/package\"");
        process::exit(1);
    }

    let content = match fs::read_to_string(manifest_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read harn.toml: {e}");
            process::exit(1);
        }
    };

    // Simple TOML parser for [dependencies] section
    let mut in_deps = false;
    let mut deps: Vec<(String, String)> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed == "[dependencies]" {
            in_deps = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_deps = false;
            continue;
        }
        if in_deps {
            if let Some((key, value)) = trimmed.split_once('=') {
                let name = key.trim().trim_matches('"').to_string();
                let path = value.trim().trim_matches('"').to_string();
                deps.push((name, path));
            }
        }
    }

    if deps.is_empty() {
        println!("No dependencies to install.");
        return;
    }

    // Create .burin/packages directory
    let pkg_dir = PathBuf::from(".burin/packages");
    if let Err(e) = fs::create_dir_all(&pkg_dir) {
        eprintln!("Failed to create package directory: {e}");
        process::exit(1);
    }

    let mut installed = 0;
    for (name, source_path) in &deps {
        let source = Path::new(source_path);
        let dest = pkg_dir.join(name);

        if source.is_dir() {
            // Copy directory
            if dest.exists() {
                println!("  updating {name} from {source_path}");
                let _ = fs::remove_dir_all(&dest);
            } else {
                println!("  installing {name} from {source_path}");
            }
            if let Err(e) = copy_dir_recursive(source, &dest) {
                eprintln!("  failed to install {name}: {e}");
                continue;
            }
        } else if source.is_file() {
            // Copy single file
            let dest_file = pkg_dir.join(format!("{name}.harn"));
            if dest_file.exists() {
                println!("  updating {name} from {source_path}");
            } else {
                println!("  installing {name} from {source_path}");
            }
            if let Err(e) = fs::copy(source, &dest_file) {
                eprintln!("  failed to install {name}: {e}");
                continue;
            }
        } else {
            // Try as .harn file
            let harn_source = PathBuf::from(format!("{source_path}.harn"));
            if harn_source.exists() {
                let dest_file = pkg_dir.join(format!("{name}.harn"));
                println!("  installing {name} from {}", harn_source.display());
                if let Err(e) = fs::copy(&harn_source, &dest_file) {
                    eprintln!("  failed to install {name}: {e}");
                    continue;
                }
            } else {
                eprintln!("  package source not found: {source_path}");
                continue;
            }
        }
        installed += 1;
    }

    println!("\nInstalled {installed} package(s) to .burin/packages/");
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}
