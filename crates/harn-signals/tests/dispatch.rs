use harn_signals::{
    Activity, CheckInScope, HarnessKind, Inbox, ManualEvent, Provenance, RoutingHints,
    SessionAnnouncement, Severity, SignalInput, TriageEvent,
};
use time::OffsetDateTime;

fn at(second: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000 + second).unwrap()
}
fn capture(inbox: &Inbox, session: &str, epoch: &str, monitor: &str, id: i64, second: i64) {
    let input = SignalInput {
        key: Some(monitor.into()),
        event: TriageEvent::manual(
            ManualEvent {
                provider: "fixture".into(),
                source_kind: "log".into(),
                source_url: "file:///fixture/log".into(),
                summary: format!("line {id}"),
                why_it_matters: "New output".into(),
                proposed_action: "Inspect output".into(),
                urgency: String::new(),
            },
            monitor,
        )
        .unwrap(),
        severity: Severity::Warning,
        routing: RoutingHints::default(),
        provenance: Provenance::local("fixture"),
        expires_at: None,
    };
    inbox
        .capture_execution(session, epoch, &id.to_string(), input, at(second))
        .unwrap();
}
fn open() -> (tempfile::TempDir, Inbox) {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    inbox.dispatch_start("A", "run1", None, at(0)).unwrap();
    (dir, inbox)
}
fn accept(inbox: &Inbox, second: i64) -> harn_signals::DispatchClaim {
    let claim = inbox
        .dispatch_claim("A", "run1", at(second))
        .unwrap()
        .claim
        .unwrap();
    assert!(inbox
        .dispatch_accept("A", "run1", &claim.batch.id, &claim.token, at(second))
        .unwrap());
    claim
}

#[test]
fn first_wake_is_delayed_and_all_monitors_share_one_ordered_batch() {
    let (_dir, inbox) = open();
    for i in 0..500 {
        capture(&inbox, "A", "run1", &format!("monitor{}", i % 10), i, 0);
    }
    for second in [0, 59] {
        let poll = inbox.dispatch_claim("A", "run1", at(second)).unwrap();
        assert!(poll.claim.is_none());
        assert_eq!(poll.next_due_at, Some(at(60)));
        assert_eq!(poll.pending, 500);
    }
    let claim = accept(&inbox, 60);
    assert_eq!(claim.batch.occurrences.len(), 500);
    assert_eq!(
        claim
            .batch
            .occurrences
            .iter()
            .map(|r| r.event.summary.clone())
            .collect::<Vec<_>>(),
        (0..500).map(|i| format!("line {i}")).collect::<Vec<_>>()
    );
    assert_eq!(
        inbox.dispatch_claim("A", "run1", at(120)).unwrap().pending,
        0
    );
}

#[test]
fn sustained_bursts_do_not_slide_deadlines_or_starve_trailing_output() {
    let (_dir, inbox) = open();
    let mut sizes = vec![];
    let mut batches = vec![];
    for second in 0..=300 {
        capture(&inbox, "A", "run1", "log", second, second);
        if second > 0 && second % 60 == 0 {
            let c = accept(&inbox, second);
            sizes.push(c.batch.occurrences.len());
            batches.push(c.batch.id);
        } else {
            assert!(inbox
                .dispatch_claim("A", "run1", at(second))
                .unwrap()
                .claim
                .is_none());
        }
    }
    capture(&inbox, "A", "run1", "log", 301, 301);
    assert!(inbox
        .dispatch_claim("A", "run1", at(359))
        .unwrap()
        .claim
        .is_none());
    sizes.push(accept(&inbox, 360).batch.occurrences.len());
    assert_eq!(sizes, [61, 60, 60, 60, 60, 1]);
    batches.sort();
    batches.dedup();
    assert_eq!(batches.len(), 5);
}

