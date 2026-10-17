use super::*;
use crate::value::json::{json_to_vm_value, vm_value_to_json};
use crate::verifier_provenance::IsolatedPythonSourceVerifier;

struct SourceFixture {
    _directory: tempfile::TempDir,
    request: IsolatedPythonSourceVerifier,
}

impl SourceFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("private source verifier fixture");
        let workspace = directory.path().canonicalize().unwrap();
        let interpreter = workspace.join("admitted-python");
        std::fs::copy("/usr/bin/python3", &interpreter).expect("required real Python interpreter");
        let source = workspace.join("runner.py");
        let target = workspace.join("test_calc.py");
        std::fs::write(
            &source,
            "import runpy, sys\nrunpy.run_path(sys.argv[1], run_name='__main__')\n",
        )
        .unwrap();
        std::fs::write(&target, "assert False, 'KNOWN_FAILING_ASSERTION'\n").unwrap();
        Self {
            _directory: directory,
            request: IsolatedPythonSourceVerifier {
                id: "calculation".to_string(),
                interpreter,
                source,
                workspace,
                args: vec![target.display().to_string()],
            },
        }
    }

    fn declaration(&self) -> (RunIntent, HostFacts) {
        let mut intent = intent();
        intent.verifier_loaders = vec![self.request.clone().into()];
        let mut host = host_facts();
        host.approval_policy = unattended_policy(WorkspaceTrust::HostMaterialized);
        host.admitted_verifier_loaders
            .insert(self.request.clone().into());
        (intent, host)
    }
}

#[derive(Clone)]
struct SourceExecutor {
    request: IsolatedPythonSourceVerifier,
    mutate_and_repair: bool,
}

impl SourceExecutor {
    fn invoke(&self, environment: serde_json::Value) -> Result<serde_json::Value, String> {
        let mut vm = crate::new_vm();
        crate::stdlib::process::register_process_builtins(&mut vm);
        let mut command = vec![self.request.interpreter.display().to_string()];
        command.extend(self.request.invocation_args());
        let values = [
            json_to_vm_value(&json!(command)),
            json_to_vm_value(
                &json!({"cwd": self.request.workspace, "env_mode": "replace", "env": environment, "timeout_ms": 10000}),
            ),
        ];
        let builtin = vm
            .builtin_handler_for_test("exec_opts")
            .expect("canonical process builtin registered");
        builtin(&values, &mut String::new())
            .map(|value| vm_value_to_json(&value))
            .map_err(|error| format!("{error:?}"))
    }
}

#[async_trait::async_trait]
impl PreparedRunExecutor for SourceExecutor {
    type Output = serde_json::Value;
    type Error = String;

    async fn execute(&self, _authority: &AuthorityUse) -> Result<Self::Output, Self::Error> {
        assert!(
            prepared_verifier(
                "/bin/cat",
                &[self.request.source.display().to_string()],
                &self.request.workspace
            )
            .unwrap()
            .is_none(),
            "reading runner source is an ordinary command"
        );
        if !self.mutate_and_repair {
            return self.invoke(json!({}));
        }
        std::fs::write(&self.request.source, "print('RUNNER_SHIM_BYPASS')\n").unwrap();
        std::fs::write(
            &self.request.interpreter,
            "#!/bin/sh\nprintf 'INTERPRETER_SHIM_BYPASS\\n'\nexit 0\n",
        )
        .unwrap();
        let ordinary = std::process::Command::new(&self.request.interpreter)
            .args(self.request.invocation_args())
            .output()
            .unwrap();
        assert!(ordinary.status.success());
        assert!(String::from_utf8_lossy(&ordinary.stdout).contains("INTERPRETER_SHIM_BYPASS"));
        let parent_thread = std::thread::current().id();
        let executor = self.clone();
        let first = crate::orchestration::run_blocking_with_ambient(move || {
            assert_ne!(std::thread::current().id(), parent_thread);
            executor.invoke(json!({}))
        })
        .await
        .map_err(|error| error.to_string())??;
        let denied = self
            .invoke(json!({"LD_PRELOAD": "/nonexistent/adversarial-loader.so"}))
            .unwrap_err();
        assert!(
            denied.contains("LD_PRELOAD"),
            "actual launch must refuse the loader override: {denied}"
        );
        std::fs::write(&self.request.args[0], "assert 2 + 2 == 4\n").unwrap();
        let executor = self.clone();
        let registry = Arc::new(crate::stdlib::pool::PoolRegistry::default());
        let child = crate::native_vm::subtask::prepare(registry, async move {
            assert_ne!(std::thread::current().id(), parent_thread);
            executor.invoke(json!({}))
        });
        let second = crate::vm::subtask::spawn(child)
            .await
            .map_err(|error| error.to_string())??;
        Ok(json!({"first": first, "second": second}))
    }
}

