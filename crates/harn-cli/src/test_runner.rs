use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use harn_lexer::Lexer;
use harn_parser::{Node, Parser, SNode};

use crate::env_guard::ScopedEnvVar;

#[derive(Clone, Debug)]
pub struct TestResult {
    pub name: String,
    pub file: String,
    pub passed: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug)]
pub struct TestSummary {
    pub results: Vec<TestResult>,
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    pub duration_ms: u64,
}

#[derive(Clone, Debug)]
pub enum TestRunEvent {
    SuiteDiscovered {
        total_tests: usize,
        total_files: usize,
        parallel: bool,
    },
    LargeSequentialSuite {
        total_tests: usize,
        total_files: usize,
    },
    FileStarted {
        file: String,
        file_index: usize,
        total_files: usize,
        test_count: usize,
    },
    TestStarted {
        name: String,
        file: String,
        test_index: usize,
        total_tests_in_file: usize,
    },
    TestFinished(TestResult),
}

pub type TestRunProgress = Arc<dyn Fn(TestRunEvent) + Send + Sync>;

const LARGE_SEQUENTIAL_TEST_THRESHOLD: usize = 50;
const LARGE_SEQUENTIAL_FILE_THRESHOLD: usize = 10;

fn canonicalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn test_execution_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

struct ParsedTestFile {
    source: String,
    program: Vec<SNode>,
    test_names: Vec<String>,
}

#[derive(Clone)]
struct TestFilePlan {
    file: PathBuf,
    test_count: usize,
}

fn parse_test_file(path: &Path, filter: Option<&str>) -> Result<ParsedTestFile, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize().map_err(|e| format!("{e}"))?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| format!("{e}"))?;
    let test_names = discover_test_names(&program, filter);

    Ok(ParsedTestFile {
        source,
        program,
        test_names,
    })
}

fn discover_test_names(program: &[SNode], filter: Option<&str>) -> Vec<String> {
    program
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
        .collect()
}

fn emit_progress(progress: &Option<TestRunProgress>, event: TestRunEvent) {
    if let Some(callback) = progress {
        callback(event);
    }
}

fn push_result(
    results: &mut Vec<TestResult>,
    result: TestResult,
    progress: &Option<TestRunProgress>,
) {
    emit_progress(progress, TestRunEvent::TestFinished(result.clone()));
    results.push(result);
}

fn should_warn_large_sequential_suite(total_tests: usize, total_files: usize) -> bool {
    total_tests >= LARGE_SEQUENTIAL_TEST_THRESHOLD || total_files >= LARGE_SEQUENTIAL_FILE_THRESHOLD
}

