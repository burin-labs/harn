use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

#[derive(Debug, PartialEq)]
struct StructuredExecutorFailure {
    input_tokens: u64,
    tool_events: Vec<&'static str>,
}

struct StructuredFailingExecutor;

#[async_trait::async_trait]
impl PreparedRunExecutor for StructuredFailingExecutor {
    type Output = ();
    type Error = StructuredExecutorFailure;

    async fn execute(&self, _authority: &AuthorityUse) -> Result<Self::Output, Self::Error> {
        Err(StructuredExecutorFailure {
            input_tokens: 1_024,
            tool_events: vec!["read", "test"],
        })
    }
}

#[derive(Default)]
struct ToggleTerminalReceiptSink {
    fail: AtomicBool,
}

impl AuthorityReceiptSink for ToggleTerminalReceiptSink {
    fn persist(&self, _receipt: &RunAuthorityReceipt) -> Result<(), String> {
        if self.fail.load(Ordering::SeqCst) {
            Err("fixture terminal receipt persistence failure".to_string())
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn structured_executor_failure_survives_the_prepared_run_boundary() {
    let (run, lease, _) = prepared(StructuredFailingExecutor);

    match run.execute(lease).await {
        ExecutionOutcome::ExecutorFailed { error, receipt } => {
            assert_eq!(
                error,
                StructuredExecutorFailure {
                    input_tokens: 1_024,
                    tool_events: vec!["read", "test"],
                }
            );
            assert_eq!(receipt.status, AuthorityReceiptStatus::Failed);
            assert!(receipt.executor_invoked);
        }
        ExecutionOutcome::Completed { .. } => panic!("failing executor must not complete"),
        ExecutionOutcome::AuthorityFailed { .. } => {
            panic!("executor evidence must not be projected as an authority failure")
        }
    }
}

#[tokio::test]
async fn terminal_receipt_failure_stays_separate_from_executor_failures() {
    let sink = Arc::new(ToggleTerminalReceiptSink::default());
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: Arc::new(AtomicUsize::new(0)),
        },
        sink.clone(),
        Arc::new(|| NOW_MS),
    );
    let lease = approved_lease(&run, &intent(), &host_facts());
    sink.fail.store(true, Ordering::SeqCst);

    match run.execute(lease).await {
        ExecutionOutcome::AuthorityFailed { error, receipt } => {
            assert_eq!(
                error,
                "execution completed but terminal authority receipt was not persisted"
            );
            assert!(receipt
                .diagnostics
                .iter()
                .any(|item| item.code == "terminal_receipt_persistence"));
        }
        ExecutionOutcome::Completed { .. } => {
            panic!("unpersisted terminal receipt must prevent completion")
        }
        ExecutionOutcome::ExecutorFailed { .. } => {
            panic!("receipt persistence must not fabricate an executor failure")
        }
    }

    let sink = Arc::new(ToggleTerminalReceiptSink::default());
    let run = PreparedRun::with_clock(StructuredFailingExecutor, sink.clone(), Arc::new(|| NOW_MS));
    let lease = approved_lease(&run, &intent(), &host_facts());
    sink.fail.store(true, Ordering::SeqCst);

    match run.execute(lease).await {
        ExecutionOutcome::ExecutorFailed { error, receipt } => {
            assert_eq!(error.input_tokens, 1_024);
            assert!(receipt
                .diagnostics
                .iter()
                .any(|item| item.code == "terminal_receipt_persistence"));
        }
        ExecutionOutcome::Completed { .. } => panic!("failing executor must not complete"),
        ExecutionOutcome::AuthorityFailed { .. } => {
            panic!("receipt failure must not erase the executor's concrete error")
        }
    }
}

#[tokio::test]
async fn lease_failure_before_dispatch_is_not_an_executor_failure() {
    let clock = Arc::new(AtomicU64::new(NOW_MS));
    let model_calls = Arc::new(AtomicUsize::new(0));
    let run = PreparedRun::with_clock(
        FixtureExecutor {
            requirements: Vec::new(),
            model_calls: model_calls.clone(),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        {
            let clock = clock.clone();
            Arc::new(move || clock.load(Ordering::SeqCst))
        },
    );
    let lease = approved_lease(&run, &intent(), &host_facts());
    clock.store(DEADLINE_MS + 1, Ordering::SeqCst);

    match run.execute(lease).await {
        ExecutionOutcome::AuthorityFailed { error, receipt } => {
            assert_eq!(error, "authority lease expired before execution");
            assert!(!receipt.executor_invoked);
        }
        ExecutionOutcome::Completed { .. } => panic!("expired lease must not execute"),
        ExecutionOutcome::ExecutorFailed { .. } => {
            panic!("an executor that was not invoked cannot own the failure")
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
}