#[test]
fn busy_boundary_and_restart_preserve_deadlines_and_acceptance_pacing() {
    let (dir, inbox) = open();
    for second in [0, 30, 61] {
        capture(&inbox, "A", "run1", "log", second, second);
    }
    drop(inbox);
    let inbox = Inbox::open(dir.path()).unwrap();
    inbox.dispatch_start("A", "run1", None, at(90)).unwrap();
    assert_eq!(accept(&inbox, 90).batch.occurrences.len(), 3);
    capture(&inbox, "A", "run1", "log", 91, 91);
    assert_eq!(
        inbox
            .dispatch_claim("A", "run1", at(149))
            .unwrap()
            .next_due_at,
        Some(at(150))
    );
    assert_eq!(accept(&inbox, 150).batch.occurrences.len(), 1);
}

#[test]
fn concurrent_claimants_have_one_lease_and_restart_reuses_batch_identity() {
    let (dir, inbox) = open();
    capture(&inbox, "A", "run1", "log", 0, 0);
    let path = dir.path().to_path_buf();
    let a = std::thread::spawn(move || {
        Inbox::open(path)
            .unwrap()
            .dispatch_claim("A", "run1", at(60))
            .unwrap()
    });
    let b = inbox.dispatch_claim("A", "run1", at(60)).unwrap();
    let a = a.join().unwrap();
    assert_ne!(a.claim.is_some(), b.claim.is_some());
    let claim = a.claim.or(b.claim).unwrap();
    capture(&inbox, "A", "run1", "log", 1, 61);
    drop(inbox);
    let inbox = Inbox::open(dir.path()).unwrap();
    assert!(inbox
        .dispatch_claim("A", "run1", at(659))
        .unwrap()
        .claim
        .is_none());
    let retry = inbox
        .dispatch_claim("A", "run1", at(660))
        .unwrap()
        .claim
        .unwrap();
    assert_eq!(retry.batch, claim.batch);
    assert_ne!(retry.token, claim.token);
    assert!(inbox
        .dispatch_accept("A", "run1", &claim.batch.id, &claim.token, at(660))
        .is_err());
    assert!(inbox
        .dispatch_accept("A", "run1", &retry.batch.id, &retry.token, at(660))
        .unwrap());
    assert!(!inbox
        .dispatch_accept("A", "run1", &retry.batch.id, &retry.token, at(661))
        .unwrap());
    assert_eq!(
        inbox
            .dispatch_claim("A", "run1", at(719))
            .unwrap()
            .next_due_at,
        Some(at(720))
    );
    assert_eq!(accept(&inbox, 720).batch.occurrences.len(), 1);
}

#[test]
fn cancellation_is_terminal_for_monitor_and_epoch_without_mixing_sessions() {
    let (_dir, inbox) = open();
    inbox.dispatch_start("B", "run1", None, at(0)).unwrap();
    for (s, m) in [("A", "cancel"), ("A", "keep"), ("B", "cancel")] {
        capture(&inbox, s, "run1", m, 0, 0);
    }
    let claim = inbox
        .dispatch_claim("A", "run1", at(60))
        .unwrap()
        .claim
        .unwrap();
    assert_eq!(
        inbox
            .dispatch_cancel("A", "run1", Some("cancel"), at(60))
            .unwrap(),
        1
    );
    assert!(inbox
        .dispatch_accept("A", "run1", &claim.batch.id, &claim.token, at(60))
        .is_err());
    assert_eq!(accept(&inbox, 60).batch.occurrences.len(), 1);
    assert_eq!(
        inbox
            .dispatch_claim("B", "run1", at(60))
            .unwrap()
            .claim
            .unwrap()
            .batch
            .occurrences
            .len(),
        1
    );
    let saved = inbox.snapshot(at(60)).unwrap();
    let retry = saved
        .signals
        .values()
        .find(|s| {
            s.capture
                .as_ref()
                .is_some_and(|c| c.session_id == "A" && c.monitor_key == "cancel")
        })
        .unwrap();
    let input = SignalInput {
        key: Some("cancel".into()),
        event: retry.event.clone(),
        severity: retry.severity,
        routing: retry.routing.clone(),
        provenance: retry.provenance.clone(),
        expires_at: None,
    };
    assert!(inbox
        .capture_execution("A", "run1", "late", input.clone(), at(61))
        .is_err());
    inbox.dispatch_cancel("A", "run1", None, at(61)).unwrap();
    assert!(inbox.dispatch_start("A", "run1", None, at(62)).is_err());
    inbox.dispatch_start("A", "run2", None, at(62)).unwrap();
    assert!(inbox
        .capture_execution("A", "run1", "late", input, at(63))
        .is_err());
    capture(&inbox, "A", "run2", "fresh", 0, 63);
    assert_eq!(
        inbox
            .dispatch_claim("A", "run2", at(119))
            .unwrap()
            .next_due_at,
        Some(at(120))
    );
}