async fn run_test_file_with_progress(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    execution_cwd: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
    progress: Option<TestRunProgress>,
) -> Result<Vec<TestResult>, String> {
    let ParsedTestFile {
        source,
        program,
        test_names,
    } = parse_test_file(path, filter)?;

    let mut results = Vec::new();

    for (test_index, test_name) in test_names.iter().enumerate() {
        emit_progress(
            &progress,
            TestRunEvent::TestStarted {
                name: test_name.clone(),
                file: path.display().to_string(),
                test_index: test_index + 1,
                total_tests_in_file: test_names.len(),
            },
        );
        harn_vm::reset_thread_local_state();

        let start = Instant::now();

        let chunk = match harn_vm::Compiler::new().compile_named(&program, test_name) {
            Ok(c) => c,
            Err(e) => {
                push_result(
                    &mut results,
                    TestResult {
                        name: test_name.clone(),
                        file: path.display().to_string(),
                        passed: false,
                        error: Some(format!("Compile error: {e}")),
                        duration_ms: 0,
                    },
                    &progress,
                );
                continue;
            }
        };

        let local = tokio::task::LocalSet::new();
        let path_str = path.display().to_string();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let execution_cwd = execution_cwd
            .map(Path::to_path_buf)
            .unwrap_or_else(test_execution_cwd);
        let result = tokio::time::timeout(
            timeout,
            local.run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                crate::install_default_hostlib(&mut vm);
                let source_parent = path.parent().unwrap_or(std::path::Path::new("."));
                let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
                let store_base = project_root.as_deref().unwrap_or(source_parent);
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
                let loaded =
                    crate::skill_loader::load_skills(&crate::skill_loader::SkillLoaderInputs {
                        cli_dirs: cli_skill_dirs.to_vec(),
                        source_path: Some(path.to_path_buf()),
                    });
                crate::skill_loader::emit_loader_warnings(&loaded.loader_warnings);
                crate::skill_loader::install_skills_global(&mut vm, &loaded);
                let extensions = crate::package::load_runtime_extensions(path);
                crate::package::install_runtime_extensions(&extensions);
                crate::package::install_manifest_triggers(&mut vm, &extensions)
                    .await
                    .map_err(|error| format!("failed to install manifest triggers: {error}"))?;
                crate::package::install_manifest_hooks(&mut vm, &extensions)
                    .await
                    .map_err(|error| format!("failed to install manifest hooks: {error}"))?;
                vm.set_harness(harn_vm::Harness::real());
                let result = match vm.execute(&chunk).await {
                    Ok(val) => Ok(val),
                    Err(e) => {
                        let formatted = vm.format_runtime_error(&e);
                        Err(formatted)
                    }
                };
                harn_vm::egress::reset_egress_policy_for_host();
                result
            }),
        )
        .await;

        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(_)) => {
                push_result(
                    &mut results,
                    TestResult {
                        name: test_name.clone(),
                        file: path.display().to_string(),
                        passed: true,
                        error: None,
                        duration_ms: duration,
                    },
                    &progress,
                );
            }
            Ok(Err(e)) => {
                push_result(
                    &mut results,
                    TestResult {
                        name: test_name.clone(),
                        file: path.display().to_string(),
                        passed: false,
                        error: Some(e),
                        duration_ms: duration,
                    },
                    &progress,
                );
            }
            Err(_) => {
                push_result(
                    &mut results,
                    TestResult {
                        name: test_name.clone(),
                        file: path.display().to_string(),
                        passed: false,
                        error: Some(format!("timed out after {timeout_ms}ms")),
                        duration_ms: timeout_ms,
                    },
                    &progress,
                );
            }
        }
    }

    Ok(results)
}

/// Run all test_* pipelines in a single source file using the VM.
pub async fn run_test_file(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    execution_cwd: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
) -> Result<Vec<TestResult>, String> {
    run_test_file_with_progress(
        path,
        filter,
        timeout_ms,
        execution_cwd,
        cli_skill_dirs,
        None,
    )
    .await
}

/// Discover and run tests in a file or directory.
pub async fn run_tests(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    parallel: bool,
    cli_skill_dirs: &[PathBuf],
) -> TestSummary {
    run_tests_with_progress(path, filter, timeout_ms, parallel, cli_skill_dirs, None).await
}

