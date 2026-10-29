use std::path::PathBuf;

use harn_signals::{
    Activity, CheckInScope, HarnessKind, Inbox, ManualEvent, Provenance, RoutingHints,
    SessionAnnouncement, Severity, SignalInput, SignatureStatus, Transition, TriageEvent,
};
use time::{Duration, OffsetDateTime};

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
}

fn input(summary: &str) -> SignalInput {
    SignalInput {
        key: Some("build-log".into()),
        event: TriageEvent::manual(
            ManualEvent {
                provider: "fixture".into(),
                source_kind: "log".into(),
                source_url: "file:///fixture/build.log".into(),
                summary: summary.into(),
                why_it_matters: "New output".into(),
                proposed_action: "Inspect the output".into(),
                urgency: String::new(),
            },
            "build-log",
        )
        .unwrap(),
        severity: Severity::Warning,
        routing: RoutingHints::default(),
        provenance: Provenance::local("fixture"),
        expires_at: None,
    }
}

fn session(id: &str) -> SessionAnnouncement {
    SessionAnnouncement {
        id: id.into(),
        harness: HarnessKind::Other("burin".into()),
        cwd: PathBuf::from("/fixture"),
        repo: Some("example/project".into()),
        title: None,
        role: Some("control".into()),
        activity: Activity::Busy,
    }
}

#[test]
fn captured_output_survives_repetition_reopen_and_unrelated_session() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    // The condition API is the negative control: it retains only the first state.
    for text in ["line one", "line two", "line two"] {
        inbox.fire(input(text), now()).unwrap();
    }
    let baseline = inbox.snapshot(now()).unwrap();
    assert_eq!(baseline.signals.len(), 1);
    assert_eq!(baseline.signals["build-log"].event.summary, "line one");

    for (offset, text) in [(0, "line one"), (9, "line two"), (18, "line two")] {
        assert!(matches!(
            inbox
                .capture("owner", &offset.to_string(), input(text), now())
                .unwrap(),
            Transition::Changed { .. }
        ));
    }
    drop(inbox);
    let inbox = Inbox::open(dir.path()).unwrap();
    assert!(matches!(
        inbox
            .capture(
                "owner",
                "9",
                input("changed retry"),
                now() + Duration::seconds(20)
            )
            .unwrap(),
        Transition::Unchanged { .. }
    ));
    // The unrelated fallback control can receive the ambient condition, not captures.
    let other = inbox
        .check_in(
            session("other"),
            CheckInScope::All,
            None,
            now() + Duration::seconds(21),
        )
        .unwrap();
    assert_eq!(other.reminders.len(), 1);
    assert_eq!(other.reminders[0].key, "build-log");
    let owned = inbox
        .check_in(
            session("owner"),
            CheckInScope::All,
            None,
            now() + Duration::seconds(22),
        )
        .unwrap();
    assert_eq!(
        owned
            .reminders
            .iter()
            .map(|r| r.event.summary.as_str())
            .collect::<Vec<_>>(),
        ["line one", "line two", "line two"]
    );
    let tokens = owned
        .reminders
        .iter()
        .map(|r| r.lease_token.clone())
        .collect::<Vec<_>>();
    assert!(inbox
        .ack_leases("other", &tokens, now() + Duration::seconds(23))
        .unwrap()
        .is_empty());
    assert_eq!(
        inbox
            .ack_leases("owner", &tokens, now() + Duration::seconds(23))
            .unwrap()
            .len(),
        3
    );
    assert!(matches!(
        inbox
            .capture(
                "owner",
                "9",
                input("late retry"),
                now() + Duration::seconds(24)
            )
            .unwrap(),
        Transition::Unchanged { .. }
    ));
    assert!(inbox
        .check_in(
            session("owner"),
            CheckInScope::All,
            None,
            now() + Duration::seconds(25)
        )
        .unwrap()
        .reminders
        .is_empty());
}

