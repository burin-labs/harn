use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use crate::harness_net::{NetPolicy, NetPolicyDefault, NetPolicyRule, OnViolation};
use crate::orchestration::{
    CapabilityPolicy, ProcessSandboxPolicy, ProcessSandboxPreset, RunApprovalPolicy,
    RunAuthorityPosture, SandboxProfile, ToolApprovalPolicy, WorkspaceTrust,
};

use super::*;

mod executor_failures;

const NOW_MS: u64 = 1_000;
const DEADLINE_MS: u64 = 61_000;
const SECRET_CANARY: &str = "secret-value-that-must-never-serialize";

#[derive(Clone)]
struct FixtureExecutor {
    requirements: Vec<AuthorityRequirement>,
    model_calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl PreparedRunExecutor for FixtureExecutor {
    type Output = &'static str;
    type Error = String;

    async fn execute(&self, authority: &AuthorityUse) -> Result<Self::Output, String> {
        for requirement in &self.requirements {
            authority.authorize(requirement)?;
        }
        self.model_calls.fetch_add(1, Ordering::SeqCst);
        Ok("completed")
    }
}

struct ExpiringExecutor {
    clock: Arc<AtomicU64>,
    model_calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl PreparedRunExecutor for ExpiringExecutor {
    type Output = ();
    type Error = String;

    async fn execute(&self, authority: &AuthorityUse) -> Result<Self::Output, String> {
        authority.authorize(&AuthorityRequirement::FilesystemWrite {
            root: "/workspace".to_string(),
        })?;
        self.clock.store(DEADLINE_MS + 1, Ordering::SeqCst);
        authority.authorize(&AuthorityRequirement::Network(network("api.example.test")))?;
        self.model_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn provenance() -> RuntimeContractProvenance {
    RuntimeContractProvenance {
        harn_version: "0.10.91".to_string(),
        harn_revision: "harn-revision".to_string(),
        host_name: "burin".to_string(),
        host_version: "0.2.0".to_string(),
        host_revision: "burin-revision".to_string(),
        contracts_version: "2026-08-14".to_string(),
        runtime_digest: "sha256:runtime".to_string(),
    }
}

fn provider_secret(consumer: &str) -> SecretRequirement {
    SecretRequirement {
        reference: SecretReference::parse("harn-secret://burin.provider_auth/openai_api_key")
            .expect("provider secret reference"),
        source: SecretSourceKind::ProcessLocal,
        consumer: SecretConsumerBinding {
            kind: SecretConsumerKind::Provider,
            id: consumer.to_string(),
            environment_name: Some("OPENAI_API_KEY".to_string()),
        },
    }
}

fn network(destination: &str) -> NetworkRequirement {
    NetworkRequirement {
        destination: destination.to_string(),
        protocol: "https".to_string(),
        port: 443,
    }
}

fn native_fixture_path(path: &str) -> String {
    #[cfg(windows)]
    {
        format!(r"C:\{}", path.trim_start_matches('/').replace('/', r"\"))
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

fn toolchain_probe(root_ceiling: &str) -> ToolchainProbeRequirement {
    ToolchainProbeRequirement {
        probe_id: "rust-sysroot".to_string(),
        executable: "rustc".to_string(),
        arguments: vec!["--print".to_string(), "sysroot".to_string()],
        working_directory: native_fixture_path("/workspace"),
        read_root_ceiling: native_fixture_path(root_ceiling),
    }
}

fn identity_requirement() -> IdentityBrokerRequirement {
    IdentityBrokerRequirement {
        reference: PlatformIdentityReference::parse(
            "harn-identity://burin.provider_auth/openai-workload",
        )
        .expect("identity reference"),
        broker_id: "burin-workload".to_string(),
        source: PlatformIdentitySourceKind::WorkloadIdentity,
        renewal: IdentityRenewalMode::BrokerManaged,
        binding: IdentityBrokerBinding {
            provider: "openai".to_string(),
            audience: "https://api.openai.com".to_string(),
            tenant: Some("tenant-a".to_string()),
            consumer: SecretConsumerBinding {
                kind: SecretConsumerKind::Provider,
                id: "openai".to_string(),
                environment_name: None,
            },
        },
    }
}

fn identity_facts(requirement: &IdentityBrokerRequirement) -> IdentityBrokerFacts {
    IdentityBrokerFacts {
        broker_id: requirement.broker_id.clone(),
        sources: BTreeSet::from([
            PlatformIdentitySourceKind::SdkProfile,
            PlatformIdentitySourceKind::WorkloadIdentity,
            PlatformIdentitySourceKind::InstanceMetadata,
            PlatformIdentitySourceKind::HostedBroker,
        ]),
        supports_non_interactive: true,
        may_prompt_gui: false,
        material_outside_sandbox: true,
        opaque_process_local_handles: true,
        renewal_modes: BTreeSet::from([
            IdentityRenewalMode::None,
            IdentityRenewalMode::BrokerManaged,
        ]),
        bindings: BTreeSet::from([requirement.binding.clone()]),
    }
}

fn capability_policy() -> CapabilityPolicy {
    CapabilityPolicy {
        tools: vec!["look".to_string(), "edit".to_string(), "run".to_string()],
        capabilities: BTreeMap::from([
            (
                "fs".to_string(),
                vec!["read".to_string(), "write".to_string()],
            ),
            ("process".to_string(), vec!["exec".to_string()]),
        ]),
        workspace_roots: vec!["/workspace".to_string()],
        read_only_roots: vec!["/toolchains".to_string()],
        side_effect_level: Some("network".to_string()),
        recursion_limit: Some(2),
        tool_arg_constraints: Vec::new(),
        tool_annotations: BTreeMap::new(),
        sandbox_profile: SandboxProfile::Worktree,
        process_sandbox: ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::SystemRuntime,
                ProcessSandboxPreset::DeveloperToolchains,
            ]),
            read_roots: vec!["/opt/sdk".to_string()],
            write_roots: vec!["/workspace/.cache".to_string()],
            ..Default::default()
        },
        process_network_proxy: None,
    }
}

fn intent() -> RunIntent {
    RunIntent {
        intent_id: "steel-thread".to_string(),
        capability_policy: capability_policy(),
        network: vec![network("api.example.test")],
        secrets: vec![provider_secret("openai")],
        admitted_environment: vec!["PATH".to_string()],
        process_sockets: Vec::new(),
        mcp: Vec::new(),
        toolchain_probes: Vec::new(),
        identity_brokers: Vec::new(),
        budget: RunBudget {
            spend_microusd: Some(25_000),
            time_ms: Some(30_000),
            turns: Some(8),
        },
        provenance: provenance(),
        interactivity: RunInteractivity::NonInteractive,
        startup_deadline_at_ms: DEADLINE_MS,
        receipt_uri: ".harn/receipts/steel-thread.ndjson".to_string(),
    }
}

fn approval_policy() -> ToolApprovalPolicy {
    serde_json::from_value(json!({
        "allow_sensitive_paths": true,
        "allow_external_paths": true,
        "rules": [{
            "id": "review-prepared-run",
            "action": "ask",
            "match": {"tool": "prepared_run.*"},
            "reason": "review the complete prepared authority envelope",
            "approval": {"risk": "prepared_run"}
        }]
    }))
    .expect("approval policy")
}

fn run_approval_policy(
    interactivity: RunInteractivity,
    approval_availability: ApprovalAvailability,
    workspace_trust: WorkspaceTrust,
) -> RunApprovalPolicy {
    RunApprovalPolicy::construct(
        RunAuthorityPosture {
            interactivity,
            approval_availability,
            workspace_trust,
        },
        |_| approval_policy(),
    )
}

fn net_policy() -> NetPolicy {
    NetPolicy {
        allow: Arc::new(vec![NetPolicyRule::parse_host(
            "api.example.test",
            Some(vec![443]),
        )
        .expect("network rule")]),
        deny: Arc::new(Vec::new()),
        default: NetPolicyDefault::Deny,
        on_violation: OnViolation::Error,
    }
}

fn host_facts() -> HostFacts {
    HostFacts {
        capability_ceiling: capability_policy(),
        approval_policy: run_approval_policy(
            RunInteractivity::NonInteractive,
            ApprovalAvailability::Available,
            WorkspaceTrust::Trusted,
        ),
        approved_batches: BTreeMap::new(),
        net_policy: net_policy(),
        secret_bindings: BTreeSet::from([provider_secret("openai")]),
        secret_brokers: BTreeMap::from([(
            SecretSourceKind::ProcessLocal,
            SecretBrokerFacts {
                outside_sandbox: true,
                supports_non_interactive: true,
                may_prompt_gui: false,
                zeroizing_handles: true,
            },
        )]),
        admitted_environment: BTreeSet::from(["PATH".to_string()]),
        process_sockets: BTreeSet::new(),
        mcp: BTreeSet::new(),
        toolchain_probes: BTreeSet::new(),
        identity_brokers: BTreeMap::new(),
        budget_ceiling: RunBudget {
            spend_microusd: Some(50_000),
            time_ms: Some(60_000),
            turns: Some(12),
        },
        provenance: provenance(),
    }
}

fn executor_requirements() -> Vec<AuthorityRequirement> {
    vec![
        AuthorityRequirement::FilesystemWrite {
            root: "/workspace".to_string(),
        },
        AuthorityRequirement::ProcessSandbox {
            profile: "worktree".to_string(),
            preset: "developer_toolchains".to_string(),
        },
        AuthorityRequirement::Network(network("api.example.test")),
        AuthorityRequirement::Secret(provider_secret("openai")),
    ]
}

fn prepared<E: PreparedRunExecutor>(
    executor: E,
) -> (
    PreparedRun<E>,
    Box<AuthorityLease>,
    Arc<MemoryAuthorityReceiptSink>,
) {
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let run = PreparedRun::with_clock(executor, receipts.clone(), Arc::new(|| NOW_MS));
    let batch = match run.prepare(intent(), host_facts()) {
        PreparationOutcome::NeedsApproval {
            batched_requests, ..
        } => batched_requests,
        other => panic!("expected one approval batch, got {other:?}"),
    };
    assert!(
        batch.groups.len() >= 5,
        "semantic grouping must preserve reviewable authority families"
    );
    let mut approved_host = host_facts();
    approved_host
        .approved_batches
        .insert(batch.batch_fingerprint, AuthorityDecider::Person);
    let lease = match run.prepare(intent(), approved_host) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("approved batch must create one lease, got {other:?}"),
    };
    (run, lease, receipts)
}

fn approved_lease<E>(
    run: &PreparedRun<E>,
    intent: &RunIntent,
    host: &HostFacts,
) -> Box<AuthorityLease> {
    let batch = match run.prepare(intent.clone(), host.clone()) {
        PreparationOutcome::NeedsApproval {
            batched_requests, ..
        } => batched_requests,
        other => panic!("expected approval batch, got {other:?}"),
    };
    let mut approved = host.clone();
    approved
        .approved_batches
        .insert(batch.batch_fingerprint, AuthorityDecider::Person);
    match run.prepare(intent.clone(), approved) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("approved plan must be ready, got {other:?}"),
    }
}

fn unattended_policy(workspace_trust: WorkspaceTrust) -> RunApprovalPolicy {
    RunApprovalPolicy::construct(
        RunAuthorityPosture {
            interactivity: RunInteractivity::NonInteractive,
            approval_availability: ApprovalAvailability::Unavailable,
            workspace_trust,
        },
        |posture| {
            let rules = if posture.workspace_trust.permits_project_policy() {
                json!([])
            } else {
                json!([{
                    "id": "workspace-untrusted-read-only",
                    "action": "deny",
                    "match": {"tool": "prepared_run.filesystem"},
                    "reason": "workspace is untrusted"
                }])
            };
            serde_json::from_value(json!({
                "allow_sensitive_paths": true,
                "allow_external_paths": true,
                "rules": rules
            }))
            .expect("unattended policy")
        },
    )
}

#[tokio::test]
async fn host_materialized_workspace_is_ready_without_path_inference_or_approval() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: vec![AuthorityRequirement::FilesystemWrite {
                root: "/workspace".to_string(),
            }],
            model_calls: model_calls.clone(),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    );

