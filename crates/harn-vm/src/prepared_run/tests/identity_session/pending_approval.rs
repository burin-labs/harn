use super::*;
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::task::Poll;

fn waiting_bridge(
    cancelled: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
) -> crate::bridge::HostBridge {
    let pending = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::<
        u64,
        tokio::sync::oneshot::Sender<serde_json::Value>,
    >::new()));
    let writer = Arc::new(move |line: &str| {
        let request: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(request["method"], "session/request_permission");
        assert_eq!(request["params"]["sessionId"], "prepared-session-1");
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let bridge = crate::bridge::HostBridge::from_parts_with_writer(pending, cancelled, writer, 1);
    bridge.set_session_id("prepared-session-1");
    bridge
}

fn fixture(
    sink: Arc<dyn AuthorityReceiptSink>,
) -> (PreparedSession<FixtureExecutor>, Arc<AtomicUsize>) {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let session = PreparedSession::new(
        PreparedRun::with_clock(
            FixtureExecutor {
                requirements: executor_requirements(),
                model_calls: model_calls.clone(),
            },
            sink,
            Arc::new(|| NOW_MS),
        ),
        Arc::new(MemoryPreparedSessionLeaseStore::default()),
    );
    (session, model_calls)
}

fn prepare(session: &PreparedSession<FixtureExecutor>, requested: RunIntent) -> ApprovalBatch {
    match session.prepare(prepared_session_binding(), requested, host_facts()) {
        PreparedSessionUpdate::NeedsApproval { batch, .. } => batch,
        other => panic!("expected real grouped approval, got {other:?}"),
    }
}

fn assert_retired(session: &PreparedSession<FixtureExecutor>, batch: ApprovalBatch) {
    match session.decide(
        "prepared-session-1",
        PreparedSessionApprovalDecision {
            batch_fingerprint: batch.batch_fingerprint,
            approved: true,
            decider: AuthorityDecider::Person,
        },
    ) {
        PreparedSessionUpdate::Blocked { diagnostics, .. } => assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "prepared_session_not_waiting")),
        other => panic!("retired preparation must not grant authority: {other:?}"),
    }
}

#[tokio::test]
async fn pending_approval_stop_and_dropped_future_persist_distinct_terminal_outcomes() {
    for stopped in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("authority.ndjson");
        let (session, model_calls) = fixture(Arc::new(NdjsonAuthorityReceiptSink::new(&path)));
        let mut requested = intent();
        requested.receipt_uri = path.to_string_lossy().into_owned();
        let batch = prepare(&session, requested);
        let cancelled = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let bridge = waiting_bridge(cancelled.clone(), calls.clone());
        let guard = session
            .pending_approval(&bridge, "prepared-session-1", &batch)
            .unwrap();
        let mut wait = Box::pin(guard.wait());
        std::future::poll_fn(|cx| {
            assert!(wait.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the real approval request must fire"
        );
        cancelled.store(stopped, Ordering::SeqCst);
        drop(wait);
        let receipts = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<RunAuthorityReceipt>(line).unwrap())
            .collect::<Vec<_>>();
        let terminal = receipts.last().unwrap();
        assert_eq!(
            terminal.stage,
            if stopped {
                AuthorityReceiptStage::Stopped
            } else {
                AuthorityReceiptStage::Terminal
            }
        );
        assert_eq!(
            terminal.status,
            if stopped {
                AuthorityReceiptStatus::Stopped
            } else {
                AuthorityReceiptStatus::Failed
            }
        );
        assert!(terminal.diagnostics.iter().any(|diagnostic| diagnostic.code
            == if stopped {
                "prepared_session_stop"
            } else {
                "prepared_session_approval_wait_lost"
            }));
        assert!(!terminal.executor_invoked);
        assert!(terminal.used.is_empty());
        assert_eq!(model_calls.load(Ordering::SeqCst), 0);
        assert_retired(&session, batch);
    }
}

#[test]
fn dropping_an_unpolled_old_wait_cannot_retire_an_identical_successor() {
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let (session, model_calls) = fixture(receipts.clone());
    let bridge = waiting_bridge(
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    );
    let earlier = prepare(&session, intent());
    let old_wait = session
        .pending_approval(&bridge, "prepared-session-1", &earlier)
        .unwrap()
        .wait();
    let successor = prepare(&session, intent());
    assert_eq!(
        earlier, successor,
        "identity must distinguish identical batches"
    );
    drop(old_wait);
    assert_eq!(
        receipts.receipts().last().unwrap().status,
        AuthorityReceiptStatus::Failed
    );
    let stopped = session
        .pending_approval(&bridge, "prepared-session-1", &successor)
        .unwrap()
        .stop()
        .unwrap();
    assert!(matches!(stopped, PreparedSessionUpdate::Stopped { .. }));
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    assert_retired(&session, successor);
}

struct RefuseTerminal {
    failed: AtomicUsize,
}

impl AuthorityReceiptSink for RefuseTerminal {
    fn persist(&self, receipt: &RunAuthorityReceipt) -> Result<(), String> {
        if matches!(
            receipt.stage,
            AuthorityReceiptStage::Terminal | AuthorityReceiptStage::Stopped
        ) {
            self.failed.fetch_add(1, Ordering::SeqCst);
            Err("terminal fixture persistence refused".to_string())
        } else {
            Ok(())
        }
    }
}

#[test]
fn pending_stop_reports_persistence_failure_without_leaving_a_grantable_request() {
    let sink = Arc::new(RefuseTerminal {
        failed: AtomicUsize::new(0),
    });
    let (session, model_calls) = fixture(sink.clone());
    let bridge = waiting_bridge(
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    );
    let batch = prepare(&session, intent());
    let error = session
        .pending_approval(&bridge, "prepared-session-1", &batch)
        .unwrap()
        .stop()
        .expect_err("persistence failure must reach the caller");
    assert_eq!(error, "terminal fixture persistence refused");
    assert_eq!(sink.failed.load(Ordering::SeqCst), 1);
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    assert_retired(&session, batch);
}

#[test]
fn dropped_approval_reports_terminal_persistence_failure_to_the_host() {
    let sink = Arc::new(RefuseTerminal {
        failed: AtomicUsize::new(0),
    });
    let (session, model_calls) = fixture(sink.clone());
    let messages = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = messages.clone();
    let bridge = crate::bridge::HostBridge::from_parts_with_writer(
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(move |line| {
            captured
                .lock()
                .unwrap()
                .push(serde_json::from_str(line).unwrap());
            Ok(())
        }),
        1,
    );
    bridge.set_session_id("prepared-session-1");
    let batch = prepare(&session, intent());
    let wait = session
        .pending_approval(&bridge, "prepared-session-1", &batch)
        .unwrap()
        .wait();
    drop(wait);
    assert_eq!(sink.failed.load(Ordering::SeqCst), 1);
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    assert_retired(&session, batch);
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "the failed sink must reach the host");
    assert_eq!(messages[0]["method"], "log");
    assert_eq!(messages[0]["params"]["level"], "error");
    assert_eq!(
        messages[0]["params"]["fields"]["code"],
        "prepared_session_pending_terminal_persistence"
    );
    assert_eq!(
        messages[0]["params"]["fields"]["session_id"],
        "prepared-session-1"
    );
    assert_eq!(
        messages[0]["params"]["fields"]["error"],
        "terminal fixture persistence refused"
    );
}
