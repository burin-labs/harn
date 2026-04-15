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

fn canonicalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn test_execution_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Run all test_* pipelines in a single source file using the VM.
pub async fn run_test_file(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    execution_cwd: Option<&Path>,
) -> Result<Vec<TestResult>, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize().map_err(|e| format!("{e}"))?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| format!("{e}"))?;

    let test_names: Vec<String> = program
        .iter()
        .filter_map(|snode| {
            // Recognize either:
            //  - the legacy naming convention: `pipeline test_*`
            //  - the explicit `@test` attribute on a Pipeline (declarative)
            let (has_test_attr, decl_node) = match &snode.node {
                Node::AttributedDecl { attributes, inner } => {
                    (attributes.iter().any(|a| a.name == "test"), inner.as_ref())
                }
                _ => (false, snode),
            };
            let name = match &decl_node.node {
                Node::Pipeline { name, .. } => name.clone(),
                _ => return None,
            };
            if !(has_test_attr || name.starts_with("test_")) {
                return None;
            }
            if let Some(pattern) = filter {
                if !name.contains(pattern) {
                    return None;
                }
            }
            Some(name)
        })
        .collect();

    let mut results = Vec::new();

    for test_name in &test_names {
        harn_vm::reset_thread_local_state();

        let start = Instant::now();

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
        let previous_cwd = if let Some(cwd) = execution_cwd {
            let previous = std::env::current_dir().ok();
            std::env::set_current_dir(cwd).map_err(|error| {
                format!(
                    "Failed to set current directory to {}: {error}",
                    cwd.display()
                )
            })?;
            previous
        } else {
            None
        };
        let result = tokio::time::timeout(
            timeout,
            local.run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let source_parent = path.parent().unwrap_or(std::path::Path::new("."));
                let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
                let store_base = project_root.as_deref().unwrap_or(source_parent);
                let execution_cwd = test_execution_cwd();
                let source_dir = source_parent.to_string_lossy().into_owned();
                harn_vm::register_store_builtins(&mut vm, store_base);
                harn_vm::register_metadata_builtins(&mut vm, store_base);
                let pipeline_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("test");
                harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
                vm.set_source_info(&path_str, &source);
                harn_vm::stdlib::process::set_thread_execution_context(Some(
                    harn_vm::orchestration::RunExecutionRecord {
                        cwd: Some(execution_cwd.to_string_lossy().into_owned()),
                        source_dir: Some(source_dir),
                        env: std::collections::BTreeMap::new(),
                        adapter: None,
                        repo_path: None,
                        worktree_path: None,
                        branch: None,
                        base_ref: None,
                        cleanup: None,
                    },
                ));
                if let Some(ref root) = project_root {
                    vm.set_project_root(root);
                }
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
        if let Some(previous) = previous_cwd {
            let _ = std::env::set_current_dir(previous);
        }

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
    // Default LLM provider to "mock" in test mode unless caller overrides.
    let prev_provider = std::env::var("HARN_LLM_PROVIDER").ok();
    if prev_provider.is_none() {
        std::env::set_var("HARN_LLM_PROVIDER", "mock");
    }

    let start = Instant::now();
    let mut all_results = Vec::new();

    let canonical_target = canonicalize_existing_path(path);
    let files = if canonical_target.is_dir() {
        discover_test_files(&canonical_target)
    } else {
        vec![canonical_target]
    };

    if parallel {
        let local = tokio::task::LocalSet::new();
        let results = local
            .run_until(async {
                let mut handles = Vec::new();
                for file in files {
                    let filter = filter.map(|s| s.to_string());
                    handles.push(tokio::task::spawn_local(async move {
                        run_test_file(&file, filter.as_deref(), timeout_ms, None).await
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
            let execution_cwd = file
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty());
            match run_test_file(file, filter, timeout_ms, execution_cwd).await {
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
                        files.push(canonicalize_existing_path(&path));
                    }
                }
            }
        }
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::{discover_test_files, run_tests};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempTestDir {
        path: PathBuf,
    }

    impl TempTestDir {
        fn new() -> Self {
            let unique = format!(
                "harn-test-runner-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, contents).unwrap();
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn discover_test_files_returns_canonical_absolute_paths() {
        let temp = TempTestDir::new();
        temp.write("suite/test_alpha.harn", "pipeline test_alpha(task) {}");
        temp.write("suite/nested/test_beta.harn", "pipeline test_beta(task) {}");
        temp.write("suite/ignore.harn", "pipeline build(task) {}");

        // Pass an absolute path rather than mutating process-wide cwd — the
        // other test_runner test asserts cwd preservation, and mutating it
        // from two tests concurrently causes cross-test flakiness.
        let files = discover_test_files(&temp.path().join("suite"));

        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|path| path.is_absolute()));
        assert!(files
            .iter()
            .any(|path| path.ends_with("suite/test_alpha.harn")));
        assert!(files
            .iter()
            .any(|path| path.ends_with("suite/nested/test_beta.harn")));
    }

    #[tokio::test]
    async fn run_tests_uses_file_parent_as_execution_cwd_and_restores_shell_cwd() {
        let temp = TempTestDir::new();
        temp.write(
            "suite/test_cwd.harn",
            r#"
pipeline test_current_dir(task) {
  assert_eq(cwd(), source_dir())
}
"#,
        );

        let original_cwd = std::env::current_dir().unwrap();
        let summary = run_tests(&temp.path().join("suite"), None, 1_000, false).await;
        let restored_cwd = std::env::current_dir().unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.passed, 1);
        assert_eq!(
            fs::canonicalize(restored_cwd).unwrap(),
            fs::canonicalize(original_cwd).unwrap()
        );
    }
}
