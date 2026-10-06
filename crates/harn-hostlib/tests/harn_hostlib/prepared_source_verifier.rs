#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use harn_hostlib::process::{
    default_spawner, EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec, WaitOutcome,
};
use harn_vm::harness_net::{NetPolicy, NetPolicyDefault, OnViolation};
use harn_vm::orchestration::{CapabilityPolicy, SandboxProfile, ToolApprovalPolicy};
use harn_vm::prepared_run::*;
use harn_vm::verifier_provenance::IsolatedPythonSourceVerifier;

struct FixtureExecutor {
    request: IsolatedPythonSourceVerifier,
    ignores_source: bool,
    unittest_runner: bool,
    background_only: bool,
}

impl FixtureExecutor {
    fn spec(&self, owner_death: OwnerDeathPolicy) -> SpawnSpec {
        SpawnSpec {
            builtin: "prepared_source_verifier_test",
            program: self.request.interpreter.display().to_string(),
            args: self.request.invocation_args(),
            cwd: Some(self.request.workspace.clone()),
            env: BTreeMap::new(),
            env_remove: Vec::new(),
            env_mode: EnvMode::Replace,
            use_stdin: false,
            configure_process_group: true,
            owner_death,
            output_capture: OutputCapture::Pipe,
        }
    }

    fn invoke(&self, mode: OwnerDeathPolicy, blocking: bool) -> (i32, String) {
        let mut child = default_spawner()
            .spawn(self.spec(mode))
            .expect("actual hostlib verifier spawn");
        assert!(
            child.missing_program().is_none(),
            "sealed executable cannot be classified by a removed origin"
        );
        let code = if blocking {
            child.wait().unwrap().code.expect("normal verifier exit")
        } else {
            match child
                .wait_with_timeout(Some(Duration::from_secs(10)), &|| false)
                .unwrap()
            {
                WaitOutcome::Exited(status) => status.code.expect("normal verifier exit"),
                other => panic!("verifier did not settle: {other:?}"),
            }
        };
        let mut stderr = String::new();
        child
            .take_stderr()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        (code, stderr)
    }

    fn background_result(&self) -> serde_json::Value {
        let _environment = harn_vm::stdlib::process::declare_session_environment_if_absent(
            harn_vm::security::SessionEnvironment::isolated(),
        );
        let session_id = format!("source-witness-{}", uuid::Uuid::now_v7());
        let info = harn_hostlib::tools::long_running::spawn_long_running(
            "prepared_source_verifier_test",
            self.request.interpreter.display().to_string(),
            self.request.invocation_args(),
            Some(self.request.workspace.clone()),
            BTreeMap::new(),
            session_id.clone(),
        )
        .expect("actual background verifier spawn");
        let result = harn_hostlib::tools::long_running::register_result_notifier(&info.handle_id)
            .expect("live or retained terminal handle")
            .recv_timeout(Duration::from_secs(10))
            .expect("actual background verifier terminal result");
        let schema = harn_hostlib::schemas::lookup(
            "tools",
            "wait_command",
            harn_hostlib::schemas::SchemaKind::Response,
        )
        .expect("canonical background wait response schema");
        let schema: serde_json::Value = serde_json::from_str(schema).unwrap();
        harn_vm::schema::validate_value_against_schema(
            &result,
            &harn_vm::schema::json_to_vm_value(&schema),
            false,
        )
        .expect("actual terminal result conforms to the public wait contract");
        let feedback = harn_vm::orchestration::agent_inbox::drain(&session_id);
        assert_eq!(feedback.len(), 1, "background terminal feedback must fire");
        let payload: serde_json::Value = serde_json::from_str(&feedback[0].content).unwrap();
        let terminal = result.as_dict().expect("retained terminal payload");
        for key in ["status", "stderr"] {
            let Some(harn_vm::VmValue::String(value)) = terminal.get(key) else {
                panic!("missing terminal string {key}")
            };
            assert_eq!(payload[key].as_str(), Some(value.as_str()));
        }
        match terminal.get("exit_code") {
            Some(harn_vm::VmValue::Int(code)) => assert_eq!(payload["exit_code"], *code),
            Some(harn_vm::VmValue::Nil) => assert!(payload["exit_code"].is_null()),
            other => panic!("missing terminal exit observation: {other:?}"),
        }
        payload
    }
}