#[test]
fn capture_refuses_loss_mutation_and_weaker_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    let summary = "x".repeat(harn_signals::MAX_SUMMARY_CHARS + 1);
    assert!(inbox.capture("owner", "0", input(&summary), now()).is_err());
    assert!(inbox.snapshot(now()).unwrap().signals.is_empty());
    assert!(inbox.capture("", "0", input("line"), now()).is_err());
    assert!(inbox.capture("owner", "\n", input("line"), now()).is_err());
    inbox.capture("owner", "1", input("line"), now()).unwrap();
    let mut weak = input("replacement");
    weak.provenance.signature = SignatureStatus::Unverified;
    assert!(inbox.capture("owner", "1", weak, now()).is_err());
    let signal = inbox
        .snapshot(now())
        .unwrap()
        .signals
        .into_values()
        .next()
        .unwrap();
    assert!(inbox
        .resolve(&signal.key, None, SignatureStatus::Local, now())
        .is_err());
    let mut overwrite = input("replacement");
    overwrite.key = Some(signal.key);
    assert!(inbox.fire(overwrite, now()).is_err());
    assert_eq!(
        inbox
            .snapshot(now())
            .unwrap()
            .signals
            .into_values()
            .next()
            .unwrap()
            .event
            .summary,
        "line"
    );
}

#[test]
fn same_timestamp_capture_burst_preserves_ingestion_order_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    for sequence in 0..32 {
        inbox
            .capture(
                "owner",
                &sequence.to_string(),
                input(&format!("line {sequence}")),
                now(),
            )
            .unwrap();
    }
    drop(inbox);
    let reopened = Inbox::open(dir.path()).unwrap();
    let batch = reopened
        .check_in(session("owner"), CheckInScope::All, None, now())
        .unwrap();
    assert_eq!(
        batch
            .reminders
            .iter()
            .map(|r| r.event.summary.clone())
            .collect::<Vec<_>>(),
        (0..32).map(|i| format!("line {i}")).collect::<Vec<_>>()
    );
}

#[test]
fn captured_monitor_mute_holds_until_policy_reopens() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "muted = [\"build-log\"]\n").unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    inbox.capture("owner", "1", input("one"), now()).unwrap();
    assert!(inbox
        .check_in(session("owner"), CheckInScope::All, None, now())
        .unwrap()
        .reminders
        .is_empty());
    assert_eq!(inbox.snapshot(now()).unwrap().signals.len(), 1);
    drop(inbox);
    std::fs::write(dir.path().join("config.toml"), "muted = []\n").unwrap();
    let reopened = Inbox::open(dir.path()).unwrap();
    let delivered = reopened
        .check_in(session("owner"), CheckInScope::All, None, now())
        .unwrap();
    assert_eq!(delivered.reminders.len(), 1);
    assert_eq!(delivered.reminders[0].event.summary, "one");
}

#[test]
fn previous_schema_is_refused_without_rewriting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.json");
    let original = br#"{"schema":"harn.signals.inbox.v1","signals":{},"sessions":{}}"#;
    std::fs::write(&path, original).unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    let error = inbox.snapshot(now()).unwrap_err().to_string();
    assert!(error.contains("harn.signals.inbox.v3"), "{error}");
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
fn captured_occurrences_remain_with_ended_session_and_refuse_full_inbox() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "max_signals = 2\n").unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    inbox.capture("owner", "1", input("one"), now()).unwrap();
    inbox.capture("owner", "2", input("two"), now()).unwrap();
    assert!(inbox.capture("owner", "3", input("three"), now()).is_err());
    let owned = inbox
        .check_in(session("owner"), CheckInScope::All, None, now())
        .unwrap();
    assert_eq!(owned.reminders.len(), 2);
    inbox.end_session("owner", now()).unwrap();
    let later = now() + Duration::minutes(11);
    assert!(inbox
        .check_in(session("other"), CheckInScope::All, None, later)
        .unwrap()
        .reminders
        .is_empty());
    let recovered = inbox
        .check_in(session("owner"), CheckInScope::All, None, later)
        .unwrap();
    assert_eq!(recovered.reminders.len(), 2);
    assert!(recovered.reminders.iter().all(|r| r.attempt == 2));
    let tokens = recovered
        .reminders
        .iter()
        .map(|r| r.lease_token.clone())
        .collect::<Vec<_>>();
    assert_eq!(inbox.ack_leases("owner", &tokens, later).unwrap().len(), 2);
    inbox.capture("owner", "3", input("three"), later).unwrap();
    let pending = inbox
        .check_in(session("owner"), CheckInScope::All, None, later)
        .unwrap();
    assert_eq!(pending.reminders.len(), 1);
    assert_eq!(pending.reminders[0].event.summary, "three");
}