    let mut materialized_host = host_facts();
    materialized_host.approval_policy = unattended_policy(WorkspaceTrust::HostMaterialized);
    let lease = match run.prepare(intent(), materialized_host) {
        PreparationOutcome::Ready {
            authority_lease,
            receipt,
        } => {
            assert!(receipt
                .policy_decisions
                .iter()
                .all(|decision| { decision.policy_decision["action"] == "allow" }));
            authority_lease
        }
        other => panic!("declared host-materialized workspace must be ready, got {other:?}"),
    };
    assert_eq!(
        lease.posture().workspace_trust,
        WorkspaceTrust::HostMaterialized
    );
    match run.execute(lease).await {
        ExecutionOutcome::Completed { .. } => {}
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("materialized run failed: {error}")
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);

    let mut untrusted_host = host_facts();
    untrusted_host.approval_policy = unattended_policy(WorkspaceTrust::Untrusted);
    match run.prepare(intent(), untrusted_host) {
        PreparationOutcome::Blocked { diagnostics, .. } => assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "policy_denied"
                && diagnostic.message == "workspace is untrusted")),
        other => panic!("untrusted control must still block, got {other:?}"),
    }
}

#[test]
fn unavailable_noninteractive_asks_are_decided_during_policy_construction() {
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: Arc::new(AtomicUsize::new(0)),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    );
    let mut host = host_facts();
    host.approval_policy = run_approval_policy(
        RunInteractivity::NonInteractive,
        ApprovalAvailability::Unavailable,
        WorkspaceTrust::Trusted,
    );

    match run.prepare(intent(), host) {
        PreparationOutcome::Blocked { diagnostics, .. } => {
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "policy_denied"
                    && diagnostic.message.starts_with("approval unavailable:")
            }));
            assert!(!diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "approval_unavailable"));
        }
        other => panic!("unsatisfiable asks must be resolved to policy denials, got {other:?}"),
    }
}

#[test]
fn prepared_run_rejects_policy_constructed_for_another_interactivity_posture() {
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: Arc::new(AtomicUsize::new(0)),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    );
    let mut host = host_facts();
    host.approval_policy = run_approval_policy(
        RunInteractivity::Interactive,
        ApprovalAvailability::Available,
        WorkspaceTrust::Trusted,
    );

    match run.prepare(intent(), host) {
        PreparationOutcome::Blocked { diagnostics, .. } => assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "approval_policy_posture_mismatch")),
        other => panic!("mismatched construction posture must block, got {other:?}"),
    }
}

#[tokio::test]
async fn steel_thread_batches_once_executes_without_prompts_and_receipts_unused_authority() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let executor = FixtureExecutor {
        requirements: executor_requirements(),
        model_calls: model_calls.clone(),
    };
    let (run, lease, receipts) = prepared(executor);
    let plan_fingerprint = lease.plan_fingerprint().to_string();
    let lease_fingerprint = lease.fingerprint().to_string();

    let terminal = match run.execute(lease).await {
        ExecutionOutcome::Completed { output, receipt } => {
            assert_eq!(output, "completed");
            receipt
        }
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("steel thread failed: {error}")
        }
    };

    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(terminal.plan_fingerprint, plan_fingerprint);
    assert_eq!(
        terminal.lease_fingerprint.as_deref(),
        Some(lease_fingerprint.as_str())
    );
    assert_eq!(terminal.used.len(), 4);
    assert!(!terminal.unused.is_empty());
    assert!(terminal.denied.is_empty());
    assert!(terminal.executor_invoked);
    let persisted = receipts.receipts();
    assert_eq!(persisted[0].stage, AuthorityReceiptStage::Startup);
    assert!(!persisted[0].executor_invoked);
    assert_eq!(
        persisted.last().expect("terminal").stage,
        AuthorityReceiptStage::Terminal
    );
    let serialized = serde_json::to_string(&persisted).expect("receipts serialize");
    assert!(!serialized.contains(SECRET_CANARY));
}

