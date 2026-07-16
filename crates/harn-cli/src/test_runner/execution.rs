//! Hermetic per-case VM setup, execution, timeout, and teardown.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::{PhaseTimings, TestCase, TestPhase, TestResult, TestTimeout};

/// Drain `harn-hostlib`'s process-global fs-snapshot sessions between test
/// cases. A reused test worker would otherwise accumulate one bundle per case.
#[cfg(feature = "hostlib")]
fn reset_hostlib_state() {
    harn_hostlib::fs_snapshot::reset_all_sessions();
}

#[cfg(not(feature = "hostlib"))]
fn reset_hostlib_state() {}

fn install_user_test_event_log_if_unset() {
    if std::env::var_os(harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV).is_some() {
        return;
    }
    harn_vm::event_log::install_memory_for_current_thread(
        harn_vm::RuntimeLimits::DEFAULT.default_event_log_queue_depth,
    );
}

fn register_manifest_host_operations(extensions: &crate::package::RuntimeExtensions) {
    let (Some(manifest), Some(manifest_dir)) = (
        extensions.root_manifest.as_ref(),
        extensions.root_manifest_dir.as_deref(),
    ) else {
        return;
    };
    let check = crate::package::absolutize_check_config_paths(manifest.check.clone(), manifest_dir);
    for (capability, operations) in crate::commands::check::load_host_capabilities(&check) {
        for operation in operations {
            harn_vm::stdlib::host::register_scoped_mockable_host_operation(
                &capability,
                &operation,
                "Host operation declared by the project manifest.",
            );
        }
    }
}

#[derive(Debug)]
enum CaseOutcome {
    Passed,
    RuntimeError(String),
    ExecutionTimedOut,
}

