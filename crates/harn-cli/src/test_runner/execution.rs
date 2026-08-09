//! Hermetic per-case VM setup, execution, timeout, and teardown.

use std::collections::BTreeMap;
use std::path::Path;
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
    for (capability, operations) in
        crate::commands::check::load_host_capabilities(&check).into_operations()
    {
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
    Passed(harn_vm::VmValue),
    RuntimeError(String),
    ExecutionTimedOut,
}

struct InvocationExecution {
    result: TestResult,
    value: Option<harn_vm::VmValue>,
}

pub(super) async fn execute_case(
    case: &TestCase,
    execution_cwd: &Path,
    timeout_ms: u64,
    loaded_skills: &crate::skill_loader::LoadedSkills,
    prepared_module_cache: &harn_vm::PreparedModuleCache,
    stdio_available: bool,
    operator_approval_grant: Option<&harn_vm::orchestration::OperatorApprovalGrant>,
) -> TestResult {
    let total_start = Instant::now();
    let compile_start = Instant::now();
    let owned_entry;
    let (entry, compile_ms) = if let Some(entry) = case.compiled_entry.as_deref() {
        (entry, 0)
    } else {
        let imported_enums = case.imported_enum_candidates.iter().cloned();
        let compiler = if case.trusted_host_dispatch {
            harn_vm::Compiler::new_trusted_host_dispatch()
                .with_imported_enum_candidates(imported_enums)
        } else {
            crate::compiler_with_imported_enum_candidates(imported_enums)
        };
        let case_fixture = case
            .fixture
            .as_ref()
            .filter(|fixture| fixture.scope == super::FixtureScope::Case)
            .map(|fixture| fixture.name.as_str());
        owned_entry = match compiler.compile_named_pipeline_entry(
            &case.program,
            &case.pipeline_name,
            case_fixture,
        ) {
            Ok(entry) => entry,
            Err(error) => {
                return compile_failure(case, &case.name, error, compile_start, total_start);
            }
        };
        (&owned_entry, compile_start.elapsed().as_millis() as u64)
    };
    let mut args = case.args.clone();
    if let Some(value) = &case.file_fixture_value {
        args.insert(0, value.instantiate());
    }
    execute_compiled(
        case,
        &case.name,
        entry,
        &args,
        execution_cwd,
        timeout_ms,
        loaded_skills,
        prepared_module_cache,
        stdio_available,
        operator_approval_grant,
        compile_ms,
        total_start,
    )
    .await
    .result
}