#[tokio::test]
async fn changed_destination_path_and_secret_consumer_block_before_model_spend() {
    for changed in [
        AuthorityRequirement::Network(network("other.example.test")),
        AuthorityRequirement::FilesystemWrite {
            root: "/other-workspace".to_string(),
        },
        AuthorityRequirement::Secret(provider_secret("anthropic")),
    ] {
        let model_calls = Arc::new(AtomicUsize::new(0));
        let executor = FixtureExecutor {
            requirements: vec![changed],
            model_calls: model_calls.clone(),
        };
        let (run, lease, _) = prepared(executor);
        let receipt = match run.execute(lease).await {
            ExecutionOutcome::ExecutorFailed { receipt, .. } => receipt,
            ExecutionOutcome::AuthorityFailed { .. } => {
                panic!("executor denial must remain an executor failure")
            }
            ExecutionOutcome::Completed { .. } => panic!("changed authority must fail"),
        };
        assert_eq!(model_calls.load(Ordering::SeqCst), 0);
        assert_eq!(receipt.denied.len(), 1);
        assert!(receipt.denied[0]
            .reason
            .contains("outside the fingerprinted authority lease"));
    }
}

#[tokio::test]
async fn narrower_path_use_is_inside_the_granted_envelope_without_another_prompt() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let executor = FixtureExecutor {
        requirements: vec![AuthorityRequirement::FilesystemWrite {
            root: "/workspace/src".to_string(),
        }],
        model_calls: model_calls.clone(),
    };
    let (run, lease, _) = prepared(executor);
    let terminal = match run.execute(lease).await {
        ExecutionOutcome::Completed { receipt, .. } => receipt,
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("narrow path failed: {error}")
        }
    };
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(terminal.used.len(), 1);
    assert!(terminal.denied.is_empty());
}

#[tokio::test]
async fn read_use_attenuates_existing_write_root_grants() {
    let requirements = vec![
        AuthorityRequirement::FilesystemRead {
            root: "/workspace/src".to_string(),
        },
        AuthorityRequirement::ProcessReadRoot {
            root: "/workspace/.cache/tool".to_string(),
        },
    ];
    let mut run_intent = intent();
    run_intent.capability_policy.process_sandbox.write_roots =
        vec!["/workspace/.cache".to_string()];
    let mut host = host_facts();
    host.capability_ceiling.process_sandbox.write_roots = vec!["/workspace/.cache".to_string()];
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let executor = FixtureExecutor {
        requirements,
        model_calls: Arc::new(AtomicUsize::new(0)),
    };
    let run = PreparedRun::with_clock(executor, receipts, Arc::new(|| NOW_MS));
    let batch = match run.prepare(run_intent.clone(), host.clone()) {
        PreparationOutcome::NeedsApproval {
            batched_requests, ..
        } => batched_requests,
        other => panic!("expected approval, got {other:?}"),
    };
    host.approved_batches
        .insert(batch.batch_fingerprint, AuthorityDecider::Person);
    let lease = match run.prepare(run_intent, host) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("expected ready, got {other:?}"),
    };
    match run.execute(lease).await {
        ExecutionOutcome::Completed { receipt, .. } => {
            assert_eq!(receipt.used.len(), 2);
            assert!(receipt.denied.is_empty());
        }
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("execution failed: {error}")
        }
    }
}

#[tokio::test]
async fn lease_expiry_is_rechecked_at_each_material_operation() {
    let clock = Arc::new(AtomicU64::new(NOW_MS));
    let model_calls = Arc::new(AtomicUsize::new(0));
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let executor = ExpiringExecutor {
        clock: clock.clone(),
        model_calls: model_calls.clone(),
    };
    let now_ms = { Arc::new(move || clock.load(Ordering::SeqCst)) };
    let run = PreparedRun::with_clock(executor, receipts, now_ms);
    let batch = match run.prepare(intent(), host_facts()) {
        PreparationOutcome::NeedsApproval {
            batched_requests, ..
        } => batched_requests,
        other => panic!("expected approval, got {other:?}"),
    };
    let mut host = host_facts();
    host.approved_batches
        .insert(batch.batch_fingerprint, AuthorityDecider::Person);
    let lease = match run.prepare(intent(), host) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("expected ready, got {other:?}"),
    };
    let receipt = match run.execute(lease).await {
        ExecutionOutcome::ExecutorFailed { receipt, .. } => receipt,
        ExecutionOutcome::AuthorityFailed { .. } => {
            panic!("mid-execution expiry must remain an executor failure")
        }
        ExecutionOutcome::Completed { .. } => panic!("expired lease must fail"),
    };
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    assert_eq!(receipt.used.len(), 1);
    assert_eq!(receipt.denied.len(), 1);
    assert!(receipt.denied[0].reason.contains("expired"));
}

#[test]
fn stale_runtime_blocks_during_preparation_before_executor_or_model() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let executor = FixtureExecutor {
        requirements: executor_requirements(),
        model_calls: model_calls.clone(),
    };
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let run = PreparedRun::with_clock(executor, receipts, Arc::new(|| NOW_MS));
    let mut stale_host = host_facts();
    stale_host.provenance.runtime_digest = "sha256:stale-runtime".to_string();
    match run.prepare(intent(), stale_host) {
        PreparationOutcome::Blocked { diagnostics, .. } => assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "runtime_provenance_mismatch")),
        other => panic!("stale runtime must block, got {other:?}"),
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn capability_root_outside_the_host_ceiling_blocks_during_preparation() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let executor = FixtureExecutor {
        requirements: executor_requirements(),
        model_calls: model_calls.clone(),
    };
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let run = PreparedRun::with_clock(executor, receipts, Arc::new(|| NOW_MS));
    let mut intent = intent();
    intent.capability_policy.workspace_roots = vec!["/outside".to_string()];
    match run.prepare(intent, host_facts()) {
        PreparationOutcome::Blocked { diagnostics, .. } => assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "capability_ceiling")),
        other => panic!("outside root must block, got {other:?}"),
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn noninteractive_gui_capable_keyring_is_structurally_rejected() {
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let executor = FixtureExecutor {
        requirements: executor_requirements(),
        model_calls: Arc::new(AtomicUsize::new(0)),
    };
    let run = PreparedRun::with_clock(executor, receipts, Arc::new(|| NOW_MS));
    let mut host = host_facts();
    host.secret_brokers.insert(
        SecretSourceKind::ProcessLocal,
        SecretBrokerFacts {
            outside_sandbox: true,
            supports_non_interactive: false,
            may_prompt_gui: true,
            zeroizing_handles: false,
        },
    );
    match run.prepare(intent(), host) {
        PreparationOutcome::Blocked { diagnostics, .. } => assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "gui_keyring_forbidden")),
        other => panic!("GUI-capable keyring must block, got {other:?}"),
    }
}

#[test]
fn policy_denial_is_attributed_before_endpoint_health_or_eperm() {
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let executor = FixtureExecutor {
        requirements: executor_requirements(),
        model_calls: Arc::new(AtomicUsize::new(0)),
    };
    let run = PreparedRun::with_clock(executor, receipts, Arc::new(|| NOW_MS));
    let mut intent = intent();
    intent.network = vec![network("blocked.example.test")];
    match run.prepare(intent, host_facts()) {
        PreparationOutcome::Blocked { diagnostics, .. } => {
            let diagnostic = diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == "policy_denied")
                .expect("network policy diagnostic");
            assert!(diagnostic.message.contains("before endpoint health"));
            assert!(!diagnostic.message.contains("unreachable"));
        }
        other => panic!("network policy must block before I/O, got {other:?}"),
    }
}