#[async_trait::async_trait]
impl PreparedRunExecutor for FixtureExecutor {
    type Output = ();
    type Error = String;

    async fn execute(&self, _authority: &AuthorityUse) -> Result<(), String> {
        if self.ignores_source {
            for mode in [OwnerDeathPolicy::None, OwnerDeathPolicy::KillContainment]
                .into_iter()
                .filter(|_| !self.background_only)
            {
                for blocking in [false, true] {
                    let mut child = default_spawner()
                        .spawn(self.spec(mode))
                        .expect("actual negative verifier spawn");
                    let error = if blocking {
                        child
                            .wait()
                            .expect_err("blocking wait must require the witness")
                    } else {
                        child
                            .wait_with_timeout(Some(Duration::from_secs(10)), &|| false)
                            .expect_err("exit zero without bootstrap execution must be unmeasured")
                    };
                    assert!(
                        error.to_string().contains("execution is unmeasured"),
                        "{error}"
                    );
                }
            }
            let result = self.background_result();
            assert_eq!(result["status"], "blocked");
            assert!(result["exit_code"].is_null());
            assert!(result["signal"].is_null());
            assert!(result["stderr"]
                .as_str()
                .unwrap()
                .contains("execution is unmeasured"));
            return Ok(());
        }
        // Mutation begins only after PreparedRun installs the native authority.
        std::fs::write(&self.request.source, "print('RUNNER_SHIM_BYPASS')\n").unwrap();
        std::fs::remove_file(&self.request.interpreter).unwrap();
        for mode in [OwnerDeathPolicy::None, OwnerDeathPolicy::KillContainment] {
            for blocking in [false, true] {
                std::fs::write(
                    &self.request.args[0],
                    "assert False, 'KNOWN_FAILING_ASSERTION'\n",
                )
                .unwrap();
                let (failed, stderr) = self.invoke(mode, blocking);
                assert_eq!(failed, 1);
                assert!(
                    stderr.contains("KNOWN_FAILING_ASSERTION"),
                    "actual original runner must reach the test: {stderr}"
                );
                if self.unittest_runner {
                    assert!(
                        stderr.contains("Ran 1 test"),
                        "actual main-module discovery must reach the test: {stderr}"
                    );
                }
                std::fs::write(&self.request.args[0], "assert 2 + 2 == 4\n").unwrap();
                let (passed, stderr) = self.invoke(mode, blocking);
                assert_eq!(passed, 0);
                if self.unittest_runner {
                    assert!(stderr.contains("Ran 1 test"), "{stderr}");
                }
                std::fs::write(&self.request.args[0], "raise SystemExit(127)\n").unwrap();
                let (status, stderr) = self.invoke(mode, blocking);
                if self.unittest_runner {
                    assert_eq!(status, 1);
                    assert!(stderr.contains("SystemExit: 127"), "{stderr}");
                } else {
                    assert_eq!(
                        status, 127,
                        "a real verifier exit is not missing-program evidence"
                    );
                }
                let mut adversarial = self.spec(mode);
                adversarial.env.insert(
                    "LD_PRELOAD".to_string(),
                    "/nonexistent/adversarial-loader.so".to_string(),
                );
                match default_spawner().spawn(adversarial) {
                    Err(error) => assert!(
                        error.to_string().contains("LD_PRELOAD"),
                        "loader refusal must be explicit: {error}"
                    ),
                    Ok(_) => panic!("agent-controlled loader must not reach the sealed verifier"),
                }
            }
        }
        std::fs::write(&self.request.args[0], "assert 2 + 2 == 4\n").unwrap();
        let result = self.background_result();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["exit_code"], 0);
        Ok(())
    }
}

#[tokio::test]
async fn prepared_source_verifier_direct_and_guardian_keep_original_material_and_refuse_loader_overrides(
) {
    source_verifier_control(false, false, false).await;
}

#[tokio::test]
async fn prepared_source_verifier_direct_and_guardian_refuse_an_elf_that_ignores_source() {
    source_verifier_control(true, false, false).await;
}