fn source_run(request: IsolatedPythonSourceVerifier) -> PreparedRun<SourceExecutor> {
    PreparedRun::with_clock(
        SourceExecutor {
            request,
            mutate_and_repair: false,
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    )
}

fn ready(
    run: &PreparedRun<SourceExecutor>,
    intent: RunIntent,
    host: HostFacts,
) -> Box<AuthorityLease> {
    match run.prepare(intent, host) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("admitted source verifier must be ready: {other:?}"),
    }
}

struct MultipleSourceExecutor(Vec<IsolatedPythonSourceVerifier>);

#[test]
fn source_capture_refuses_a_fifo_without_waiting_for_a_writer() {
    use crate::verifier_provenance::{PreparedVerifier, VerifierLoaderObservation};
    use std::os::unix::ffi::OsStrExt;

    let fixture = SourceFixture::new();
    std::fs::remove_file(&fixture.request.source).unwrap();
    let path = std::ffi::CString::new(fixture.request.source.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is a valid NUL-terminated path in this test's private directory.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(matches!(
        PreparedVerifier::capture(fixture.request.into()),
        Err(VerifierLoaderObservation::Unmeasured { reason })
            if reason.contains("not a regular file")
    ));
}

#[derive(Clone)]
struct ModuleExecutor {
    request: crate::verifier_provenance::IsolatedPythonModuleVerifier,
    background: bool,
}

impl ModuleExecutor {
    async fn invoke(&self) -> Result<serde_json::Value, String> {
        let mut command = vec![self.request.interpreter.display().to_string()];
        command.extend(self.request.invocation_args());
        let params = json_to_vm_value(&json!({
            "mode": "argv", "argv": command,
            "cwd": self.request.workspace, "env_mode": "replace", "env": {}, "timeout_ms": 10000,
        }));
        let operation = if self.background { "spawn" } else { "exec" };
        let mut result = crate::stdlib::host::dispatch_host_operation(
            "process",
            operation,
            params.as_dict().unwrap(),
        )
        .await
        .map_err(|error| format!("{error:?}"))?;
        if self.background {
            let handle = result.as_dict().unwrap().get("handle_id").unwrap().clone();
            let mut wait = crate::value::DictMap::new();
            wait.insert("handle_id".into(), handle);
            let completed =
                crate::stdlib::host::dispatch_host_operation("process", "wait", &wait).await;
            crate::stdlib::host::dispatch_host_operation("process", "release", &wait)
                .await
                .map_err(|error| format!("{error:?}"))?;
            result = completed.map_err(|error| format!("{error:?}"))?;
        }
        Ok(vm_value_to_json(&result))
    }
}

#[async_trait::async_trait]
impl PreparedRunExecutor for ModuleExecutor {
    type Output = serde_json::Value;
    type Error = String;
    async fn execute(&self, _: &AuthorityUse) -> Result<Self::Output, Self::Error> {
        self.invoke().await
    }
}

