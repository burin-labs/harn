use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use crate::harness_net::{NetPolicy, NetPolicyDefault, NetPolicyRule, OnViolation};
use crate::orchestration::{
    CapabilityPolicy, ProcessSandboxPolicy, ProcessSandboxPreset, SandboxProfile,
    ToolApprovalPolicy,
};

use super::*;

const NOW_MS: u64 = 1_000;
const DEADLINE_MS: u64 = 61_000;
const SECRET_CANARY: &str = "secret-value-that-must-never-serialize";

#[derive(Clone)]
struct FixtureExecutor {
    requirements: Vec<AuthorityRequirement>,
    model_calls: Arc<AtomicUsize>,
}

impl PreparedRunExecutor for FixtureExecutor {
    type Output = &'static str;

    fn execute(&self, authority: &mut AuthorityUse<'_>) -> Result<Self::Output, String> {
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

impl PreparedRunExecutor for ExpiringExecutor {
    type Output = ();

    fn execute(&self, authority: &mut AuthorityUse<'_>) -> Result<Self::Output, String> {
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
        },
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
        approval_policy: approval_policy(),
        approval_availability: ApprovalAvailability::Available,
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

#[test]
fn steel_thread_batches_once_executes_without_prompts_and_receipts_unused_authority() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let executor = FixtureExecutor {
        requirements: executor_requirements(),
        model_calls: model_calls.clone(),
    };
    let (run, lease, receipts) = prepared(executor);
    let plan_fingerprint = lease.plan_fingerprint().to_string();
    let lease_fingerprint = lease.fingerprint().to_string();

    let terminal = match run.execute(lease) {
        ExecutionOutcome::Completed { output, receipt } => {
            assert_eq!(output, "completed");
            receipt
        }
        ExecutionOutcome::Failed { error, .. } => panic!("steel thread failed: {error}"),
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

#[test]
fn changed_destination_path_and_secret_consumer_block_before_model_spend() {
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
        let receipt = match run.execute(lease) {
            ExecutionOutcome::Failed { receipt, .. } => receipt,
            ExecutionOutcome::Completed { .. } => panic!("changed authority must fail"),
        };
        assert_eq!(model_calls.load(Ordering::SeqCst), 0);
        assert_eq!(receipt.denied.len(), 1);
        assert!(receipt.denied[0]
            .reason
            .contains("outside the fingerprinted authority lease"));
    }
}

#[test]
fn narrower_path_use_is_inside_the_granted_envelope_without_another_prompt() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let executor = FixtureExecutor {
        requirements: vec![AuthorityRequirement::FilesystemWrite {
            root: "/workspace/src".to_string(),
        }],
        model_calls: model_calls.clone(),
    };
    let (run, lease, _) = prepared(executor);
    let terminal = match run.execute(lease) {
        ExecutionOutcome::Completed { receipt, .. } => receipt,
        ExecutionOutcome::Failed { error, .. } => panic!("narrow path failed: {error}"),
    };
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(terminal.used.len(), 1);
    assert!(terminal.denied.is_empty());
}

#[test]
fn read_use_attenuates_existing_write_root_grants() {
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
    match run.execute(lease) {
        ExecutionOutcome::Completed { receipt, .. } => {
            assert_eq!(receipt.used.len(), 2);
            assert!(receipt.denied.is_empty());
        }
        ExecutionOutcome::Failed { error, .. } => panic!("execution failed: {error}"),
    }
}

#[test]
fn lease_expiry_is_rechecked_at_each_material_operation() {
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
    let receipt = match run.execute(lease) {
        ExecutionOutcome::Failed { receipt, .. } => receipt,
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
fn ten_noninteractive_preparations_and_executions_complete() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..10 {
        let executor = FixtureExecutor {
            requirements: executor_requirements(),
            model_calls: model_calls.clone(),
        };
        let (run, lease, _) = prepared(executor);
        match run.execute(lease) {
            ExecutionOutcome::Completed { .. } => {}
            ExecutionOutcome::Failed { error, .. } => panic!("repeat failed: {error}"),
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 10);
}

#[test]
fn ndjson_sink_persists_startup_before_ready_and_terminal() {
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
    let _ = run.execute(lease);
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
    let mut host = host_facts();
    host.process_sockets = intent.process_sockets.iter().cloned().collect();
    host.mcp = intent.mcp.iter().cloned().collect();
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