#[test]
fn ordinary_hooks_cannot_bypass_gate_and_pending_capture_does_not_expire() {
    let (_dir, inbox) = open();
    capture(&inbox, "A", "run1", "log", 0, 0);
    let checked = inbox
        .check_in(
            SessionAnnouncement {
                id: "A".into(),
                harness: HarnessKind::Other("fixture".into()),
                cwd: "/fixture".into(),
                repo: None,
                title: None,
                role: None,
                activity: Activity::Busy,
            },
            CheckInScope::All,
            None,
            at(1),
        )
        .unwrap();
    assert!(checked.reminders.is_empty());
    let delayed = accept(&inbox, 86401);
    assert_eq!(delayed.batch.occurrences.len(), 1);
}

#[test]
fn policy_and_clock_rollback_hold_without_dropping_capture() {
    let (dir, inbox) = open();
    capture(&inbox, "A", "run1", "log", 0, 0);
    assert_eq!(
        inbox
            .dispatch_claim("A", "run1", at(59))
            .unwrap()
            .next_due_at,
        Some(at(60))
    );
    assert!(inbox
        .dispatch_claim("A", "run1", at(-600))
        .unwrap()
        .claim
        .is_none());
    std::fs::write(dir.path().join("config.toml"), "muted = [\"log\"]\n").unwrap();
    let muted = Inbox::open(dir.path()).unwrap();
    assert_eq!(
        muted.dispatch_claim("A", "run1", at(60)).unwrap().pending,
        0
    );
    drop(muted);
    std::fs::remove_file(dir.path().join("config.toml")).unwrap();
    assert_eq!(
        accept(&Inbox::open(dir.path()).unwrap(), 61)
            .batch
            .occurrences
            .len(),
        1
    );
}

#[test]
fn end_session_invalidates_claim_and_epoch_and_scope_capacity_is_explicit() {
    let (dir, inbox) = open();
    capture(&inbox, "A", "run1", "log", 0, 0);
    let claim = inbox
        .dispatch_claim("A", "run1", at(60))
        .unwrap()
        .claim
        .unwrap();
    inbox.end_session("A", at(61)).unwrap();
    assert!(inbox
        .dispatch_accept("A", "run1", &claim.batch.id, &claim.token, at(62))
        .is_err());
    assert!(inbox.dispatch_claim("A", "run1", at(62)).is_err());
    assert!(inbox
        .snapshot(at(62))
        .unwrap()
        .signals
        .values()
        .all(|s| !s.is_pending() && s.cancelled_at.is_some()));
    inbox.dispatch_start("C", "run1", None, at(62)).unwrap();
    capture(&inbox, "C", "run1", "c-log", 1, 62);
    std::fs::write(dir.path().join("config.toml"), "max_signals = 1\n").unwrap();
    let bounded = Inbox::open(dir.path()).unwrap();
    let refused = bounded
        .dispatch_start("B", "run1", None, at(63))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("raise the cap"));
    let gates = bounded.snapshot(at(63)).unwrap().dispatch;
    assert_eq!(gates.keys().collect::<Vec<_>>(), ["A", "C"]);
}