#[tokio::test]
async fn module_and_package_verifiers_execute_the_baseline_through_the_real_process_builtin() {
    use crate::verifier_provenance::{IsolatedPythonModuleVerifier, VerifierLoaderSpec};
    for package in [false, true] {
        for background in [false, true] {
            let fixture = SourceFixture::new();
            std::fs::remove_file(&fixture.request.source).unwrap();
            let root = &fixture.request.workspace;
            let modules = root.join("modules");
            std::fs::create_dir(&modules).unwrap();
            let (runner, original) = if package {
                let package = modules.join("runner");
                std::fs::create_dir(&package).unwrap();
                std::fs::write(package.join("__init__.py"), "VALUE = 'BASELINE_PACKAGE'\n")
                    .unwrap();
                (package.join("__main__.py"), "from . import VALUE\nimport runpy, sys\nprint(VALUE)\nrunpy.run_path(sys.argv[1], run_name='__main__')\n")
            } else {
                (modules.join("runner.py"), "import runpy, sys\nprint('BASELINE_MODULE')\nrunpy.run_path(sys.argv[1], run_name='__main__')\n")
            };
            std::fs::write(&runner, original).unwrap();
            let declaration = IsolatedPythonModuleVerifier {
                id: "module-runner".into(),
                interpreter: fixture.request.interpreter.clone(),
                module: "runner".into(),
                search_roots: vec![root.clone(), modules],
                dependencies: Vec::new(),
                workspace: root.clone(),
                args: fixture.request.args.clone(),
            };
            let spec = VerifierLoaderSpec::PythonModule(declaration.clone());
            let (mut intent, mut host) = fixture.declaration();
            intent.verifier_loaders = vec![spec.clone()];
            host.admitted_verifier_loaders = BTreeSet::from([spec]);
            let run = PreparedRun::with_clock(
                ModuleExecutor {
                    request: declaration,
                    background,
                },
                Arc::new(MemoryAuthorityReceiptSink::default()),
                Arc::new(|| NOW_MS),
            );
            let prepare = || match run.prepare(intent.clone(), host.clone()) {
                PreparationOutcome::Ready {
                    authority_lease, ..
                } => authority_lease,
                other => panic!("module preparation failed: {other:?}"),
            };
            let first = prepare();
            let schema: serde_json::Value =
                serde_json::from_str(RUN_AUTHORITY_PLAN_V1_SCHEMA_JSON).unwrap();
            let validator = jsonschema::validator_for(&schema).unwrap();
            let plan = serde_json::to_value(first.plan()).unwrap();
            assert!(
                validator.is_valid(&plan),
                "module loader declaration must match the public schema"
            );
            // Both a changed selected runner and a newly introduced earlier shadow
            // exit zero normally. Neither can replace the captured execution.
            std::fs::write(&runner, "print('CHANGED_RUNNER')\n").unwrap();
            std::fs::write(root.join("runner.py"), "print('NEW_RUNNER')\n").unwrap();
            let control = std::process::Command::new(&fixture.request.interpreter)
                .args(["-S", "-m", "runner"])
                .env_clear()
                .current_dir(root)
                .output()
                .unwrap();
            assert!(control.status.success());
            assert!(String::from_utf8_lossy(&control.stdout).contains("NEW_RUNNER"));
            let output = match run.execute(first).await {
                ExecutionOutcome::Completed { output, .. } => output,
                ExecutionOutcome::ExecutorFailed { error, .. } => {
                    panic!("baseline execution failed: {error}")
                }
                _ => panic!("baseline execution did not complete"),
            };
            assert_eq!(
                output["status"],
                if background { "exited" } else { "completed" }
            );
            assert_eq!(output["exit_code"], 1);
            assert!(output["stderr"]
                .as_str()
                .unwrap()
                .contains("KNOWN_FAILING_ASSERTION"));
            assert!(
                output["stdout"].as_str().unwrap().contains("BASELINE_"),
                "{output}"
            );
            assert_eq!(
                output["verifier_receipt"]["executed_material"]["disposition"],
                "unchanged"
            );
            let digest = output["verifier_receipt"]["baseline_sha256"].clone();
            std::fs::write(&fixture.request.args[0], "assert 2 + 2 == 4\n").unwrap();
            let second = match run.execute(prepare()).await {
                ExecutionOutcome::Completed { output, .. } => output,
                ExecutionOutcome::ExecutorFailed { error, .. } => {
                    panic!("refreshed baseline execution failed: {error}")
                }
                _ => panic!("refreshed baseline execution did not complete"),
            };
            assert_eq!(second["exit_code"], 0);
            assert_eq!(second["verifier_receipt"]["baseline_sha256"], digest);
            assert!(!second["stdout"].as_str().unwrap().contains("RUNNER"));
        }
    }
}

#[test]
fn absent_baseline_cannot_be_recaptured_after_goal_refresh() {
    let fixture = SourceFixture::new();
    std::fs::remove_file(&fixture.request.source).unwrap();
    let run = source_run(fixture.request.clone());
    let (intent, host) = fixture.declaration();
    let first = run.prepare(intent.clone(), host.clone());
    assert!(matches!(first, PreparationOutcome::Blocked { .. }));
    std::fs::write(&fixture.request.source, "print('NEW_RUNNER')\n").unwrap();
    assert!(matches!(
        run.prepare(intent, host),
        PreparationOutcome::Blocked { .. }
    ));
}

