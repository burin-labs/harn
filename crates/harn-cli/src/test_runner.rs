use std::path::{Path, PathBuf};
use std::time::Instant;

use harn_lexer::Lexer;
use harn_parser::{Node, Parser};

pub struct TestResult {
    pub name: String,
    pub file: String,
    pub passed: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
}

pub struct TestSummary {
    pub results: Vec<TestResult>,
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    pub duration_ms: u64,
}

/// Run all test_* pipelines in a single source file using the VM.
pub async fn run_test_file(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
) -> Result<Vec<TestResult>, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize().map_err(|e| format!("{e}"))?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| format!("{e}"))?;

    // Find all test_* pipeline names
    let test_names: Vec<String> = program
        .iter()
        .filter_map(|snode| {
            if let Node::Pipeline { name, .. } = &snode.node {
                if name.starts_with("test_") {
                    // Apply filter
                    if let Some(pattern) = filter {
                        if !name.contains(pattern) {
                            return None;
                        }
                    }
                    return Some(name.clone());
                }
            }
            None
        })
        .collect();

    let mut results = Vec::new();

    for test_name in &test_names {
        let start = Instant::now();

        // Compile the test pipeline via the VM compiler
        let chunk = match harn_vm::Compiler::new().compile_named(&program, test_name) {
            Ok(c) => c,
            Err(e) => {
                results.push(TestResult {
                    name: test_name.clone(),
                    file: path.display().to_string(),
                    passed: false,
                    error: Some(format!("Compile error: {e}")),
                    duration_ms: 0,
                });
                continue;
            }
        };

        let local = tokio::task::LocalSet::new();
        let path_str = path.display().to_string();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let result = tokio::time::timeout(
            timeout,
            local.run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                harn_vm::register_http_builtins(&mut vm);
                harn_vm::register_llm_builtins(&mut vm);
                let store_base = path.parent().unwrap_or(std::path::Path::new("."));
                harn_vm::register_store_builtins(&mut vm, store_base);
                harn_vm::register_metadata_builtins(&mut vm, store_base);
                vm.set_source_info(&path_str, &source);
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        vm.set_source_dir(parent);
                    }
                }
                match vm.execute(&chunk).await {
                    Ok(val) => Ok(val),
                    Err(e) => {
                        let formatted = vm.format_runtime_error(&e);
                        Err(formatted)
                    }
                }
            }),
        )
        .await;

        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(_)) => {
                results.push(TestResult {
                    name: test_name.clone(),
                    file: path.display().to_string(),
                    passed: true,
                    error: None,
                    duration_ms: duration,
                });
            }
            Ok(Err(e)) => {
                results.push(TestResult {
                    name: test_name.clone(),
                    file: path.display().to_string(),
                    passed: false,
                    error: Some(e),
                    duration_ms: duration,
                });
            }
            Err(_) => {
                results.push(TestResult {
                    name: test_name.clone(),
                    file: path.display().to_string(),
                    passed: false,
                    error: Some(format!("timed out after {timeout_ms}ms")),
                    duration_ms: timeout_ms,
                });
            }
        }
    }

    Ok(results)
}

/// Discover and run tests in a file or directory.
pub async fn run_tests(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    parallel: bool,
) -> TestSummary {
    // Default LLM provider to "mock" in test mode unless explicitly set
    let prev_provider = std::env::var("HARN_LLM_PROVIDER").ok();
    if prev_provider.is_none() {
        std::env::set_var("HARN_LLM_PROVIDER", "mock");
    }

    let start = Instant::now();
    let mut all_results = Vec::new();

    let files = if path.is_dir() {
        discover_test_files(path)
    } else {
        vec![path.to_path_buf()]
    };

    if parallel {
        // Run files concurrently using spawn_local
        let local = tokio::task::LocalSet::new();
        let results = local
            .run_until(async {
                let mut handles = Vec::new();
                for file in files {
                    let filter = filter.map(|s| s.to_string());
                    handles.push(tokio::task::spawn_local(async move {
                        run_test_file(&file, filter.as_deref(), timeout_ms).await
                    }));
                }
                let mut results = Vec::new();
                for handle in handles {
                    match handle.await {
                        Ok(Ok(r)) => results.extend(r),
                        Ok(Err(e)) => results.push(TestResult {
                            name: "<file error>".to_string(),
                            file: String::new(),
                            passed: false,
                            error: Some(e),
                            duration_ms: 0,
                        }),
                        Err(e) => results.push(TestResult {
                            name: "<join error>".to_string(),
                            file: String::new(),
                            passed: false,
                            error: Some(format!("{e}")),
                            duration_ms: 0,
                        }),
                    }
                }
                results
            })
            .await;
        all_results = results;
    } else {
        for file in &files {
            match run_test_file(file, filter, timeout_ms).await {
                Ok(results) => all_results.extend(results),
                Err(e) => {
                    all_results.push(TestResult {
                        name: "<file error>".to_string(),
                        file: file.display().to_string(),
                        passed: false,
                        error: Some(e),
                        duration_ms: 0,
                    });
                }
            }
        }
    }

    // Restore previous HARN_LLM_PROVIDER state
    match prev_provider {
        Some(val) => std::env::set_var("HARN_LLM_PROVIDER", val),
        None => std::env::remove_var("HARN_LLM_PROVIDER"),
    }

    let passed = all_results.iter().filter(|r| r.passed).count();
    let failed = all_results.iter().filter(|r| !r.passed).count();
    let total = all_results.len();

    TestSummary {
        results: all_results,
        passed,
        failed,
        total,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

fn discover_test_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(discover_test_files(&path));
            } else if path.extension().is_some_and(|e| e == "harn") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if content.contains("test_") {
                        files.push(path);
                    }
                }
            }
        }
    }
    files.sort();
    files
}
