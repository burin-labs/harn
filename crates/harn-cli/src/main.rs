use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::{env, fs, process};

use harn_lexer::Lexer;
use harn_parser::Parser;
use harn_runtime::{HarnError, Interpreter};
use harn_stdlib::register_stdlib;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: harn <command> [args]");
        eprintln!("Commands:");
        eprintln!("  run <file.harn>        Execute a Harn file");
        eprintln!("  test <directory>       Run conformance test suite");
        eprintln!("  repl                   Interactive REPL");
        process::exit(1);
    }

    match args[1].as_str() {
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: harn run <file.harn>");
                process::exit(1);
            }
            run_file(&args[2]).await;
        }
        "test" => {
            let dir = if args.len() >= 3 {
                &args[2]
            } else {
                "conformance"
            };
            run_conformance_tests(dir).await;
        }
        "repl" => run_repl().await,
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
            eprintln!("{e}");
            process::exit(1);
        }
    }
}

async fn execute(source: &str, source_path: Option<&Path>) -> Result<Vec<u8>, HarnError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize()?;

    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    let mut interp = Interpreter::new();
    register_stdlib(&mut interp);

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