#[test]
fn identical_invocations_with_different_material_have_different_authority() {
    let fixture = SourceFixture::new();
    let (intent, host) = fixture.declaration();
    let first_run = source_run(fixture.request.clone());
    let first = ready(&first_run, intent.clone(), host.clone());
    std::fs::write(
        &fixture.request.source,
        "print('DIFFERENT_ADMITTED_RUNNER')\n",
    )
    .unwrap();
    let second_run = source_run(fixture.request);
    let second = ready(&second_run, intent.clone(), host.clone());
    assert_eq!(first.plan_fingerprint(), second.plan_fingerprint());
    assert_ne!(first.fingerprint(), second.fingerprint());
    assert_ne!(first.verifier_baselines(), second.verifier_baselines());
    let refreshed = match first_run.prepare(intent, host) {
        PreparationOutcome::Ready {
            authority_lease,
            receipt,
            ..
        } => {
            assert!(matches!(
                receipt.verifier_observations.get("calculation"),
                Some(crate::verifier_provenance::VerifierLoaderObservation::Changed { .. })
            ));
            authority_lease
        }
        _ => panic!("refresh must retain the original baseline"),
    };
    assert_eq!(first.fingerprint(), refreshed.fingerprint());
    assert_eq!(first.verifier_baselines(), refreshed.verifier_baselines());
}

#[test]
fn loader_observations_distinguish_workspace_changes_from_execution_material() {
    use crate::verifier_provenance::{PreparedVerifier, VerifierLoaderObservation};
    let fixture = SourceFixture::new();
    let captured = PreparedVerifier::capture(fixture.request.clone().into()).unwrap();
    assert_eq!(
        captured.observe_workspace(),
        VerifierLoaderObservation::Unchanged
    );
    std::fs::write(&fixture.request.source, "print('REPLACEMENT')\n").unwrap();
    assert_eq!(
        captured.observe_workspace(),
        VerifierLoaderObservation::Changed {
            paths: vec![fixture.request.source.clone()]
        }
    );
    std::fs::remove_file(&fixture.request.source).unwrap();
    assert_eq!(
        captured.observe_workspace(),
        VerifierLoaderObservation::Absent {
            paths: vec![fixture.request.source.clone()]
        }
    );
    assert!(matches!(
        PreparedVerifier::capture(fixture.request.into()),
        Err(VerifierLoaderObservation::Absent { .. })
    ));
}

#[tokio::test]
async fn canonical_exec_and_spawn_refuse_exit_zero_without_the_declared_entrypoint() {
    use crate::verifier_provenance::{IsolatedPythonModuleVerifier, VerifierLoaderSpec};
    for background in [false, true] {
        for exits_in_package in [false, true] {
            let fixture = SourceFixture::new();
            let root = &fixture.request.workspace;
            let package = root.join("runner");
            std::fs::create_dir(&package).unwrap();
            std::fs::write(
                package.join("__init__.py"),
                if exits_in_package {
                    "raise SystemExit(0)\n"
                } else {
                    ""
                },
            )
            .unwrap();
            std::fs::write(
                package.join("__main__.py"),
                "assert False, 'ENTRYPOINT_MUST_RUN'\n",
            )
            .unwrap();
            let declaration = IsolatedPythonModuleVerifier {
                id: "missing-witness".into(),
                interpreter: if exits_in_package {
                    fixture.request.interpreter.clone()
                } else {
                    Path::new("/usr/bin/true").canonicalize().unwrap()
                },
                module: "runner".into(),
                search_roots: vec![root.clone()],
                dependencies: Vec::new(),
                workspace: root.clone(),
                args: Vec::new(),
            };
            let spec = VerifierLoaderSpec::PythonModule(declaration.clone());
            let (mut intent, mut host) = fixture.declaration();
            intent.verifier_loaders = vec![spec.clone()];
            host.admitted_verifier_loaders = BTreeSet::from([spec]);
            let run = PreparedRun::with_clock(
                ModuleExecutor {
                    request: declaration,
                    background,
                },
                Arc::new(MemoryAuthorityReceiptSink::default()),
                Arc::new(|| NOW_MS),
            );
            let lease = match run.prepare(intent, host) {
                PreparationOutcome::Ready {
                    authority_lease, ..
                } => authority_lease,
                other => panic!("negative control must reach execution: {other:?}"),
            };
            match run.execute(lease).await {
                ExecutionOutcome::ExecutorFailed { error, .. } => {
                    assert!(error.contains("execution is unmeasured"), "{error}");
                }
                _ => panic!("exit zero without the entrypoint cannot qualify"),
            }
        }
    }
}