pub async fn run_tests_with_progress(
    path: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    parallel: bool,
    cli_skill_dirs: &[PathBuf],
    progress: Option<TestRunProgress>,
) -> TestSummary {
    // Default LLM provider to "mock" in test mode unless caller overrides.
    let _default_llm_provider = ScopedEnvVar::set_if_unset("HARN_LLM_PROVIDER", "mock");
    let _disable_llm_calls = ScopedEnvVar::set(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1");

    let start = Instant::now();
    let mut all_results = Vec::new();

    let canonical_target = canonicalize_existing_path(path);
    let files = if canonical_target.is_dir() {
        discover_test_files(&canonical_target)
    } else {
        vec![canonical_target]
    };
    let file_plans = files
        .into_iter()
        .map(|file| {
            let test_count = parse_test_file(&file, filter)
                .map(|parsed| parsed.test_names.len())
                .unwrap_or(0);
            TestFilePlan { file, test_count }
        })
        .collect::<Vec<_>>();
    let total_tests = file_plans.iter().map(|plan| plan.test_count).sum();
    let total_files = file_plans.iter().filter(|plan| plan.test_count > 0).count();
    emit_progress(
        &progress,
        TestRunEvent::SuiteDiscovered {
            total_tests,
            total_files,
            parallel,
        },
    );
    if !parallel && should_warn_large_sequential_suite(total_tests, total_files) {
        emit_progress(
            &progress,
            TestRunEvent::LargeSequentialSuite {
                total_tests,
                total_files,
            },
        );
    }

    if parallel {
        let mut handles = Vec::new();
        let mut progress_file_index = 0;
        for plan in file_plans {
            let filter = filter.map(|s| s.to_string());
            let cli_skill_dirs = cli_skill_dirs.to_vec();
            let progress = progress.clone();
            if plan.test_count > 0 {
                progress_file_index += 1;
                emit_progress(
                    &progress,
                    TestRunEvent::FileStarted {
                        file: plan.file.display().to_string(),
                        file_index: progress_file_index,
                        total_files,
                        test_count: plan.test_count,
                    },
                );
            }
            handles.push(tokio::task::spawn_blocking(move || {
                let execution_cwd = plan
                    .file
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map(Path::to_path_buf);
                run_test_file_on_isolated_thread(
                    &plan.file,
                    filter.as_deref(),
                    timeout_ms,
                    execution_cwd.as_deref(),
                    &cli_skill_dirs,
                    None,
                )
            }));
        }
        for handle in handles {
            match handle.await {
                Ok(Ok(r)) => {
                    for result in &r {
                        emit_progress(&progress, TestRunEvent::TestFinished(result.clone()));
                    }
                    all_results.extend(r);
                }
                Ok(Err(e)) => {
                    let result = TestResult {
                        name: "<file error>".to_string(),
                        file: String::new(),
                        passed: false,
                        error: Some(e),
                        duration_ms: 0,
                    };
                    push_result(&mut all_results, result, &progress);
                }
                Err(e) => {
                    let result = TestResult {
                        name: "<join error>".to_string(),
                        file: String::new(),
                        passed: false,
                        error: Some(format!("{e}")),
                        duration_ms: 0,
                    };
                    push_result(&mut all_results, result, &progress);
                }
            }
        }
    } else {
        let mut progress_file_index = 0;
        for plan in &file_plans {
            if plan.test_count > 0 {
                progress_file_index += 1;
                emit_progress(
                    &progress,
                    TestRunEvent::FileStarted {
                        file: plan.file.display().to_string(),
                        file_index: progress_file_index,
                        total_files,
                        test_count: plan.test_count,
                    },
                );
            }
            let execution_cwd = plan
                .file
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty());
            match run_test_file_with_progress(
                &plan.file,
                filter,
                timeout_ms,
                execution_cwd,
                cli_skill_dirs,
                progress.clone(),
            )
            .await
            {
                Ok(results) => all_results.extend(results),
                Err(e) => {
                    let result = TestResult {
                        name: "<file error>".to_string(),
                        file: plan.file.display().to_string(),
                        passed: false,
                        error: Some(e),
                        duration_ms: 0,
                    };
                    push_result(&mut all_results, result, &progress);
                }
            }
        }
    }

    let total = all_results.len();
    let passed = all_results.iter().filter(|r| r.passed).count();
    let failed = total - passed;

    TestSummary {
        results: all_results,
        passed,
        failed,
        total,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

fn run_test_file_on_isolated_thread(
    file: &Path,
    filter: Option<&str>,
    timeout_ms: u64,
    execution_cwd: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
    progress: Option<TestRunProgress>,
) -> Result<Vec<TestResult>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start test runtime: {error}"))?;
    runtime.block_on(run_test_file_with_progress(
        file,
        filter,
        timeout_ms,
        execution_cwd,
        cli_skill_dirs,
        progress,
    ))
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
                    if content.contains("test_") || content.contains("@test") {
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
    use super::{
        discover_test_files, run_tests, run_tests_with_progress,
        should_warn_large_sequential_suite, TestRunEvent, TestRunProgress,
    };
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    struct TempTestDir {
        inner: tempfile::TempDir,
    }

    impl TempTestDir {
        fn new() -> Self {
            Self {
                inner: tempfile::tempdir().unwrap(),
            }
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, contents).unwrap();
        }

        fn path(&self) -> &Path {
            self.inner.path()
        }
    }

    #[test]
    fn discover_test_files_returns_canonical_absolute_paths() {
        let temp = TempTestDir::new();
        temp.write("suite/test_alpha.harn", "pipeline test_alpha(task) {}");
        temp.write("suite/nested/test_beta.harn", "pipeline test_beta(task) {}");
        temp.write("suite/annotated.harn", "@test\npipeline annotated(task) {}");
        temp.write("suite/ignore.harn", "pipeline build(task) {}");

        // Pass an absolute path rather than mutating process-wide cwd — the
        // other test_runner test asserts cwd preservation, and mutating it
        // from two tests concurrently causes cross-test flakiness.
        let files = discover_test_files(&temp.path().join("suite"));

        assert_eq!(files.len(), 3);
        assert!(files.iter().all(|path| path.is_absolute()));
        assert!(files
            .iter()
            .any(|path| path.ends_with("suite/test_alpha.harn")));
        assert!(files
            .iter()
            .any(|path| path.ends_with("suite/nested/test_beta.harn")));
        assert!(files
            .iter()
            .any(|path| path.ends_with("suite/annotated.harn")));
    }

    #[test]
    fn large_sequential_suite_warning_threshold_is_conservative() {
        assert!(!should_warn_large_sequential_suite(49, 9));
        assert!(should_warn_large_sequential_suite(50, 1));
        assert!(should_warn_large_sequential_suite(1, 10));
    }

    #[tokio::test]
    async fn run_tests_emits_progress_events() {
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "suite/test_progress.harn",
            r#"
pipeline test_alpha(task) {
  assert_eq(1, 1)
}

pipeline test_beta(task) {
  assert_eq(2, 2)
}
"#,
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let progress: TestRunProgress = Arc::new(move |event| {
            captured.lock().unwrap().push(event);
        });

        let summary = run_tests_with_progress(
            &temp.path().join("suite"),
            None,
            1_000,
            false,
            &[],
            Some(progress),
        )
        .await;
        let events = events.lock().unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.passed, 2);
        assert!(matches!(
            events.first(),
            Some(TestRunEvent::SuiteDiscovered {
                total_tests: 2,
                total_files: 1,
                parallel: false
            })
        ));
        let file_started = events
            .iter()
            .position(|event| matches!(event, TestRunEvent::FileStarted { .. }))
            .expect("file start event");
        let test_started = events
            .iter()
            .position(|event| matches!(event, TestRunEvent::TestStarted { .. }))
            .expect("test start event");
        let test_finished = events
            .iter()
            .position(|event| matches!(event, TestRunEvent::TestFinished(_)))
            .expect("test finished event");
        assert!(file_started < test_started);
        assert!(test_started < test_finished);
    }

    #[tokio::test]
    async fn run_tests_uses_file_parent_as_execution_cwd_and_restores_shell_cwd() {
        let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
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
        let summary = run_tests(&temp.path().join("suite"), None, 1_000, false, &[]).await;
        let restored_cwd = std::env::current_dir().unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.passed, 1);
        assert_eq!(
            fs::canonicalize(restored_cwd).unwrap(),
            fs::canonicalize(original_cwd).unwrap()
        );
    }

    #[tokio::test]
    async fn parallel_run_tests_uses_each_file_parent_as_execution_cwd() {
        let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "suite/a/test_one.harn",
            r#"
pipeline test_one(task) {
  assert_eq(cwd(), source_dir())
}
"#,
        );
        temp.write(
            "suite/b/test_two.harn",
            r#"
pipeline test_two(task) {
  assert_eq(cwd(), source_dir())
}
"#,
        );

        let summary = run_tests(&temp.path().join("suite"), None, 1_000, true, &[]).await;
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.passed, 2);
    }

    #[tokio::test]
    async fn run_tests_loads_cli_skill_dirs() {
        let _env_guard = crate::tests::common::env_lock::lock_env().lock().await;
        let temp = TempTestDir::new();
        temp.write(
            "skills/review/SKILL.md",
            r#"---
name: review
short: Review PRs
description: Review pull requests
---

Review instructions.
"#,
        );
        temp.write(
            "suite/test_skills.harn",
            r#"
pipeline test_cli_skills(task) {
  assert_eq(skill_count(skills), 1)
  let found = skill_find(skills, "review")
  assert_eq(found.name, "review")
}
"#,
        );

        let summary = run_tests(
            &temp.path().join("suite"),
            None,
            1_000,
            false,
            &[temp.path().join("skills")],
        )
        .await;

        assert_eq!(summary.failed, 0, "{:?}", summary.results[0].error);
        assert_eq!(summary.passed, 1);
    }
}
