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
        intent.isolated_source_verifiers = vec![self.request.clone()];
        let mut host = host_facts();
        host.approval_policy = unattended_policy(WorkspaceTrust::HostMaterialized);
        host.admitted_source_verifiers.insert(self.request.clone());
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
        let mut vm = crate::vm::Vm::new();
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
            .builtins
            .get("exec_opts")
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
        let child = crate::vm::subtask::prepare(registry, async move {
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
    intent.isolated_source_verifiers = vec![first.clone(), second.clone()];
    host.admitted_source_verifiers = BTreeSet::from([first.clone(), second.clone()]);
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
            assert_eq!(output[0]["source_verifier_id"], "a-first");
            assert_eq!(output[0]["status"], 1);
            assert!(output[0]["stderr"]
                .as_str()
                .unwrap()
                .contains("KNOWN_FAILING_ASSERTION"));
            assert_eq!(output[1]["source_verifier_id"], "z-second");
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
            assert_eq!(first["source_verifier_id"], "calculation");
            assert_eq!(first["status"], 1);
            assert!(first["stderr"]
                .as_str()
                .unwrap()
                .contains("KNOWN_FAILING_ASSERTION"));
            assert!(!first["stdout"].as_str().unwrap().contains("SHIM_BYPASS"));
            let second = &output["second"];
            assert_eq!(second["source_verifier_id"], "calculation");
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
    host.admitted_source_verifiers.clear();
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
    for result in [results.0, results.1] {
        match result {
            ExecutionOutcome::Completed { output, .. } => {
                assert_eq!(output["first"]["source_verifier_id"], "calculation");
                assert_eq!(output["first"]["status"], 1);
                assert_eq!(output["second"]["source_verifier_id"], "calculation");
                assert_eq!(output["second"]["status"], 0);
            }
            ExecutionOutcome::ExecutorFailed { error, .. }
            | ExecutionOutcome::AuthorityFailed { error, .. } => {
                panic!("each prepared execution must retain its own authority: {error}")
            }
        }
    }
    assert!(
        prepared_source_verifier(
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
    forged.intent.isolated_source_verifiers[0].args = vec!["forged-target.py".to_string()];
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
    let lease = match before_restart.prepare(binding.clone(), intent, host.clone()) {
        PreparedSessionUpdate::Ready { lease, .. } => *lease,
        other => panic!("original session must be ready: {other:?}"),
    };
    std::fs::write(&original.request.source, "print('RUNNER_SHIM_BYPASS')\n").unwrap();
    let restarted = PreparedSession::new(source_run(original.request.clone()), claims);
    assert!(
        matches!(restarted.attach(lease, host, attachment), Err(PreparedSessionUpdate::Blocked { diagnostics, .. })
        if diagnostics.iter().any(|item| item.code == "verifier_baseline_unmeasured"))
    );
}
