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

    fn invoke(&self, mode: OwnerDeathPolicy) -> (i32, String) {
        let mut child = default_spawner()
            .spawn(self.spec(mode))
            .expect("actual hostlib verifier spawn");
        assert!(
            child.missing_program().is_none(),
            "sealed executable cannot be classified by a removed origin"
        );
        let outcome = child
            .wait_with_timeout(Some(Duration::from_secs(10)), &|| false)
            .unwrap();
        let code = match outcome {
            WaitOutcome::Exited(status) => status.code.expect("normal verifier exit"),
            other => panic!("verifier did not settle: {other:?}"),
        };
        let mut stderr = String::new();
        child
            .take_stderr()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        (code, stderr)
    }
}

#[async_trait::async_trait]
impl PreparedRunExecutor for FixtureExecutor {
    type Output = ();
    type Error = String;

    async fn execute(&self, _authority: &AuthorityUse) -> Result<(), String> {
        // Mutation begins only after PreparedRun installs the native authority.
        std::fs::write(&self.request.source, "print('RUNNER_SHIM_BYPASS')\n").unwrap();
        std::fs::remove_file(&self.request.interpreter).unwrap();
        for mode in [OwnerDeathPolicy::None, OwnerDeathPolicy::KillContainment] {
            std::fs::write(
                &self.request.args[0],
                "assert False, 'KNOWN_FAILING_ASSERTION'\n",
            )
            .unwrap();
            let (failed, stderr) = self.invoke(mode);
            assert_eq!(failed, 1);
            assert!(
                stderr.contains("KNOWN_FAILING_ASSERTION"),
                "actual original runner must reach the test: {stderr}"
            );
            std::fs::write(&self.request.args[0], "assert 2 + 2 == 4\n").unwrap();
            assert_eq!(self.invoke(mode).0, 0);
            std::fs::write(&self.request.args[0], "raise SystemExit(127)\n").unwrap();
            assert_eq!(
                self.invoke(mode).0,
                127,
                "a real verifier exit is not missing-program evidence"
            );
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
        Ok(())
    }
}

#[tokio::test]
async fn prepared_source_verifier_direct_and_guardian_keep_original_material_and_refuse_loader_overrides(
) {
    let _guardian = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);
    let scratch = tempfile::tempdir().unwrap();
    let workspace = scratch.path().canonicalize().unwrap();
    let interpreter = workspace.join("admitted-python");
    std::fs::copy("/usr/bin/python3", &interpreter).expect("required real Python interpreter");
    let source = workspace.join("runner.py");
    std::fs::write(
        &source,
        "import runpy, sys\nrunpy.run_path(sys.argv[1], run_name='__main__')\n",
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
        spend_microusd: None,
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
        FixtureExecutor { request },
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
        other => panic!("prepared hostlib verifier execution failed: {other:?}"),
    }
}
