use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use super::*;

struct FailingProbeRunner(String);

impl ToolchainProbeRunner for FailingProbeRunner {
    fn run(&self, _probe: &ToolchainProbeRequirement) -> Result<ToolchainProbeResult, String> {
        Err(self.0.clone())
    }
}

#[derive(Default)]
struct ToggleReceiptSink {
    fail: AtomicBool,
    receipts: Mutex<Vec<RunAuthorityReceipt>>,
}

impl AuthorityReceiptSink for ToggleReceiptSink {
    fn persist(&self, receipt: &RunAuthorityReceipt) -> Result<(), String> {
        if self.fail.load(Ordering::SeqCst) {
            return Err("fixture receipt persistence failure".to_string());
        }
        self.receipts
            .lock()
            .expect("fixture receipt sink poisoned")
            .push(receipt.clone());
        Ok(())
    }
}

#[test]
fn missing_toolchain_discovery_leaves_parent_lease_executable() {
    failed_optional_discovery_leaves_parent_lease_executable(&format!(
        "toolchain executable was not found: {SECRET_CANARY}"
    ));
}

#[test]
fn malformed_toolchain_discovery_leaves_parent_lease_executable() {
    failed_optional_discovery_leaves_parent_lease_executable(&format!(
        "toolchain probe returned malformed output: {SECRET_CANARY}"
    ));
}

fn failed_optional_discovery_leaves_parent_lease_executable(error: &str) {
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
    run_intent.toolchain_probes = vec![probe.clone()];
    let mut host = host_facts();
    host.toolchain_probes.insert(probe.clone());
    let mut lease = approved_lease(&run, &run_intent, &host);

    match run.discover_toolchain(&mut lease, &probe, &FailingProbeRunner(error.to_string())) {
        ToolchainDiscoveryOutcome::Blocked {
            diagnostics,
            receipt,
        } => {
            assert_eq!(diagnostics[0].code, "toolchain_probe_failed");
            assert!(!format!("{diagnostics:?}").contains(SECRET_CANARY));
            assert!(!serde_json::to_string(&receipt)
                .expect("serialize discovery receipt")
                .contains(SECRET_CANARY));
        }
        other => panic!("failed optional probe must be receipted, got {other:?}"),
    }
    match run.execute(lease) {
        ExecutionOutcome::Completed { .. } => {}
        ExecutionOutcome::Failed { error, .. } => {
            panic!("failed optional discovery invalidated the parent lease: {error}")
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn expired_toolchain_discovery_invalidates_parent_lease() {
    let clock = Arc::new(AtomicU64::new(NOW_MS));
    let probe = toolchain_probe("/toolchains");
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: Arc::new(AtomicUsize::new(0)),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        {
            let clock = clock.clone();
            Arc::new(move || clock.load(Ordering::SeqCst))
        },
    );
    let mut run_intent = intent();
    run_intent.toolchain_probes = vec![probe.clone()];
    let mut host = host_facts();
    host.toolchain_probes.insert(probe.clone());
    let mut lease = approved_lease(&run, &run_intent, &host);
    clock.store(DEADLINE_MS + 1, Ordering::SeqCst);

    match run.discover_toolchain(
        &mut lease,
        &probe,
        &FailingProbeRunner("runner must not be invoked".to_string()),
    ) {
        ToolchainDiscoveryOutcome::Blocked { diagnostics, .. } => {
            assert_eq!(diagnostics[0].code, "toolchain_discovery_lease_expired");
        }
        other => panic!("expired discovery must block, got {other:?}"),
    }
    assert!(lease.invalidated.is_some());
}

#[test]
fn probe_outside_fingerprinted_lease_invalidates_parent_lease() {
    let probe = toolchain_probe("/toolchains");
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: Arc::new(AtomicUsize::new(0)),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    );
    let mut run_intent = intent();
    run_intent.toolchain_probes = vec![probe.clone()];
    let mut host = host_facts();
    host.toolchain_probes.insert(probe.clone());
    let mut lease = approved_lease(&run, &run_intent, &host);
    let mut unreviewed_probe = probe;
    unreviewed_probe.arguments.push("--unreviewed".to_string());

    match run.discover_toolchain(
        &mut lease,
        &unreviewed_probe,
        &FailingProbeRunner("runner must not be invoked".to_string()),
    ) {
        ToolchainDiscoveryOutcome::Blocked { diagnostics, .. } => {
            assert_eq!(diagnostics[0].code, "toolchain_probe_outside_lease");
        }
        other => panic!("unreviewed probe must block, got {other:?}"),
    }
    assert!(lease.invalidated.is_some());
}

#[test]
fn discovery_receipt_persistence_failure_invalidates_parent_lease() {
    let probe = toolchain_probe("/toolchains");
    let sink = Arc::new(ToggleReceiptSink::default());
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: Arc::new(AtomicUsize::new(0)),
        },
        sink.clone(),
        Arc::new(|| NOW_MS),
    );
    let mut run_intent = intent();
    run_intent.toolchain_probes = vec![probe.clone()];
    let mut host = host_facts();
    host.toolchain_probes.insert(probe.clone());
    let mut lease = approved_lease(&run, &run_intent, &host);
    sink.fail.store(true, Ordering::SeqCst);

    match run.discover_toolchain(
        &mut lease,
        &probe,
        &FixtureProbeRunner {
            result: ToolchainProbeResult::default(),
            calls: Arc::new(AtomicUsize::new(0)),
        },
    ) {
        ToolchainDiscoveryOutcome::Blocked { diagnostics, .. } => {
            assert_eq!(diagnostics[0].code, "discovery_receipt_persistence");
        }
        other => panic!("unreceipted discovery must block, got {other:?}"),
    }
    assert!(lease.invalidated.is_some());
}
