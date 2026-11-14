use std::sync::{mpsc, Arc};
use std::time::Duration;

use super::{
    next_progress_interval, CancelState, HandleStore, TerminalResult, TERMINAL_RESULT_CAPACITY,
};
use crate::process::ProcessCleanupReport;

#[test]
fn progress_backoff_doubles_then_clamps_to_max() {
    let max = Duration::from_secs(30);
    // Doubles from the base while below the cap.
    assert_eq!(
        next_progress_interval(Duration::from_secs(2), max),
        Duration::from_secs(4)
    );
    assert_eq!(
        next_progress_interval(Duration::from_secs(8), max),
        Duration::from_secs(16)
    );
    // Clamps to the cap once doubling would exceed it, and stays there.
    assert_eq!(next_progress_interval(Duration::from_secs(16), max), max);
    assert_eq!(next_progress_interval(max, max), max);
    // Saturates instead of overflowing at the Duration ceiling.
    assert_eq!(next_progress_interval(Duration::MAX, max), max);
}

#[test]
fn terminal_snapshot_waits_for_cleanup_publication() {
    let state = Arc::new(CancelState::default());
    let mut publication = state
        .begin_cancellation(true)
        .expect("fresh handle should accept cancellation");
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(1);
    let waiter_state = state.clone();
    let waiter = harn_parser::runtime_stack::spawn(move || {
        started_tx.send(()).expect("test waiter start receiver");
        snapshot_tx
            .send(waiter_state.complete_wait(None))
            .expect("test snapshot receiver");
    });

    started_rx.recv().expect("test waiter did not start");
    assert!(matches!(
        snapshot_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let mut report = ProcessCleanupReport::for_signal(Some(42), 9);
    report.census = harn_vm::op_interrupt::ProcessCleanupCensus {
        control_read: true,
        scanned_processes: 1,
        unreadable_processes: 0,
    };
    publication.record_cleanup(report);
    publication.publish_cancellation();
    drop(publication);

    let snapshot = snapshot_rx.recv().expect("waiter did not publish snapshot");
    waiter.join().expect("test waiter panicked");
    assert!(snapshot.cancelled);
    assert!(snapshot.timed_out);
    let report = snapshot
        .process_cleanup
        .expect("cleanup must publish with cancellation");
    assert_eq!(report.root_pid, Some(42));
    assert_eq!(
        report.survivor_count(),
        Some(0),
        "missing guardian receipt must preserve measured cancellation"
    );
    assert!(
        state.begin_cancellation(false).is_none(),
        "a completed terminal result must not be retroactively cancelled"
    );
}

#[test]
fn terminal_result_ledger_is_bounded_and_evicts_oldest() {
    let mut store = HandleStore::default();
    for index in 0..=TERMINAL_RESULT_CAPACITY {
        store.record_terminal_result(
            format!("handle-{index}"),
            TerminalResult {
                session_id: "session".to_string(),
                value: harn_vm::VmValue::Int(index as i64),
            },
        );
    }

    assert_eq!(store.terminal_results.len(), TERMINAL_RESULT_CAPACITY);
    assert!(store.terminal_result("handle-0").is_none());
    assert!(store
        .terminal_result(&format!("handle-{TERMINAL_RESULT_CAPACITY}"))
        .is_some());
}

#[test]
fn session_cleanup_removes_only_its_terminal_results() {
    let mut store = HandleStore::default();
    for (handle_id, session_id) in [("a", "ended"), ("b", "live"), ("c", "ended")] {
        store.record_terminal_result(
            handle_id.to_string(),
            TerminalResult {
                session_id: session_id.to_string(),
                value: harn_vm::VmValue::String(handle_id.into()),
            },
        );
    }

    store.remove_session_terminal_results("ended");

    assert!(store.terminal_result("a").is_none());
    assert!(store.terminal_result("c").is_none());
    assert!(store.terminal_result("b").is_some());
    assert_eq!(store.terminal_order.iter().collect::<Vec<_>>(), vec!["b"]);
}