const HOUR: i64 = 60 * 60;
const WEEK: i64 = 7 * 24 * HOUR;

fn with_max_signals(dir: &tempfile::TempDir, max: usize) -> Inbox {
    std::fs::write(
        dir.path().join("config.toml"),
        format!("max_signals = {max}\n"),
    )
    .unwrap();
    Inbox::open(dir.path()).unwrap()
}

#[test]
fn a_gate_idle_past_retention_is_forgotten_and_restarts_cleanly() {
    let (_dir, inbox) = open();
    inbox.dispatch_start("busy", "run1", None, at(0)).unwrap();
    capture(&inbox, "busy", "run1", "busy-log", 0, WEEK - 1);
    let early = inbox.snapshot(at(WEEK - 1)).unwrap().dispatch;
    assert_eq!(early.len(), 2);
    let gates = inbox.snapshot(at(WEEK)).unwrap().dispatch;
    assert_eq!(gates.keys().collect::<Vec<_>>(), ["busy"]);
    inbox.dispatch_start("A", "run1", None, at(WEEK)).unwrap();
    capture(&inbox, "A", "run1", "log", 1, WEEK);
    assert!(inbox
        .dispatch_claim("A", "run1", at(WEEK + 60))
        .unwrap()
        .claim
        .is_some());
}

#[test]
fn a_dead_session_with_unaccepted_captures_stops_pinning_its_record() {
    let (dir, inbox) = open();
    capture(&inbox, "A", "run1", "log", 0, 0);
    inbox.dispatch_claim("A", "run1", at(60)).unwrap();
    let bounded = with_max_signals(&dir, 1);
    assert!(bounded.dispatch_start("B", "run1", None, at(WEEK)).is_err());

    bounded
        .dispatch_start("B", "run1", None, at(WEEK + 60))
        .unwrap();
    let saved = bounded.snapshot(at(WEEK + 60)).unwrap();
    assert_eq!(saved.dispatch.keys().collect::<Vec<_>>(), ["B"]);
    let output = saved.signals.values().next().unwrap();
    assert!(!output.is_pending() && output.cancelled_at.is_some());
}

#[test]
fn a_forgotten_live_session_captures_again_and_keeps_monitoring() {
    let (_dir, inbox) = open();
    assert!(matches!(
        inbox.dispatch_claim("A", "run1", at(WEEK)),
        Err(harn_signals::SignalsError::DispatchForgotten)
    ));

    let input = SignalInput {
        key: Some("log".into()),
        event: TriageEvent::manual(
            ManualEvent {
                provider: "fixture".into(),
                source_kind: "log".into(),
                source_url: "file:///fixture/log".into(),
                summary: "line after a quiet week".into(),
                why_it_matters: "New output".into(),
                proposed_action: "Inspect output".into(),
                urgency: String::new(),
            },
            "log",
        )
        .unwrap(),
        severity: Severity::Warning,
        routing: RoutingHints::default(),
        provenance: Provenance::local("fixture"),
        expires_at: None,
    };
    inbox
        .capture_execution_or_restart("A", "run1", "later", Some(30), input, at(WEEK))
        .unwrap();
    assert_eq!(
        inbox
            .dispatch_claim_or_restart("A", "run1", Some(30), at(WEEK + 29))
            .unwrap()
            .next_due_at,
        Some(at(WEEK + 30))
    );
    let claim = inbox
        .dispatch_claim_or_restart("A", "run1", Some(30), at(WEEK + 30))
        .unwrap()
        .claim
        .unwrap();
    assert_eq!(claim.batch.occurrences.len(), 1);

    // A claim alone restarts too, so a host with nothing captured stays on the books.
    let poll = inbox
        .dispatch_claim_or_restart("A", "run1", Some(30), at(3 * WEEK))
        .unwrap();
    assert_eq!(poll.pending, 0);
    assert_eq!(inbox.snapshot(at(3 * WEEK)).unwrap().dispatch.len(), 1);
}