pub(super) async fn execute_case(
    case: &TestCase,
    execution_cwd: &Path,
    timeout_ms: u64,
    cli_skill_dirs: &[PathBuf],
    prepared_module_cache: &harn_vm::PreparedModuleCache,
    stdio_available: bool,
    #[cfg(test)] setup_delay_ms: u64,
) -> TestResult {
    harn_vm::reset_thread_local_state();
    let _stdio_guard = (!stdio_available).then(harn_vm::reserve_stdio_for_current_thread);
    reset_hostlib_state();

    let mut phases = PhaseTimings::default();
    let total_start = Instant::now();

    let compile_start = Instant::now();
    let compiler = harn_vm::Compiler::new();
    let chunk = match if case.bindings.is_empty() {
        compiler.compile_named(&case.program, &case.pipeline_name)
    } else {
        compiler.compile_named_with_param_globals(&case.program, &case.pipeline_name)
    } {
        Ok(c) => c,
        Err(e) => {
            phases.compile_ms = compile_start.elapsed().as_millis() as u64;
            return TestResult {
                name: case.name.clone(),
                file: case.file.display().to_string(),
                passed: false,
                error: Some(format!("Compile error: {e}")),
                timeout: None,
                duration_ms: total_start.elapsed().as_millis() as u64,
                phases: Some(phases),
            };
        }
    };
    phases.compile_ms = compile_start.elapsed().as_millis() as u64;

    let local = tokio::task::LocalSet::new();
    let file_display = case.file.display().to_string();
    let setup_start = Instant::now();
    let mut vm = harn_vm::Vm::new();
    let module_phase_recorder = vm.enable_module_phase_timing();
    let result = local
        .run_until(async {
            vm.set_prepared_module_cache(prepared_module_cache.clone());
            harn_vm::register_vm_stdlib(&mut vm);
            crate::install_default_hostlib(&mut vm);
            let source_parent = case.file.parent().unwrap_or(Path::new("."));
            let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
            // Persistent runtime state is production behavior, but sharing it
            // between user tests leaks store overrides, metadata, and
            // checkpoints across otherwise-fresh VMs. A per-case root keeps
            // both sequential and parallel test execution hermetic.
            let test_state = tempfile::Builder::new()
                .prefix("harn-user-test-state-")
                .tempdir()
                .map_err(|error| format!("failed to create test state directory: {error}"))?;
            let store_base = test_state.path();
            let source_dir = source_parent.to_string_lossy().into_owned();
            install_user_test_event_log_if_unset();
            harn_vm::register_store_builtins(&mut vm, store_base);
            harn_vm::register_metadata_builtins(&mut vm, store_base);
            let pipeline_name = case
                .file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test");
            harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
            vm.set_source_info(&file_display, &case.source);
            harn_vm::stdlib::process::set_thread_execution_context(Some(
                harn_vm::orchestration::RunExecutionRecord {
                    cwd: Some(execution_cwd.to_string_lossy().into_owned()),
                    project_root: project_root
                        .as_ref()
                        .map(|root| root.to_string_lossy().into_owned()),
                    source_dir: Some(source_dir),
                    env: BTreeMap::new(),
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
            if let Some(parent) = case.file.parent() {
                if !parent.as_os_str().is_empty() {
                    vm.set_source_dir(parent);
                }
            }
            let loaded =
                crate::skill_loader::load_skills(&crate::skill_loader::SkillLoaderInputs {
                    cli_dirs: cli_skill_dirs.to_vec(),
                    source_path: Some(case.file.clone()),
                });
            crate::skill_loader::emit_loader_warnings(&loaded.loader_warnings);
            crate::skill_loader::install_skills_global(&mut vm, &loaded);
            let extensions = crate::package::try_load_runtime_extensions(&case.file)
                .map_err(|error| format!("failed to load runtime extensions: {error}"))?;
            register_manifest_host_operations(&extensions);
            crate::package::install_runtime_extensions(&extensions);
            crate::package::install_manifest_triggers_with_mode(&mut vm, &extensions, true)
                .await
                .map_err(|error| format!("failed to install manifest triggers: {error}"))?;
            // Install manifest hooks lazily: a pure-logic unit test that
            // never fires a hook must not pay the ~1s cost of instantiating
            // the handler module's whole import graph during setup. Lazy
            // hooks resolve on first fire against the firing VM (a cache hit
            // when the test already imported the graph), preserving per-test
            // module-state isolation.
            crate::package::install_manifest_hooks_with_mode(&mut vm, &extensions, true)
                .await
                .map_err(|error| format!("failed to install manifest hooks: {error}"))?;
            vm.set_harness(harn_vm::Harness::real());
            for (name, value) in &case.bindings {
                vm.set_global(name, value.clone());
            }
            #[cfg(test)]
            wait_for_test_setup_delay(setup_delay_ms).await;
            let setup_ms = setup_start.elapsed().as_millis() as u64;
            let exec_start = Instant::now();
            let outcome = match vm
                .execute_with_timeout(&chunk, std::time::Duration::from_millis(timeout_ms))
                .await
            {
                Ok(_) => CaseOutcome::Passed,
                Err(harn_vm::VmError::ExecutionDeadlineExceeded) => CaseOutcome::ExecutionTimedOut,
                Err(error) => CaseOutcome::RuntimeError(vm.format_runtime_error(&error)),
            };
            let execute_ms = exec_start.elapsed().as_millis() as u64;
            let execute_ms = if matches!(outcome, CaseOutcome::ExecutionTimedOut) {
                execute_ms.max(timeout_ms)
            } else {
                execute_ms
            };
            harn_vm::egress::reset_egress_policy_for_host();
            Ok::<_, String>((outcome, setup_ms, execute_ms))
        })
        .await;
    let failed_setup_ms = result
        .as_ref()
        .err()
        .map(|_| setup_start.elapsed().as_millis() as u64);
    let teardown_start = Instant::now();
    // Cancel and drain detached VM/LocalSet work inside the teardown phase,
    // then snapshot spans closed by task cancellation for this case.
    drop(local);
    drop(vm);
    phases.modules = module_phase_recorder.snapshot();
    // Clear thread-locals so the next case scheduled onto this worker
    // sees a clean slate. Wall clock for this work lands in the
    // teardown bucket so the phase breakdown sums to wall time.
    harn_vm::reset_thread_local_state();
    reset_hostlib_state();
    phases.teardown_ms = teardown_start.elapsed().as_millis() as u64;

    let elapsed_ms = total_start.elapsed().as_millis() as u64;
    let (passed, error, timeout, duration_ms) = match result {
        Ok((outcome, setup_ms, execute_ms)) => {
            phases.setup_ms = setup_ms;
            phases.execute_ms = execute_ms;
            match outcome {
                CaseOutcome::Passed => (true, None, None, elapsed_ms),
                CaseOutcome::RuntimeError(message) => (false, Some(message), None, elapsed_ms),
                CaseOutcome::ExecutionTimedOut => (
                    false,
                    Some(format!("execute phase timed out after {timeout_ms}ms")),
                    Some(TestTimeout {
                        phase: TestPhase::Execute,
                        limit_ms: timeout_ms,
                    }),
                    elapsed_ms,
                ),
            }
        }
        Err(setup_error) => {
            phases.setup_ms = failed_setup_ms.unwrap_or_default();
            (false, Some(setup_error), None, elapsed_ms)
        }
    };

    TestResult {
        name: case.name.clone(),
        file: file_display,
        passed,
        error,
        timeout,
        duration_ms,
        phases: Some(phases),
    }
}

#[cfg(test)]
async fn wait_for_test_setup_delay(delay_ms: u64) {
    if delay_ms == 0 {
        return;
    }
    // This must yield: the regression proves an outer timeout would expire
    // during setup, rather than merely measuring setup wall time afterward.
    tokio::task::spawn_blocking(move || {
        let state = std::sync::Mutex::new(());
        let guard = state.lock().unwrap();
        let condition = std::sync::Condvar::new();
        let _ = condition
            .wait_timeout_while(guard, std::time::Duration::from_millis(delay_ms), |_| true)
            .unwrap();
    })
    .await
    .unwrap();
}