#[tokio::test]
async fn prepared_source_verifier_background_refuses_an_elf_that_ignores_source() {
    source_verifier_control(true, false, true).await;
}

#[tokio::test]
async fn prepared_source_verifier_direct_and_guardian_preserve_registered_main_module_test_discovery(
) {
    source_verifier_control(false, true, false).await;
}

async fn source_verifier_control(
    ignores_source: bool,
    unittest_runner: bool,
    background_only: bool,
) {
    let _guardian = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);
    let scratch = tempfile::tempdir().unwrap();
    let workspace = scratch.path().canonicalize().unwrap();
    let interpreter = workspace.join("admitted-python");
    std::fs::copy(
        if ignores_source {
            "/usr/bin/true"
        } else {
            "/usr/bin/python3"
        },
        &interpreter,
    )
    .expect("required real interpreter control");
    let source = workspace.join("runner.py");
    std::fs::write(
        &source,
        if unittest_runner {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../harn-vm/tests/fixtures/source_verifier_unittest.py"
            ))
        } else {
            "import runpy, sys\nrunpy.run_path(sys.argv[1], run_name='__main__')\n"
        },
    )
    .unwrap();
    let target = workspace.join("test_calc.py");
    std::fs::write(&target, "assert False, 'KNOWN_FAILING_ASSERTION'\n").unwrap();
    let request = IsolatedPythonSourceVerifier {
        id: "calculation".to_string(),
        interpreter,
        source,
        workspace,
        args: vec![target.display().to_string()],
    };
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Unrestricted,
        ..Default::default()
    };
    let budget = RunBudget {
        spend_microusd: Some(0),
        time_ms: Some(10000),
        turns: Some(2),
    };
    let provenance = RuntimeContractProvenance {
        harn_version: "fixture".to_string(),
        harn_revision: "fixture".to_string(),
        host_name: "hostlib".to_string(),
        host_version: "fixture".to_string(),
        host_revision: "fixture".to_string(),
        contracts_version: "fixture".to_string(),
        runtime_digest: "fixture".to_string(),
    };
    let intent = RunIntent {
        intent_id: "source-verifier".to_string(),
        isolated_source_verifiers: vec![request.clone()],
        capability_policy: policy.clone(),
        network: Vec::new(),
        secrets: Vec::new(),
        admitted_environment: Vec::new(),
        process_sockets: Vec::new(),
        mcp: Vec::new(),
        toolchain_probes: Vec::new(),
        identity_brokers: Vec::new(),
        budget: budget.clone(),
        provenance: provenance.clone(),
        interactivity: RunInteractivity::NonInteractive,
        startup_deadline_at_ms: 10000,
        receipt_uri: "memory:source-verifier".to_string(),
    };
    let host = HostFacts {
        capability_ceiling: policy,
        admitted_source_verifiers: BTreeSet::from([request.clone()]),
        approval_policy: RunApprovalPolicy::construct(
            RunAuthorityPosture {
                interactivity: RunInteractivity::NonInteractive,
                approval_availability: ApprovalAvailability::Unavailable,
                workspace_trust: WorkspaceTrust::HostMaterialized,
            },
            |_| ToolApprovalPolicy::default(),
        ),
        approved_batches: BTreeMap::new(),
        net_policy: NetPolicy {
            allow: Arc::new(Vec::new()),
            deny: Arc::new(Vec::new()),
            default: NetPolicyDefault::Deny,
            on_violation: OnViolation::Error,
        },
        secret_bindings: BTreeSet::new(),
        secret_brokers: BTreeMap::new(),
        admitted_environment: BTreeSet::new(),
        process_sockets: BTreeSet::new(),
        mcp: BTreeSet::new(),
        toolchain_probes: BTreeSet::new(),
        identity_brokers: BTreeMap::new(),
        budget_ceiling: budget,
        provenance,
    };
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            request,
            ignores_source,
            unittest_runner,
            background_only,
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| 1),
    );
    let lease = match run.prepare(intent, host) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("hostlib source verifier must be ready: {other:?}"),
    };
    match run.execute(lease).await {
        ExecutionOutcome::Completed { receipt, .. } => assert!(receipt.executor_invoked),
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("prepared hostlib verifier execution failed: {error}")
        }
    }
}