#[test]
fn module_capture_does_not_execute_workspace_import_or_startup_code() {
    use crate::verifier_provenance::{
        IsolatedPythonModuleVerifier, PreparedVerifier, VerifierLoaderSpec,
    };
    let fixture = SourceFixture::new();
    let root = &fixture.request.workspace;
    let marker = root.join("startup-executed");
    let startup = format!(
        "open({:?}, 'w').write('startup')\n",
        marker.display().to_string()
    );
    std::fs::write(root.join("sitecustomize.py"), startup).unwrap();
    std::fs::write(root.join("runner.py"), "assert False\n").unwrap();
    let request = IsolatedPythonModuleVerifier {
        id: "startup-control".into(),
        interpreter: fixture.request.interpreter.clone(),
        module: "runner".into(),
        search_roots: vec![root.clone()],
        dependencies: Vec::new(),
        workspace: root.clone(),
        args: Vec::new(),
    };
    let captured = PreparedVerifier::capture(VerifierLoaderSpec::PythonModule(request)).unwrap();
    assert_eq!(
        captured.observe_workspace(),
        crate::verifier_provenance::VerifierLoaderObservation::Unchanged
    );
    assert!(!marker.exists());
    let probe = std::process::Command::new(&fixture.request.interpreter)
        .args([
            "-c",
            "import importlib.util; print(importlib.util.find_spec('runner').origin)",
        ])
        .env_clear()
        .env("PYTHONPATH", root)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(probe.status.success());
    assert!(
        marker.exists(),
        "the independent-probe control must execute startup code"
    );
    assert!(String::from_utf8_lossy(&probe.stdout).contains("runner.py"));
}

#[async_trait::async_trait]
impl PreparedRunExecutor for MultipleSourceExecutor {
    type Output = Vec<serde_json::Value>;
    type Error = String;

    async fn execute(&self, _authority: &AuthorityUse) -> Result<Self::Output, Self::Error> {
        self.0
            .iter()
            .map(|request| {
                SourceExecutor {
                    request: request.clone(),
                    mutate_and_repair: false,
                }
                .invoke(json!({}))
            })
            .collect()
    }
}

#[tokio::test]
async fn two_admitted_invocations_sharing_a_source_execute_their_own_arguments() {
    let fixture = SourceFixture::new();
    let mut first = fixture.request.clone();
    first.id = "a-first".to_string();
    let mut second = first.clone();
    second.id = "z-second".to_string();
    let second_test = fixture.request.workspace.join("test_second.py");
    std::fs::write(&second_test, "print('SECOND_ADMITTED_SOURCE_REACHED')\n").unwrap();
    second.args = vec![second_test.display().to_string()];
    let (mut intent, mut host) = fixture.declaration();
    intent.verifier_loaders = vec![first.clone().into(), second.clone().into()];
    host.admitted_verifier_loaders = BTreeSet::from([first.clone().into(), second.clone().into()]);
    let run = PreparedRun::with_clock(
        MultipleSourceExecutor(vec![first, second]),
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    );
    let lease = match run.prepare(intent, host) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("both exact invocations must be admitted: {other:?}"),
    };
    match run.execute(lease).await {
        ExecutionOutcome::Completed { output, .. } => {
            assert_eq!(output.len(), 2);
            assert_eq!(output[0]["verifier_receipt"]["verifier_id"], "a-first");
            assert_eq!(output[0]["status"], 1);
            assert!(output[0]["stderr"]
                .as_str()
                .unwrap()
                .contains("KNOWN_FAILING_ASSERTION"));
            assert_eq!(output[1]["verifier_receipt"]["verifier_id"], "z-second");
            assert_eq!(output[1]["status"], 0);
            assert!(output[1]["stdout"]
                .as_str()
                .unwrap()
                .contains("SECOND_ADMITTED_SOURCE_REACHED"));
        }
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("exact admission failed: {error}")
        }
    }
}