pub(super) async fn execute_file_fixture(
    case: &TestCase,
    fixture: &super::TestFixture,
    execution_cwd: &Path,
    timeout_ms: u64,
    loaded_skills: &crate::skill_loader::LoadedSkills,
    prepared_module_cache: &harn_vm::PreparedModuleCache,
    stdio_available: bool,
    operator_approval_grant: Option<&harn_vm::orchestration::OperatorApprovalGrant>,
) -> Result<harn_vm::IsolateValue, TestResult> {
    let total_start = Instant::now();
    let compile_start = Instant::now();
    let owned_entry;
    let entry = if let Some(entry) = case.compiled_file_fixture_entry.as_ref() {
        match entry {
            Ok(entry) => entry.as_ref(),
            Err(error) => {
                return Err(compile_failure(
                    case,
                    &format!("<fixture {}>", fixture.name),
                    error.clone(),
                    compile_start,
                    total_start,
                ));
            }
        }
    } else {
        let imported_enums = case.imported_enum_candidates.iter().cloned();
        let compiler = if case.trusted_host_dispatch {
            harn_vm::Compiler::new_trusted_host_dispatch()
                .with_imported_enum_candidates(imported_enums)
        } else {
            crate::compiler_with_imported_enum_candidates(imported_enums)
        };
        owned_entry = match compiler.compile_named_function_entry(&case.program, &fixture.name) {
            Ok(entry) => entry,
            Err(error) => {
                return Err(compile_failure(
                    case,
                    &format!("<fixture {}>", fixture.name),
                    error,
                    compile_start,
                    total_start,
                ));
            }
        };
        &owned_entry
    };
    let compile_ms = if case.compiled_file_fixture_entry.is_none() {
        compile_start.elapsed().as_millis() as u64
    } else {
        0
    };
    let execution = execute_compiled(
        case,
        &format!("<fixture {}>", fixture.name),
        entry,
        &[],
        execution_cwd,
        timeout_ms,
        loaded_skills,
        prepared_module_cache,
        stdio_available,
        operator_approval_grant,
        compile_ms,
        total_start,
    )
    .await;
    match execution.value {
        Some(value) => value.try_into_isolate_value().map_err(|error| {
            let mut result = execution.result;
            result.passed = false;
            result.error = Some(format!(
                "file fixture `{}` returned a value that cannot cross test isolates: {error}",
                fixture.name
            ));
            result
        }),
        None => Err(execution.result),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_compiled(
    case: &TestCase,
    result_name: &str,
    entry: &harn_vm::CompiledCallableEntry,
    args: &[harn_vm::VmValue],
    execution_cwd: &Path,
    timeout_ms: u64,
    loaded_skills: &crate::skill_loader::LoadedSkills,
    prepared_module_cache: &harn_vm::PreparedModuleCache,
    stdio_available: bool,
    operator_approval_grant: Option<&harn_vm::orchestration::OperatorApprovalGrant>,
    compile_ms: u64,
    total_start: Instant,
) -> InvocationExecution {
    let _egress_scope = harn_vm::egress::scope_egress_policy_for_current_thread();
    harn_vm::reset_thread_local_state();
    let _operator_approval_guard = operator_approval_grant
        .cloned()
        .map(harn_vm::orchestration::install_operator_approval_grant);
    let _stdio_guard = (!stdio_available).then(harn_vm::reserve_stdio_for_current_thread);
    reset_hostlib_state();

    let mut phases = PhaseTimings {
        compile_ms,
        ..PhaseTimings::default()
    };
    let local = tokio::task::LocalSet::new();
    let file_display = case.file.display().to_string();
    let setup_start = Instant::now();
    let mut vm = harn_vm::Vm::new();
    if case.trusted_host_dispatch {
        vm.enable_trusted_host_dispatch()
            .expect("fresh test VM accepts explicit trusted host-dispatch authority");
    }
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
            let state_root = test_state.path().join(".harn");
            #[cfg(feature = "hostlib")]
            let _conditional_replace_lock_root =
                harn_hostlib::fs::scope_conditional_replace_lock_root(
                    state_root.join("fs-cas-locks"),
                );
            let source_dir = source_parent.to_string_lossy().into_owned();
            install_user_test_event_log_if_unset();
            let pipeline_name = case
                .file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test");
            harn_vm::register_persistent_state_builtins_at_root(
                &mut vm,
                test_state.path(),
                harn_vm::PersistentStateRoot::new(&state_root),
                pipeline_name,
            );
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
                    environment_policy: Default::default(),
                    grants: Vec::new(),
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
            crate::skill_loader::install_skills_global(&mut vm, loaded_skills);
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
            let setup_ms = setup_start.elapsed().as_millis() as u64;
            let exec_start = Instant::now();
            let outcome = match vm
                .execute_callable_entry_with_timeout(
                    entry,
                    args,
                    std::time::Duration::from_millis(timeout_ms),
                )
                .await
            {
                Ok(value) => CaseOutcome::Passed(value),
                Err(harn_vm::VmError::ExecutionDeadlineExceeded) => CaseOutcome::ExecutionTimedOut,
                Err(error) => CaseOutcome::RuntimeError(vm.format_runtime_error(&error)),
            };
            let execute_ms = exec_start.elapsed().as_millis() as u64;
            let execute_ms = if matches!(&outcome, CaseOutcome::ExecutionTimedOut) {
                execute_ms.max(timeout_ms)
            } else {
                execute_ms
            };
            harn_vm::egress::reset_egress_policy_for_host();
            Ok::<_, String>((outcome, setup_ms, execute_ms))
        })
        .await;
    // Read before `drop(vm)` below. Populated regardless of outcome: a
    // timed-out or setup-failed case can still have useful `log()` calls
    // that ran before the deadline/failure, and withholding them here would
    // silently discard exactly the probes an author added to find where
    // execution stalled or diverged.
    let captured_output = {
        let raw = vm.take_output();
        (!raw.trim().is_empty()).then_some(raw)
    };
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
    let (passed, error, timeout, duration_ms, value) = match result {
        Ok((outcome, setup_ms, execute_ms)) => {
            phases.setup_ms = setup_ms;
            phases.execute_ms = execute_ms;
            match outcome {
                CaseOutcome::Passed(value) => (true, None, None, elapsed_ms, Some(value)),
                CaseOutcome::RuntimeError(message) => {
                    (false, Some(message), None, elapsed_ms, None)
                }
                CaseOutcome::ExecutionTimedOut => (
                    false,
                    Some(format!("execute phase timed out after {timeout_ms}ms")),
                    Some(TestTimeout {
                        phase: TestPhase::Execute,
                        limit_ms: timeout_ms,
                    }),
                    elapsed_ms,
                    None,
                ),
            }
        }
        Err(setup_error) => {
            phases.setup_ms = failed_setup_ms.unwrap_or_default();
            (false, Some(setup_error), None, elapsed_ms, None)
        }
    };

    InvocationExecution {
        result: TestResult {
            name: result_name.to_string(),
            file: file_display,
            passed,
            error,
            captured_output,
            timeout,
            duration_ms,
            phases: Some(phases),
        },
        value,
    }
}

fn compile_failure(
    case: &TestCase,
    result_name: &str,
    error: harn_vm::CompileError,
    compile_start: Instant,
    total_start: Instant,
) -> TestResult {
    TestResult {
        name: result_name.to_string(),
        file: case.file.display().to_string(),
        passed: false,
        error: Some(format!("Compile error: {error}")),
        captured_output: None,
        timeout: None,
        duration_ms: total_start.elapsed().as_millis() as u64,
        phases: Some(PhaseTimings {
            compile_ms: compile_start.elapsed().as_millis() as u64,
            ..PhaseTimings::default()
        }),
    }
}