#[test]
fn typed_delta_can_narrow_a_path_but_cannot_widen_the_parent() {
    let executor = FixtureExecutor {
        requirements: executor_requirements(),
        model_calls: Arc::new(AtomicUsize::new(0)),
    };
    let (run, lease, _) = prepared(executor);
    let narrow = AuthorityRequirement::FilesystemWrite {
        root: "/workspace/src".to_string(),
    };
    match run.request_delta(&lease, narrow.clone()) {
        LeaseDeltaOutcome::Attenuated(delta) => {
            assert_eq!(delta.parent_lease_fingerprint(), lease.fingerprint());
            assert_eq!(delta.requirement(), &narrow);
            assert_eq!(delta.expires_at_ms(), lease.expires_at_ms());
        }
        other => panic!("narrow path must yield a typed delta, got {other:?}"),
    }
    let wide = AuthorityRequirement::FilesystemWrite {
        root: "/".to_string(),
    };
    match run.request_delta(&lease, wide) {
        LeaseDeltaOutcome::Blocked(diagnostic) => {
            assert_eq!(diagnostic.code, "delta_widens_parent");
        }
        other => panic!("widening delta must block, got {other:?}"),
    }
}

#[test]
fn typed_delta_rejects_parent_traversal_that_lexically_starts_inside_the_grant() {
    let executor = FixtureExecutor {
        requirements: Vec::new(),
        model_calls: Arc::new(AtomicUsize::new(0)),
    };
    let (run, lease, _) = prepared(executor);
    match run.request_delta(
        &lease,
        AuthorityRequirement::FilesystemWrite {
            root: "/workspace/../etc".to_string(),
        },
    ) {
        LeaseDeltaOutcome::Blocked(diagnostic) => {
            assert_eq!(diagnostic.code, "delta_widens_parent");
        }
        other => panic!("parent traversal must not attenuate a path grant, got {other:?}"),
    }
}

