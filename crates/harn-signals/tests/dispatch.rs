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
    std::fs::write(dir.path().join("config.toml"), "max_signals = 1\n").unwrap();
    let bounded = Inbox::open(dir.path()).unwrap();
    assert!(bounded.dispatch_start("B", "run1", None, at(63)).is_err());
    assert_eq!(bounded.snapshot(at(63)).unwrap().dispatch.len(), 1);
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