#[test]
fn a_full_inbox_forgets_only_quiet_gates_and_a_refusal_forgets_nothing() {
    let (dir, inbox) = open();
    for session in ["B", "C", "D"] {
        inbox
            .dispatch_start(session, "run1", None, at(3 * HOUR))
            .unwrap();
    }
    let bounded = with_max_signals(&dir, 2);

    // A is quiet, but forgetting it alone would not make room.
    assert!(bounded
        .dispatch_start("E", "run1", None, at(3 * HOUR))
        .is_err());
    let gates = bounded.snapshot(at(3 * HOUR)).unwrap().dispatch;
    assert_eq!(gates.keys().collect::<Vec<_>>(), ["A", "B", "C", "D"]);

    bounded
        .dispatch_start("E", "run1", None, at(6 * HOUR))
        .unwrap();
    let gates = bounded.snapshot(at(6 * HOUR)).unwrap().dispatch;
    assert_eq!(gates.keys().collect::<Vec<_>>(), ["D", "E"]);
    for n in 0..10 {
        bounded
            .dispatch_start(&format!("churn{n}"), "run1", None, at((9 + 3 * n) * HOUR))
            .unwrap();
    }
    assert_eq!(bounded.snapshot(at(40 * HOUR)).unwrap().dispatch.len(), 2);
}

#[test]
fn a_full_inbox_keeps_a_closed_run_and_an_open_pacing_window() {
    const DAY: i64 = 24 * HOUR;
    let (dir, inbox) = open();
    inbox.dispatch_cancel("A", "run1", None, at(1)).unwrap();
    let bounded = with_max_signals(&dir, 1);
    assert!(bounded.dispatch_start("B", "run1", None, at(2)).is_err());
    assert!(bounded.dispatch_start("A", "run1", None, at(2)).is_err());

    let dir = tempfile::tempdir().unwrap();
    let paced = Inbox::open(dir.path()).unwrap();
    paced
        .dispatch_start("P", "run1", Some(86400), at(0))
        .unwrap();
    capture(&paced, "P", "run1", "log", 0, 0);
    let claim = paced
        .dispatch_claim("P", "run1", at(DAY))
        .unwrap()
        .claim
        .unwrap();
    paced
        .dispatch_accept("P", "run1", &claim.batch.id, &claim.token, at(DAY))
        .unwrap();
    let bounded = with_max_signals(&dir, 1);
    assert!(bounded
        .dispatch_start("Q", "run1", None, at(DAY + 3 * HOUR))
        .is_err());
    bounded
        .dispatch_start("Q", "run1", None, at(2 * DAY))
        .unwrap();
}

#[test]
fn cancelling_a_monitor_at_the_cap_makes_room_from_quiet_gates() {
    let (dir, inbox) = open();
    inbox
        .dispatch_start("B", "run1", None, at(3 * HOUR))
        .unwrap();
    let bounded = with_max_signals(&dir, 2);
    bounded
        .dispatch_cancel("B", "run1", Some("noisy"), at(3 * HOUR))
        .unwrap();
    let gates = bounded.snapshot(at(3 * HOUR)).unwrap().dispatch;
    assert_eq!(gates.keys().collect::<Vec<_>>(), ["B"]);
}

#[test]
fn replayed_start_cannot_reactivate_a_retired_execution() {
    let (_dir, inbox) = open();
    inbox.dispatch_start("A", "run2", None, at(1)).unwrap();
    assert!(inbox.dispatch_start("A", "run1", None, at(2)).is_err());
    capture(&inbox, "A", "run2", "log", 0, 2);
    assert_eq!(
        inbox
            .dispatch_claim("A", "run2", at(62))
            .unwrap()
            .claim
            .unwrap()
            .batch
            .occurrences
            .len(),
        1
    );
}