#[tokio::test]
async fn ten_noninteractive_preparations_and_executions_complete() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..10 {
        let executor = FixtureExecutor {
            requirements: executor_requirements(),
            model_calls: model_calls.clone(),
        };
        let (run, lease, _) = prepared(executor);
        match run.execute(lease).await {
            ExecutionOutcome::Completed { .. } => {}
            ExecutionOutcome::ExecutorFailed { error, .. }
            | ExecutionOutcome::AuthorityFailed { error, .. } => {
                panic!("repeat failed: {error}")
            }
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn ndjson_sink_persists_startup_before_ready_and_terminal() {
    let directory = tempfile::tempdir().expect("receipt directory");
    let path = directory.path().join("authority.ndjson");
    let sink = Arc::new(NdjsonAuthorityReceiptSink::new(&path));
    let executor = FixtureExecutor {
        requirements: executor_requirements(),
        model_calls: Arc::new(AtomicUsize::new(0)),
    };
    let run = PreparedRun::with_clock(executor, sink, Arc::new(|| NOW_MS));
    let mut intent = intent();
    intent.receipt_uri = path.to_string_lossy().into_owned();
    let batch = match run.prepare(intent.clone(), host_facts()) {
        PreparationOutcome::NeedsApproval {
            batched_requests, ..
        } => batched_requests,
        other => panic!("expected approval, got {other:?}"),
    };
    let text = std::fs::read_to_string(&path).expect("startup receipts");
    let rows = text.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].contains(r#""stage":"startup""#));
    assert!(rows[1].contains(r#""stage":"needs_approval""#));
    assert!(!text.contains(SECRET_CANARY));

    let mut host = host_facts();
    host.approved_batches
        .insert(batch.batch_fingerprint, AuthorityDecider::Person);
    let lease = match run.prepare(intent, host) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("expected ready, got {other:?}"),
    };
    let _ = run.execute(lease).await;
    let text = std::fs::read_to_string(&path).expect("complete receipts");
    let rows = text.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 5);
    assert!(rows[2].contains(r#""stage":"startup""#));
    assert!(rows[3].contains(r#""stage":"ready""#));
    assert!(rows[4].contains(r#""stage":"terminal""#));
}

#[test]
fn versioned_plan_schema_accepts_the_canonical_plan_and_all_requirement_shapes() {
    let documentation_projection = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/schemas/run-authority-plan.v1.json"),
    )
    .expect("documentation schema projection");
    let documentation_schema: serde_json::Value =
        serde_json::from_str(&documentation_projection).expect("valid documentation schema");
    let packaged_schema: serde_json::Value =
        serde_json::from_str(RUN_AUTHORITY_PLAN_V1_SCHEMA_JSON).expect("valid packaged schema");
    assert_eq!(
        documentation_schema, packaged_schema,
        "documentation schema must remain a semantic projection of the packaged contract"
    );

    let mut intent = intent();
    intent.process_sockets = vec![ProcessSocketRequirement {
        socket_kind: ProcessSocketKind::Unix,
        endpoint: Some("/tmp/agent.sock".to_string()),
    }];
    intent.mcp = vec![McpRequirement {
        server: "repository".to_string(),
        tool: "read_issue".to_string(),
        side_effect: "read_only".to_string(),
    }];
    intent.toolchain_probes = vec![toolchain_probe("/toolchains")];
    intent.identity_brokers = vec![identity_requirement()];
    let mut host = host_facts();
    host.process_sockets = intent.process_sockets.iter().cloned().collect();
    host.mcp = intent.mcp.iter().cloned().collect();
    host.toolchain_probes = intent.toolchain_probes.iter().cloned().collect();
    host.identity_brokers = BTreeMap::from([(
        intent.identity_brokers[0].broker_id.clone(),
        identity_facts(&intent.identity_brokers[0]),
    )]);
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let executor = FixtureExecutor {
        requirements: Vec::new(),
        model_calls: Arc::new(AtomicUsize::new(0)),
    };
    let run = PreparedRun::with_clock(executor, receipts, Arc::new(|| NOW_MS));
    let batch = match run.prepare(intent.clone(), host.clone()) {
        PreparationOutcome::NeedsApproval {
            batched_requests, ..
        } => batched_requests,
        other => panic!("expected approval, got {other:?}"),
    };
    host.approved_batches
        .insert(batch.batch_fingerprint, AuthorityDecider::Person);
    let lease = match run.prepare(intent, host) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("expected ready plan, got {other:?}"),
    };
    let plan = lease.plan();
    let schema: serde_json::Value =
        serde_json::from_str(RUN_AUTHORITY_PLAN_V1_SCHEMA_JSON).expect("schema JSON");
    let validator = jsonschema::validator_for(&schema).expect("compile plan schema");
    let value = serde_json::to_value(plan).expect("serialize plan");
    let errors = validator
        .iter_errors(&value)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    assert!(errors.is_empty(), "plan schema violations: {errors:#?}");

    let serialized = serde_json::to_string(plan).expect("serialize plan text");
    assert!(!serialized.contains(SECRET_CANARY));
}

#[test]
fn secret_reference_type_rejects_raw_values_before_they_can_enter_a_plan() {
    assert!(SecretReference::parse(SECRET_CANARY).is_err());
    let decoded = serde_json::from_value::<SecretReference>(json!(SECRET_CANARY));
    assert!(decoded.is_err());
}

mod discovery_failures;

struct FixtureProbeRunner {
    result: ToolchainProbeResult,
    calls: Arc<AtomicUsize>,
}

#[test]
fn toolchain_probe_fixture_paths_are_native_absolute() {
    let probe = toolchain_probe("/toolchains");
    assert!(Path::new(&probe.working_directory).is_absolute());
    assert!(Path::new(&probe.read_root_ceiling).is_absolute());
}

impl ToolchainProbeRunner for FixtureProbeRunner {
    fn run(&self, _probe: &ToolchainProbeRequirement) -> Result<ToolchainProbeResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone())
    }
}

#[tokio::test]
async fn toolchain_discovery_runs_after_readiness_and_accounts_for_applied_delta() {
    let probe = toolchain_probe("/toolchains");
    let discovered = AuthorityRequirement::ProcessReadRoot {
        root: native_fixture_path("/toolchains/rust/stable"),
    };
    let model_calls = Arc::new(AtomicUsize::new(0));
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: vec![discovered.clone()],
            model_calls: model_calls.clone(),
        },
        receipts.clone(),
        Arc::new(|| NOW_MS),
    );
    let mut run_intent = intent();
    run_intent.toolchain_probes = vec![probe.clone()];
    let mut host = host_facts();
    host.toolchain_probes.insert(probe.clone());
    let mut lease = approved_lease(&run, &run_intent, &host);
    assert_eq!(
        receipts.receipts().last().map(|receipt| receipt.stage),
        Some(AuthorityReceiptStage::Ready),
        "the durable readiness receipt must exist before the runner can be invoked"
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let discovery = run.discover_toolchain(
        &mut lease,
        &probe,
        &FixtureProbeRunner {
            result: ToolchainProbeResult {
                discovered_read_roots: vec![native_fixture_path("/toolchains/rust/stable")],
                attempted_authority: Vec::new(),
            },
            calls: calls.clone(),
        },
    );
    let discovery_receipt = match discovery {
        ToolchainDiscoveryOutcome::Discovered { deltas, receipt } => {
            assert_eq!(deltas.len(), 1);
            receipt
        }
        other => panic!("narrow discovery must succeed, got {other:?}"),
    };
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(discovery_receipt.used.iter().any(|authority| {
        matches!(
            authority.requirement,
            AuthorityRequirement::ToolchainProbe(_)
        )
    }));
    assert!(discovery_receipt
        .unused
        .iter()
        .any(|authority| { authority.requirement == discovered }));

    let terminal = match run.execute(lease).await {
        ExecutionOutcome::Completed { receipt, .. } => receipt,
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("discovered root must execute: {error}")
        }
    };
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert!(terminal
        .used
        .iter()
        .any(|authority| authority.requirement == discovered));
    assert!(terminal.used.iter().any(|authority| {
        matches!(
            authority.requirement,
            AuthorityRequirement::ToolchainProbe(_)
        )
    }));
}

#[tokio::test]
async fn denied_toolchain_discovery_leaves_parent_lease_executable() {
    let probe = toolchain_probe("/toolchains");
    let model_calls = Arc::new(AtomicUsize::new(0));
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: model_calls.clone(),
        },
        receipts,
        Arc::new(|| NOW_MS),
    );
    let mut run_intent = intent();
    run_intent.toolchain_probes = vec![probe.clone()];
    let mut host = host_facts();
    host.toolchain_probes.insert(probe.clone());
    let mut lease = approved_lease(&run, &run_intent, &host);
    lease.approval_policy = unattended_policy(WorkspaceTrust::HostMaterialized);

    let outcome = run.discover_toolchain(
        &mut lease,
        &probe,
        &FixtureProbeRunner {
            result: ToolchainProbeResult {
                discovered_read_roots: vec![native_fixture_path("/etc")],
                attempted_authority: vec![AuthorityRequirement::Network(network(
                    "metadata.invalid",
                ))],
            },
            calls: Arc::new(AtomicUsize::new(0)),
        },
    );
    let receipt = match outcome {
        ToolchainDiscoveryOutcome::Blocked {
            diagnostics,
            receipt,
        } => {
            assert!(diagnostics
                .iter()
                .any(|item| item.code == "discovery_approval_unavailable"));
            receipt
        }
        other => panic!("malicious widening must block, got {other:?}"),
    };
    assert!(receipt.denied.iter().any(|denied| matches!(
        denied.authority.requirement,
        AuthorityRequirement::Network(_)
    )));
    assert!(receipt.denied.iter().any(|denied| matches!(
        denied.authority.requirement,
        AuthorityRequirement::ProcessReadRoot { .. }
    )));
    assert!(!serde_json::to_string(&receipt)
        .expect("serialize refusal receipt")
        .contains(SECRET_CANARY));
    match run.execute(lease).await {
        ExecutionOutcome::Completed { .. } => {}
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("refused optional discovery invalidated the parent lease: {error}")
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn interactive_toolchain_widening_returns_one_semantically_grouped_batch() {
    let probe = toolchain_probe("/toolchains");
    let model_calls = Arc::new(AtomicUsize::new(0));
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: model_calls.clone(),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    );
    let mut run_intent = intent();
    run_intent.interactivity = RunInteractivity::Interactive;
    run_intent.toolchain_probes = vec![probe.clone()];
    let mut host = host_facts();
    host.approval_policy = run_approval_policy(
        RunInteractivity::Interactive,
        ApprovalAvailability::Available,
        WorkspaceTrust::Trusted,
    );
    host.toolchain_probes.insert(probe.clone());
    let mut lease = approved_lease(&run, &run_intent, &host);

    match run.discover_toolchain(
        &mut lease,
        &probe,
        &FixtureProbeRunner {
            result: ToolchainProbeResult {
                discovered_read_roots: vec![native_fixture_path("/etc")],
                attempted_authority: vec![AuthorityRequirement::Network(network(
                    "metadata.invalid",
                ))],
            },
            calls: Arc::new(AtomicUsize::new(0)),
        },
    ) {
        ToolchainDiscoveryOutcome::NeedsApproval {
            batched_requests,
            receipt,
        } => {
            assert_eq!(batched_requests.groups.len(), 2);
            assert!(batched_requests
                .groups
                .iter()
                .any(|group| group.semantic_group == "process"));
            assert!(batched_requests
                .groups
                .iter()
                .any(|group| group.semantic_group == "network"));
            assert_eq!(receipt.status, AuthorityReceiptStatus::NeedsApproval);
            assert_eq!(receipt.denied.len(), 2);
        }
        other => panic!("interactive widening must return one batch, got {other:?}"),
    }
    match run.execute(lease).await {
        ExecutionOutcome::Completed { .. } => {}
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("pending optional discovery invalidated the parent lease: {error}")
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
struct RealRustcProbeRunner {
    receipts: Arc<MemoryAuthorityReceiptSink>,
}

#[cfg(unix)]
impl ToolchainProbeRunner for RealRustcProbeRunner {
    fn run(&self, probe: &ToolchainProbeRequirement) -> Result<ToolchainProbeResult, String> {
        assert_eq!(
            self.receipts.receipts().last().map(|receipt| receipt.stage),
            Some(AuthorityReceiptStage::Ready),
            "real subprocess reached before the readiness receipt"
        );
        let output = std::process::Command::new(&probe.executable)
            .args(&probe.arguments)
            .current_dir(&probe.working_directory)
            .output()
            .map_err(|error| format!("spawn rustc toolchain probe: {error}"))?;
        if !output.status.success() {
            return Err(format!("rustc toolchain probe exited {}", output.status));
        }
        let root = String::from_utf8(output.stdout)
            .map_err(|error| format!("rustc toolchain probe output: {error}"))?
            .trim()
            .to_string();
        Ok(ToolchainProbeResult {
            discovered_read_roots: vec![root],
            attempted_authority: Vec::new(),
        })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn real_rustc_toolchain_probe_runs_on_macos_and_linux_after_readiness() {
    let cwd = std::env::current_dir()
        .expect("current directory")
        .to_string_lossy()
        .into_owned();
    let probe = ToolchainProbeRequirement {
        probe_id: "real-rust-sysroot".to_string(),
        executable: std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string()),
        arguments: vec!["--print".to_string(), "sysroot".to_string()],
        working_directory: cwd,
        read_root_ceiling: "/".to_string(),
    };
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: Arc::new(AtomicUsize::new(0)),
        },
        receipts.clone(),
        Arc::new(|| NOW_MS),
    );
    let mut run_intent = intent();
    run_intent.toolchain_probes = vec![probe.clone()];
    let mut host = host_facts();
    host.toolchain_probes.insert(probe.clone());
    let mut lease = approved_lease(&run, &run_intent, &host);
    match run.discover_toolchain(&mut lease, &probe, &RealRustcProbeRunner { receipts }) {
        ToolchainDiscoveryOutcome::Discovered { deltas, receipt } => {
            assert_eq!(deltas.len(), 1);
            assert!(receipt.granted.iter().any(|authority| matches!(
                &authority.requirement,
                AuthorityRequirement::ProcessReadRoot { root } if std::path::Path::new(root).is_absolute()
            )));
        }
        other => panic!("real rustc discovery must succeed, got {other:?}"),
    }
}

#[derive(Clone)]
struct FixtureIdentityBroker {
    requirement: IdentityBrokerRequirement,
}

#[async_trait::async_trait]
impl ConsumerBoundIdentityBroker for FixtureIdentityBroker {
    fn facts(&self) -> IdentityBrokerFacts {
        identity_facts(&self.requirement)
    }

    async fn acquire(
        &self,
        requirement: &IdentityBrokerRequirement,
    ) -> Result<OpaqueIdentityHandle, IdentityBrokerError> {
        if requirement != &self.requirement {
            return Err(IdentityBrokerError::new(
                "identity_binding_mismatch",
                "fixture broker binding mismatch",
            ));
        }
        Ok(OpaqueIdentityHandle::new(
            requirement,
            crate::secrets::SecretBytes::from(SECRET_CANARY),
            Some(DEADLINE_MS),
        ))
    }
}

struct IdentityConsumingExecutor {
    requirement: IdentityBrokerRequirement,
    model_calls: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct MaterialIdentityBroker {
    requirement: IdentityBrokerRequirement,
    material: &'static str,
}

struct SequencedIdentityBroker {
    facts: IdentityBrokerFacts,
    acquisitions: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ConsumerBoundIdentityBroker for SequencedIdentityBroker {
    fn facts(&self) -> IdentityBrokerFacts {
        self.facts.clone()
    }

    async fn acquire(
        &self,
        requirement: &IdentityBrokerRequirement,
    ) -> Result<OpaqueIdentityHandle, IdentityBrokerError> {
        let attempt = self.acquisitions.fetch_add(1, Ordering::SeqCst);
        Ok(OpaqueIdentityHandle::new(
            requirement,
            crate::secrets::SecretBytes::from(SECRET_CANARY),
            Some(if attempt == 0 {
                NOW_MS - 1
            } else {
                DEADLINE_MS
            }),
        ))
    }
}

#[async_trait::async_trait]
impl ConsumerBoundIdentityBroker for MaterialIdentityBroker {
    fn facts(&self) -> IdentityBrokerFacts {
        identity_facts(&self.requirement)
    }

    async fn acquire(
        &self,
        requirement: &IdentityBrokerRequirement,
    ) -> Result<OpaqueIdentityHandle, IdentityBrokerError> {
        Ok(OpaqueIdentityHandle::new(
            requirement,
            crate::secrets::SecretBytes::from(self.material),
            Some(DEADLINE_MS),
        ))
    }
}

enum ProviderIdentityExecutor {
    Bedrock,
    Vertex,
}

#[async_trait::async_trait]
impl PreparedRunExecutor for ProviderIdentityExecutor {
    type Output = String;
    type Error = String;

    async fn execute(&self, _authority: &AuthorityUse) -> Result<Self::Output, Self::Error> {
        match self {
            Self::Bedrock => crate::llm::providers::bedrock::resolve_bedrock_credentials(
                "us-east-1",
                "https://bedrock.local",
            )
            .await
            .map(|credentials| credentials.access_key_id)
            .map_err(|error| error.to_string()),
            Self::Vertex => crate::llm::providers::vertex::resolve_vertex_token("", "project-a")
                .await
                .map_err(|error| error.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl PreparedRunExecutor for IdentityConsumingExecutor {
    type Output = usize;
    type Error = String;

    async fn execute(&self, _authority: &AuthorityUse) -> Result<Self::Output, Self::Error> {
        let material_len = consume_provider_identity(
            &self.requirement.binding.provider,
            &self.requirement.binding.audience,
            self.requirement.binding.tenant.as_deref(),
            |material| Ok(material.as_ref().len()),
        )
        .await
        .map_err(|error| error.code)?
        .ok_or_else(|| "prepared_identity_context_missing".to_string())?;
        self.model_calls.fetch_add(1, Ordering::SeqCst);
        Ok(material_len)
    }
}

#[tokio::test]
async fn identity_broker_is_readiness_bound_and_handle_is_opaque_and_consumer_exact() {
    let identity = identity_requirement();
    let mut run_intent = intent();
    run_intent.identity_brokers = vec![identity.clone()];
    let mut host = host_facts();
    host.identity_brokers
        .insert(identity.broker_id.clone(), identity_facts(&identity));
    let model_calls = Arc::new(AtomicUsize::new(0));
    let broker = Arc::new(FixtureIdentityBroker {
        requirement: identity.clone(),
    });
    let mut brokers = IdentityBrokerRegistry::default();
    brokers.insert(identity.broker_id.clone(), broker.clone());
    let run = PreparedRun::with_clock(
        IdentityConsumingExecutor {
            requirement: identity.clone(),
            model_calls: model_calls.clone(),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    )
    .with_identity_brokers(brokers, identity.binding.consumer.clone());
    let lease = approved_lease(&run, &run_intent, &host);
    assert_eq!(broker.facts(), identity_facts(&identity));
    match run.execute(lease).await {
        ExecutionOutcome::Completed { output, receipt } => {
            assert_eq!(output, SECRET_CANARY.len());
            assert!(receipt.used.iter().any(|authority| {
                authority.requirement == AuthorityRequirement::IdentityBroker(identity.clone())
            }));
            let serialized = serde_json::to_string(&receipt).expect("receipt JSON");
            assert!(!serialized.contains(SECRET_CANARY));
            assert!(serialized.contains("burin-workload"));
        }
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("identity run failed: {error}")
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bedrock_and_vertex_resolvers_consume_the_prepared_provider_identity() {
    let cases = [
        (
            "bedrock",
            "bedrock.local",
            None,
            PlatformIdentitySourceKind::SdkProfile,
            r#"{"access_key_id":"brokered-access","secret_access_key":"brokered-secret"}"#,
            "brokered-access",
            ProviderIdentityExecutor::Bedrock,
        ),
        (
            "vertex",
            "https://www.googleapis.com/auth/cloud-platform",
            Some("project-a"),
            PlatformIdentitySourceKind::WorkloadIdentity,
            "brokered-vertex-token",
            "brokered-vertex-token",
            ProviderIdentityExecutor::Vertex,
        ),
    ];
    for (provider, audience, tenant, source, material, expected, executor) in cases {
        let identity = IdentityBrokerRequirement {
            reference: PlatformIdentityReference::parse(&format!(
                "harn-identity://burin.provider_auth/{provider}"
            ))
            .expect("identity reference"),
            broker_id: format!("{provider}-broker"),
            source,
            renewal: IdentityRenewalMode::BrokerManaged,
            binding: IdentityBrokerBinding {
                provider: provider.to_string(),
                audience: audience.to_string(),
                tenant: tenant.map(str::to_string),
                consumer: SecretConsumerBinding {
                    kind: SecretConsumerKind::Provider,
                    id: provider.to_string(),
                    environment_name: None,
                },
            },
        };
        let mut run_intent = intent();
        run_intent.identity_brokers = vec![identity.clone()];
        let mut host = host_facts();
        host.identity_brokers
            .insert(identity.broker_id.clone(), identity_facts(&identity));
        let broker = Arc::new(MaterialIdentityBroker {
            requirement: identity.clone(),
            material,
        });
        let mut brokers = IdentityBrokerRegistry::default();
        brokers.insert(identity.broker_id.clone(), broker);
        let run = PreparedRun::with_clock(
            executor,
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        )
        .with_identity_brokers(brokers, identity.binding.consumer.clone());
        let lease = approved_lease(&run, &run_intent, &host);
        match run.execute(lease).await {
            ExecutionOutcome::Completed { output, receipt } => {
                assert_eq!(output, expected);
                assert_eq!(receipt.used.len(), 1);
                assert!(!serde_json::to_string(&receipt)
                    .expect("receipt JSON")
                    .contains(material));
            }
            ExecutionOutcome::ExecutorFailed { error, .. }
            | ExecutionOutcome::AuthorityFailed { error, .. } => {
                panic!("{provider} prepared identity failed: {error}")
            }
        }
    }
}

#[tokio::test]
async fn broker_managed_identity_renews_once_and_runtime_fact_drift_fails_closed() {
    let mut identity = identity_requirement();
    identity.source = PlatformIdentitySourceKind::InstanceMetadata;
    let mut run_intent = intent();
    run_intent.identity_brokers = vec![identity.clone()];
    let mut host = host_facts();
    host.identity_brokers
        .insert(identity.broker_id.clone(), identity_facts(&identity));

    let acquisitions = Arc::new(AtomicUsize::new(0));
    let mut brokers = IdentityBrokerRegistry::default();
    brokers.insert(
        identity.broker_id.clone(),
        Arc::new(SequencedIdentityBroker {
            facts: identity_facts(&identity),
            acquisitions: acquisitions.clone(),
        }),
    );
    let run = PreparedRun::with_clock(
        IdentityConsumingExecutor {
            requirement: identity.clone(),
            model_calls: Arc::new(AtomicUsize::new(0)),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    )
    .with_identity_brokers(brokers, identity.binding.consumer.clone());
    match run.execute(approved_lease(&run, &run_intent, &host)).await {
        ExecutionOutcome::Completed { receipt, .. } => {
            assert!(receipt.used.iter().any(|authority| authority.requirement
                == AuthorityRequirement::IdentityBroker(identity.clone())));
        }
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("broker-managed renewal failed: {error}")
        }
    }
    assert_eq!(acquisitions.load(Ordering::SeqCst), 2);

    let mut drifted_facts = identity_facts(&identity);
    drifted_facts
        .sources
        .remove(&PlatformIdentitySourceKind::InstanceMetadata);
    let model_calls = Arc::new(AtomicUsize::new(0));
    let mut drifted_brokers = IdentityBrokerRegistry::default();
    drifted_brokers.insert(
        identity.broker_id.clone(),
        Arc::new(SequencedIdentityBroker {
            facts: drifted_facts,
            acquisitions: Arc::new(AtomicUsize::new(0)),
        }),
    );
    let drifted_run = PreparedRun::with_clock(
        IdentityConsumingExecutor {
            requirement: identity.clone(),
            model_calls: model_calls.clone(),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    )
    .with_identity_brokers(drifted_brokers, identity.binding.consumer.clone());
    match drifted_run
        .execute(approved_lease(&drifted_run, &run_intent, &host))
        .await
    {
        ExecutionOutcome::ExecutorFailed { receipt, .. } => {
            assert!(receipt.used.is_empty());
            assert_eq!(receipt.denied.len(), 1);
        }
        ExecutionOutcome::Completed { .. } => panic!("metadata drift reached model spend"),
        ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("unexpected authority failure: {error}")
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn prepared_identity_missing_or_wrong_consumer_fails_before_model_spend() {
    for wrong_consumer in [false, true] {
        let identity = identity_requirement();
        let mut run_intent = intent();
        run_intent.identity_brokers = vec![identity.clone()];
        let mut host = host_facts();
        host.identity_brokers
            .insert(identity.broker_id.clone(), identity_facts(&identity));
        let model_calls = Arc::new(AtomicUsize::new(0));
        let mut brokers = IdentityBrokerRegistry::default();
        if wrong_consumer {
            brokers.insert(
                identity.broker_id.clone(),
                Arc::new(FixtureIdentityBroker {
                    requirement: identity.clone(),
                }),
            );
        }
        let consumer = if wrong_consumer {
            SecretConsumerBinding {
                id: "different-consumer".to_string(),
                ..identity.binding.consumer.clone()
            }
        } else {
            identity.binding.consumer.clone()
        };
        let run = PreparedRun::with_clock(
            IdentityConsumingExecutor {
                requirement: identity.clone(),
                model_calls: model_calls.clone(),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        )
        .with_identity_brokers(brokers, consumer);
        let lease = approved_lease(&run, &run_intent, &host);
        match run.execute(lease).await {
            ExecutionOutcome::ExecutorFailed { receipt, .. } => {
                assert!(receipt.used.is_empty());
                assert!(!receipt.denied.is_empty());
            }
            ExecutionOutcome::Completed { .. } => {
                panic!("prepared identity failure must not reach model spend")
            }
            ExecutionOutcome::AuthorityFailed { error, .. } => {
                panic!("provider identity refusal must remain an executor failure: {error}")
            }
        }
        assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn opaque_identity_handle_rejects_a_different_consumer_binding() {
    let identity = identity_requirement();
    let broker = FixtureIdentityBroker {
        requirement: identity.clone(),
    };
    let handle = broker.acquire(&identity).await.expect("opaque handle");
    let mut drifted = identity;
    drifted.binding.consumer.id = "different-consumer".to_string();
    let error = handle
        .consume(&drifted, NOW_MS, |_| ())
        .expect_err("a handle cannot cross consumer bindings");
    assert_eq!(error.code, "identity_binding_mismatch");
}

#[test]
fn identity_broker_host_properties_are_readiness_facts() {
    let identity = identity_requirement();
    let cases = [
        ("noninteractive", "identity_broker_interactivity"),
        ("gui", "identity_broker_interactivity"),
        ("sandbox", "identity_broker_sandbox_boundary"),
        ("opaque", "identity_broker_sandbox_boundary"),
        ("source", "identity_source_mismatch"),
        ("renewal", "identity_renewal_mismatch"),
    ];
    for (case, expected_code) in cases {
        let mut facts = identity_facts(&identity);
        match case {
            "noninteractive" => facts.supports_non_interactive = false,
            "gui" => facts.may_prompt_gui = true,
            "sandbox" => facts.material_outside_sandbox = false,
            "opaque" => facts.opaque_process_local_handles = false,
            "source" => facts.sources.clear(),
            "renewal" => facts.renewal_modes.clear(),
            _ => unreachable!(),
        }
        let mut run_intent = intent();
        run_intent.identity_brokers = vec![identity.clone()];
        let mut host = host_facts();
        host.identity_brokers
            .insert(identity.broker_id.clone(), facts);
        let run = PreparedRun::with_clock(
            FixtureExecutor {
                requirements: Vec::new(),
                model_calls: Arc::new(AtomicUsize::new(0)),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        );
        match run.prepare(run_intent, host) {
            PreparationOutcome::Blocked { diagnostics, .. } => assert!(
                diagnostics.iter().any(|item| item.code == expected_code),
                "{case} must emit {expected_code}: {diagnostics:#?}"
            ),
            other => panic!("{case} must block readiness, got {other:?}"),
        }
    }
}

#[test]
fn provider_audience_tenant_and_consumer_drift_each_block_before_spend() {
    let original = identity_requirement();
    for drift in ["provider", "audience", "tenant", "consumer"] {
        let mut changed = original.clone();
        match drift {
            "provider" => changed.binding.provider = "other-provider".to_string(),
            "audience" => changed.binding.audience = "https://other.invalid".to_string(),
            "tenant" => changed.binding.tenant = Some("tenant-b".to_string()),
            "consumer" => changed.binding.consumer.id = "other-consumer".to_string(),
            _ => unreachable!(),
        }
        let mut run_intent = intent();
        run_intent.identity_brokers = vec![changed];
        let mut host = host_facts();
        host.identity_brokers
            .insert(original.broker_id.clone(), identity_facts(&original));
        let model_calls = Arc::new(AtomicUsize::new(0));
        let run = PreparedRun::with_clock(
            FixtureExecutor {
                requirements: Vec::new(),
                model_calls: model_calls.clone(),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        );
        match run.prepare(run_intent, host) {
            PreparationOutcome::Blocked { diagnostics, .. } => assert!(diagnostics
                .iter()
                .any(|item| item.code == "identity_consumer_binding")),
            other => panic!("{drift} drift must block readiness, got {other:?}"),
        }
        assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn platform_identity_reference_rejects_values_and_ambiguous_paths() {
    assert!(PlatformIdentityReference::parse(SECRET_CANARY).is_err());
    assert!(PlatformIdentityReference::parse("harn-identity://tenant/name/extra").is_err());
    assert!(serde_json::from_value::<PlatformIdentityReference>(json!(SECRET_CANARY)).is_err());
}

fn prepared_session_binding() -> PreparedSessionBindingV1 {
    PreparedSessionBindingV1 {
        session_id: "prepared-session-1".to_string(),
        workspace_fingerprint: "blake3:workspace-a".to_string(),
        runtime: provenance(),
        consumer: SecretConsumerBinding {
            kind: SecretConsumerKind::Provider,
            id: "prepared-run".to_string(),
            environment_name: None,
        },
    }
}

fn prepared_runtime_attachment() -> PreparedRuntimeAttachment {
    PreparedRuntimeAttachment {
        session_id: "prepared-session-1".to_string(),
        workspace_fingerprint: "blake3:workspace-a".to_string(),
        runtime: provenance(),
        consumer: SecretConsumerBinding {
            kind: SecretConsumerKind::Provider,
            id: "prepared-run".to_string(),
            environment_name: None,
        },
    }
}

#[tokio::test]
async fn prepared_session_persists_one_approval_reuses_the_envelope_and_rejects_replay() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let claims = Arc::new(MemoryPreparedSessionLeaseStore::default());
    let host_session = PreparedSession::new(
        PreparedRun::with_clock(
            FixtureExecutor {
                requirements: executor_requirements(),
                model_calls: model_calls.clone(),
            },
            receipts.clone(),
            Arc::new(|| NOW_MS),
        ),
        claims.clone(),
    );
    let batch = match host_session.prepare(prepared_session_binding(), intent(), host_facts()) {
        PreparedSessionUpdate::NeedsApproval { batch, .. } => batch,
        other => panic!("interactive session must present one grouped batch, got {other:?}"),
    };
    let decision = PreparedSessionApprovalDecision {
        batch_fingerprint: batch.batch_fingerprint,
        approved: true,
        decider: AuthorityDecider::Person,
    };
    let lease = match host_session.decide("prepared-session-1", decision) {
        PreparedSessionUpdate::Ready { lease, .. } => *lease,
        other => panic!("approved session must become ready, got {other:?}"),
    };
    assert_eq!(lease.schema, PREPARED_SESSION_SCHEMA);
    let schema: serde_json::Value =
        serde_json::from_str(PREPARED_SESSION_V1_SCHEMA_JSON).expect("prepared-session schema");
    let validator = jsonschema::validator_for(&schema).expect("compile prepared-session schema");
    let lease_value = serde_json::to_value(&lease).expect("serialize prepared-session lease");
    let errors = validator
        .iter_errors(&lease_value)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "prepared-session lease schema violations: {errors:#?}"
    );
    assert!(receipts
        .receipts()
        .iter()
        .any(|receipt| receipt.stage == AuthorityReceiptStage::ApprovalDecision));

    // A separate state-machine instance models a long-lived external harn
    // serve process attaching from the persisted, value-free lease.
    let server_session = PreparedSession::new(
        PreparedRun::with_clock(
            FixtureExecutor {
                requirements: executor_requirements(),
                model_calls: model_calls.clone(),
            },
            receipts.clone(),
            Arc::new(|| NOW_MS),
        ),
        claims,
    );
    let active = server_session
        .attach(lease.clone(), host_facts(), prepared_runtime_attachment())
        .expect("external server accepts the exact prepared lease");
    assert_eq!(server_session.run_turn(&active).await.unwrap(), "completed");
    assert_eq!(server_session.run_turn(&active).await.unwrap(), "completed");
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);

    let replay = server_session.attach(lease, host_facts(), prepared_runtime_attachment());
    match replay {
        Err(PreparedSessionUpdate::Blocked { diagnostics, .. }) => assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "prepared_session_replay")),
        _ => panic!("a claimed prepared-session lease must reject replay"),
    }
    match server_session
        .finish(active, true)
        .expect("terminal receipt")
    {
        PreparedSessionUpdate::Terminal { receipt, .. } => {
            assert_eq!(receipt.status, AuthorityReceiptStatus::Completed);
            assert_eq!(receipt.used.len(), executor_requirements().len());
            assert!(receipt.unused.len() < receipt.granted.len());
        }
        other => panic!("finished session must emit terminal accounting, got {other:?}"),
    }
}

#[tokio::test]
async fn prepared_session_rejects_stale_runtime_and_cross_workspace_attach_before_turns() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let session = PreparedSession::new(
        PreparedRun::with_clock(
            FixtureExecutor {
                requirements: executor_requirements(),
                model_calls: model_calls.clone(),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        ),
        Arc::new(MemoryPreparedSessionLeaseStore::default()),
    );
    let mut approved_host = host_facts();
    let batch = match session.prepare(prepared_session_binding(), intent(), approved_host.clone()) {
        PreparedSessionUpdate::NeedsApproval { batch, .. } => batch,
        other => panic!("expected approval batch, got {other:?}"),
    };
    approved_host
        .approved_batches
        .insert(batch.batch_fingerprint.clone(), AuthorityDecider::Person);
    let lease = match session.decide(
        "prepared-session-1",
        PreparedSessionApprovalDecision {
            batch_fingerprint: batch.batch_fingerprint,
            approved: true,
            decider: AuthorityDecider::Person,
        },
    ) {
        PreparedSessionUpdate::Ready { lease, .. } => *lease,
        other => panic!("expected ready lease, got {other:?}"),
    };
    for drift in ["workspace", "runtime", "consumer"] {
        let mut attachment = prepared_runtime_attachment();
        if drift == "workspace" {
            attachment.workspace_fingerprint = "blake3:workspace-b".to_string();
        } else if drift == "runtime" {
            attachment.runtime.runtime_digest = "blake3:stale-runtime".to_string();
        } else {
            attachment.consumer.id = "wrong-provider".to_string();
        }
        match session.attach(lease.clone(), approved_host.clone(), attachment) {
            Err(PreparedSessionUpdate::Blocked { diagnostics, .. }) => assert!(diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "prepared_session_attachment_binding")),
            _ => panic!("{drift} drift must block before engine startup"),
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn prepared_session_widening_is_one_semantic_delta_batch() {
    let session = PreparedSession::new(
        PreparedRun::with_clock(
            FixtureExecutor {
                requirements: Vec::new(),
                model_calls: Arc::new(AtomicUsize::new(0)),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        ),
        Arc::new(MemoryPreparedSessionLeaseStore::default()),
    );
    let batch = match session.prepare(prepared_session_binding(), intent(), host_facts()) {
        PreparedSessionUpdate::NeedsApproval { batch, .. } => batch,
        other => panic!("expected initial approval batch, got {other:?}"),
    };
    let lease = match session.decide(
        "prepared-session-1",
        PreparedSessionApprovalDecision {
            batch_fingerprint: batch.batch_fingerprint,
            approved: true,
            decider: AuthorityDecider::Person,
        },
    ) {
        PreparedSessionUpdate::Ready { lease, .. } => *lease,
        other => panic!("expected ready lease, got {other:?}"),
    };
    let active = session
        .attach(lease, host_facts(), prepared_runtime_attachment())
        .expect("attach prepared session");
    let widened = AuthorityRequirement::Tool {
        pattern: "deploy".to_string(),
    };
    let delta_batch = match session.request_delta(&active, widened.clone()) {
        PreparedSessionUpdate::Delta {
            outcome: PreparedSessionDelta::NeedsApproval { batch },
            ..
        } => batch,
        other => panic!("widening must yield one approval batch, got {other:?}"),
    };
    assert_eq!(delta_batch.groups.len(), 1);
    assert_eq!(delta_batch.groups[0].semantic_group, "host_capabilities");
    assert!(active.authorize(&widened).is_err());
    match session.decide_delta(
        &active,
        PreparedSessionApprovalDecision {
            batch_fingerprint: delta_batch.batch_fingerprint,
            approved: true,
            decider: AuthorityDecider::Person,
        },
    ) {
        PreparedSessionUpdate::Delta {
            outcome: PreparedSessionDelta::Granted { .. },
            ..
        } => {}
        other => panic!("approved delta must join the active envelope, got {other:?}"),
    }
    active
        .authorize(&widened)
        .expect("approved widening is live without re-preparation");
}