#[tokio::test]
async fn an_elf_that_ignores_python_source_cannot_mint_verifier_authority() {
    let mut fixture = SourceFixture::new();
    fixture.request.interpreter = Path::new("/usr/bin/true").canonicalize().unwrap();
    let ignored = std::process::Command::new(&fixture.request.interpreter)
        .args(fixture.request.invocation_args())
        .status()
        .unwrap();
    assert!(
        ignored.success(),
        "the negative must demonstrate exit-zero without running the failing source"
    );
    let run = source_run(fixture.request.clone());
    let (intent, host) = fixture.declaration();
    let lease = ready(&run, intent, host);
    match run.execute(lease).await {
        ExecutionOutcome::ExecutorFailed { error, .. } => {
            assert!(error.contains("execution is unmeasured"), "{error}");
        }
        ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("admission failed before negative execution: {error}")
        }
        ExecutionOutcome::Completed { .. } => {
            panic!("exit-zero without source execution must not mint a verifier receipt")
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_source_verifier_preserves_registered_main_module_test_discovery() {
    let fixture = SourceFixture::new();
    std::fs::write(
        &fixture.request.source,
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/source_verifier_unittest.py"
        )),
    )
    .unwrap();
    let mut run = source_run(fixture.request.clone());
    run.executor.mutate_and_repair = true;
    let (intent, host) = fixture.declaration();
    let lease = ready(&run, intent, host);
    match run.execute(lease).await {
        ExecutionOutcome::Completed { output, .. } => {
            assert_eq!(
                output["first"]["status"], 1,
                "the registered main module must discover the known failing test"
            );
            assert!(output["first"]["stderr"]
                .as_str()
                .unwrap()
                .contains("KNOWN_FAILING_ASSERTION"));
            assert!(output["first"]["stderr"]
                .as_str()
                .unwrap()
                .contains("Ran 1 test"));
            assert_eq!(output["second"]["status"], 0);
            assert!(output["second"]["stderr"]
                .as_str()
                .unwrap()
                .contains("Ran 1 test"));
        }
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("main-module execution must settle: {error}")
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_source_verifier_executes_original_bytes_after_interpreter_and_runner_mutation() {
    let fixture = SourceFixture::new();
    let mut run = source_run(fixture.request.clone());
    run.executor.mutate_and_repair = true;
    let (intent, host) = fixture.declaration();
    let lease = ready(&run, intent, host);
    match run.execute(lease).await {
        ExecutionOutcome::Completed { output, receipt } => {
            assert!(receipt.executor_invoked);
            let first = &output["first"];
            assert_eq!(first["verifier_receipt"]["verifier_id"], "calculation");
            assert_eq!(first["status"], 1);
            assert!(first["stderr"]
                .as_str()
                .unwrap()
                .contains("KNOWN_FAILING_ASSERTION"));
            assert!(!first["stdout"].as_str().unwrap().contains("SHIM_BYPASS"));
            let second = &output["second"];
            assert_eq!(second["verifier_receipt"]["verifier_id"], "calculation");
            assert_eq!(second["status"], 0);
            assert_eq!(second["success"], true);
        }
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("prepared source execution must complete: {error}")
        }
    }
}

#[test]
fn prepared_source_verifier_refuses_missing_host_admission_and_forged_baseline_fields() {
    let fixture = SourceFixture::new();
    let run = source_run(fixture.request.clone());
    let (intent, mut host) = fixture.declaration();
    host.admitted_verifier_loaders.clear();
    assert!(
        matches!(run.prepare(intent, host), PreparationOutcome::Blocked { diagnostics, .. }
        if diagnostics.iter().any(|item| item.code == "verifier_unmeasured"))
    );
    let mut forged = serde_json::to_value(&fixture.request).unwrap();
    forged["baseline_sha256"] = json!("model-provided-digest");
    assert!(serde_json::from_value::<IsolatedPythonSourceVerifier>(forged).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_prepared_verifiers_keep_their_own_native_material_across_worker_hops() {
    let first = SourceFixture::new();
    let second = SourceFixture::new();
    let mut first_run = source_run(first.request.clone());
    let mut second_run = source_run(second.request.clone());
    first_run.executor.mutate_and_repair = true;
    second_run.executor.mutate_and_repair = true;
    let (first_intent, first_host) = first.declaration();
    let (second_intent, second_host) = second.declaration();
    let first_lease = ready(&first_run, first_intent, first_host);
    let second_lease = ready(&second_run, second_intent, second_host);
    let results = tokio::join!(
        first_run.execute(first_lease),
        second_run.execute(second_lease)
    );
    for result in <[_; 2]>::from(results) {
        match result {
            ExecutionOutcome::Completed { output, .. } => {
                assert_eq!(
                    output["first"]["verifier_receipt"]["verifier_id"],
                    "calculation"
                );
                assert_eq!(output["first"]["status"], 1);
                assert_eq!(
                    output["second"]["verifier_receipt"]["verifier_id"],
                    "calculation"
                );
                assert_eq!(output["second"]["status"], 0);
            }
            ExecutionOutcome::ExecutorFailed { error, .. }
            | ExecutionOutcome::AuthorityFailed { error, .. } => {
                panic!("each prepared execution must retain its own authority: {error}")
            }
        }
    }
    assert!(
        prepared_verifier(
            &first.request.interpreter.display().to_string(),
            &first.request.invocation_args(),
            &first.request.workspace,
        )
        .unwrap()
        .is_none(),
        "native authority must leave with its execution scope"
    );
}

#[tokio::test]
async fn prepared_source_verifier_session_turns_keep_original_material_and_restart_refuses_recapture(
) {
    let fixture = SourceFixture::new();
    let (intent, host) = fixture.declaration();
    let claims = Arc::new(MemoryPreparedSessionLeaseStore::default());
    let session = PreparedSession::new(source_run(fixture.request.clone()), claims.clone());
    let binding = super::identity_session::prepared_session_binding();
    let lease = match session.prepare(binding.clone(), intent.clone(), host.clone()) {
        PreparedSessionUpdate::Ready { lease, .. } => *lease,
        other => panic!("source session must be ready: {other:?}"),
    };
    std::fs::write(&fixture.request.source, "print('RUNNER_SHIM_BYPASS')\n").unwrap();
    let attachment = PreparedRuntimeAttachment {
        session_id: binding.session_id,
        workspace_fingerprint: binding.workspace_fingerprint,
        runtime: binding.runtime,
        consumer: binding.consumer,
    };
    let mut forged = lease.clone();
    let crate::verifier_provenance::VerifierLoaderSpec::PythonSource(request) =
        &mut forged.intent.verifier_loaders[0]
    else {
        panic!("source loader fixture");
    };
    request.args = vec!["forged-target.py".to_string()];
    assert!(
        matches!(session.attach(forged, host.clone(), attachment.clone()), Err(PreparedSessionUpdate::Blocked { diagnostics, .. })
        if diagnostics.iter().any(|item| item.code == "prepared_session_intent_drift"))
    );
    let active = session
        .attach(lease, host.clone(), attachment.clone())
        .unwrap();
    let first = session.run_turn(&active).await.unwrap();
    assert_eq!(first["status"], 1);
    assert!(first["stderr"]
        .as_str()
        .unwrap()
        .contains("KNOWN_FAILING_ASSERTION"));
    std::fs::write(&fixture.request.args[0], "assert 2 + 2 == 4\n").unwrap();
    assert_eq!(session.run_turn(&active).await.unwrap()["status"], 0);

    let original = SourceFixture::new();
    let (intent, host) = original.declaration();
    let claims = Arc::new(MemoryPreparedSessionLeaseStore::default());
    let before_restart = PreparedSession::new(source_run(original.request.clone()), claims.clone());
    let binding = super::identity_session::prepared_session_binding();
    let lease = match before_restart.prepare(binding, intent, host.clone()) {
        PreparedSessionUpdate::Ready { lease, .. } => *lease,
        other => panic!("original session must be ready: {other:?}"),
    };
    std::fs::write(&original.request.source, "print('RUNNER_SHIM_BYPASS')\n").unwrap();
    let restarted = PreparedSession::new(source_run(original.request), claims);
    assert!(
        matches!(restarted.attach(lease, host, attachment), Err(PreparedSessionUpdate::Blocked { diagnostics, .. })
        if diagnostics.iter().any(|item| item.code == "verifier_baseline_unmeasured"))
    );
}
