use std::path::{Path, PathBuf};
use std::time::Instant;

use harn_lexer::Lexer;
use harn_parser::{Node, Parser};
use harn_runtime::Interpreter;
use harn_stdlib::{
    register_async_builtins, register_http_builtins, register_llm_builtins,
    register_logging_builtins, register_stdlib, register_tool_builtins,
};

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

/// Run all test_* pipelines in a single source file.
pub async fn run_test_file(path: &Path) -> Result<Vec<TestResult>, String> {
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
                    return Some(name.clone());
                }
            }
            None
        })
        .collect();

    let mut results = Vec::new();

    for test_name in &test_names {
        let start = Instant::now();

        // Create fresh interpreter for each test
        let mut interp = Interpreter::new();
        register_stdlib(&mut interp);
        register_async_builtins(&mut interp);
        register_http_builtins(&mut interp);
        register_llm_builtins(&mut interp);
        register_logging_builtins(&mut interp);
        register_tool_builtins(&mut interp);

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                interp.set_source_dir(parent);
            }
        }

        let local = tokio::task::LocalSet::new();
        let program_clone = program.clone();
        let test_name_clone = test_name.clone();
        let result = local
            .run_until(async { interp.run_test(&program_clone, &test_name_clone).await })
            .await;

        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(()) => {
                results.push(TestResult {
                    name: test_name.clone(),
                    file: path.display().to_string(),
                    passed: true,
                    error: None,
                    duration_ms: duration,
                });
            }
            Err(e) => {
                results.push(TestResult {
                    name: test_name.clone(),
                    file: path.display().to_string(),
                    passed: false,
                    error: Some(format!("{e}")),
                    duration_ms: duration,
                });
            }
        }
    }

    Ok(results)
}

/// Discover and run tests in a file or directory.
pub async fn run_tests(path: &Path) -> TestSummary {
    let start = Instant::now();
    let mut all_results = Vec::new();

    let files = if path.is_dir() {
        discover_test_files(path)
    } else {
        vec![path.to_path_buf()]
    };

    for file in &files {
        match run_test_file(file).await {
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
                // Only include files that have test_ pipelines
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
